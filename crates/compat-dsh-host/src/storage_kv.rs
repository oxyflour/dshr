//! `storage-kv/v1` 文件型 adapter（`docs/m3-l2-services.md` §3.3，spec §13）。
//!
//! 跨进程切面是官方 `@deepseek-ai/dsh-storage` 的 **kv facet**：Rust 只实现 unit 的
//! 7 个操作（open/loadAll/putRecord/deleteRecord/deleteRecords/setGlobal/close），
//! schema 校验、layout 语义、变更事件都留在官方 JS 代码里。
//!
//! 介质（v2 选择，见 spec §14 Q7）：`<root>/<plugin_id>/<unit.name>.json`，文件内容为
//! `{version, compatible_versions, layout, tables, global}`。写操作在 per-unit mutex
//! 内 load→mutate→temp+rename，保证"不丢已写记录"；`single` 与 `per-record` 两种
//! layout 共用同一个 JSON 介质（v2 的显式限制：介质形态不随 layout 分叉）。
//!
//! 版本戳：写操作原子地打上 descriptor 的当前 `version`；读接受当前版本或
//! descriptor 声明的 `compatible_versions`，否则 `E_INTERNAL` +
//! `detail.storage_code = "version-mismatch"`（spec §9.3）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use dshr_compat_dsh_protocol::{HostServiceKind, HostServiceMethod, HostServiceTable};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::service::{HostService, ServiceCallContext, ServiceError};

/// 服务键（spec §13.2 形态 `<name>/v<major>`）。
pub const STORAGE_KV_V1: &str = "storage-kv/v1";
/// 单个 record key 的防御性长度上限。
const MAX_KEY_LENGTH: usize = 256;
/// `loadAll` 结果内联上限（spec §13.8，768 KiB）。
const MAX_RESULT_BYTES: u64 = 786_432;

/// unit 布局（官方 `dsh-storage` 的 `layout`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StorageLayout {
    /// 整个 unit 一个文件（本 adapter 的唯一介质形态）。
    #[default]
    Single,
    /// 语义上每表每记录一个 key；v2 仍存于同一 JSON 文件（见模块注释）。
    PerRecord,
}

impl StorageLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::PerRecord => "per-record",
        }
    }
}

/// `unit.open` 的 descriptor（官方 `KvFacet.open` 的 JSON 投影）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageUnitDescriptor {
    pub name: String,
    pub version: u32,
    #[serde(default)]
    pub compatible_versions: Vec<u32>,
    #[serde(default)]
    pub layout: StorageLayout,
    #[serde(default)]
    pub tables: Vec<String>,
    /// 官方 descriptor 的 `hasGlobal`：`false` 时 `unit.setGlobal` 报
    /// `facet-unsupported`（spec §9.3 的 storage 词表）。
    #[serde(default)]
    pub has_global: bool,
}

impl StorageUnitDescriptor {
    /// 排序/去重兼容版本与表名，让"同 descriptor 重开幂等"不受声明顺序影响。
    fn canonicalized(self) -> Self {
        let mut compatible_versions = self.compatible_versions;
        compatible_versions.sort_unstable();
        compatible_versions.dedup();
        let mut tables = self.tables;
        tables.sort();
        tables.dedup();
        Self {
            compatible_versions,
            tables,
            ..self
        }
    }

    fn validate(&self) -> Result<(), String> {
        validate_unit_name(&self.name, "unit.name")?;
        for (index, table) in self.tables.iter().enumerate() {
            validate_unit_name(table, &format!("unit.tables[{index}]"))?;
        }
        for (index, compatible) in self.compatible_versions.iter().enumerate() {
            if *compatible >= self.version {
                return Err(format!(
                    "unit.compatible_versions[{index}] = {compatible} must be < version {}",
                    self.version
                ));
            }
        }
        Ok(())
    }

    /// 读兼容（官方契约）：当前版本或声明过的兼容版本。
    fn accepts_stored(&self, stored_version: u32) -> bool {
        stored_version == self.version || self.compatible_versions.contains(&stored_version)
    }
}

/// 介质内容（spec §13.8 / 任务书要求的文件形状）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UnitFile {
    version: u32,
    #[serde(default)]
    compatible_versions: Vec<u32>,
    #[serde(default)]
    layout: StorageLayout,
    #[serde(default)]
    tables: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default)]
    global: Option<Value>,
}

impl UnitFile {
    fn from_descriptor(descriptor: &StorageUnitDescriptor) -> Self {
        let mut tables = BTreeMap::new();
        for table in &descriptor.tables {
            tables.insert(table.clone(), BTreeMap::new());
        }
        Self {
            version: descriptor.version,
            compatible_versions: descriptor.compatible_versions.clone(),
            layout: descriptor.layout,
            tables,
            global: None,
        }
    }

    fn stamp(&mut self, descriptor: &StorageUnitDescriptor) {
        self.version = descriptor.version;
        self.compatible_versions = descriptor.compatible_versions.clone();
        self.layout = descriptor.layout;
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    unit: StorageUnitDescriptor,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnitArgs {
    unit_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PutRecordArgs {
    unit_id: String,
    table: String,
    key: String,
    value: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteRecordArgs {
    unit_id: String,
    table: String,
    key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteRecordsArgs {
    unit_id: String,
    table: String,
    keys: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetGlobalArgs {
    unit_id: String,
    value: Value,
}

#[derive(Debug, Clone)]
struct UnitSnapshot {
    descriptor: StorageUnitDescriptor,
    path: PathBuf,
}

struct UnitHandle {
    plugin_id: String,
    descriptor: StorageUnitDescriptor,
    path: PathBuf,
    open: bool,
}

/// `storage-kv/v1` 的宿主实现。
///
/// `root` 由宿主配置（profile 目录），测试用 `std::env::temp_dir()` 的唯一目录。
/// 进程内按介质绝对路径串行化（`UNIT_LOCKS`）：同一个 unit 的写不会互相覆盖，
/// 即使存在多个 service 实例（重启/多线程测试）。
pub struct StorageKvService {
    root: PathBuf,
    table: HostServiceTable,
    units: BTreeMap<String, UnitHandle>,
    by_scope: BTreeMap<(String, String), String>,
    next_unit: u64,
}

impl StorageKvService {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ServiceError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|error| {
            storage_error("unknown-state", format!("create storage root: {error}"))
        })?;
        Ok(Self {
            root,
            table: storage_kv_table(),
            units: BTreeMap::new(),
            by_scope: BTreeMap::new(),
            next_unit: 0,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn open_unit(&mut self, args: &Value, ctx: &ServiceCallContext) -> Result<Value, ServiceError> {
        let args: OpenArgs = parse_args(args)?;
        let descriptor = args.unit.canonicalized();
        descriptor
            .validate()
            .map_err(|reason| ServiceError::args("/args/unit", reason))?;
        validate_plugin_id(&ctx.plugin_id)?;

        let scope = (ctx.plugin_id.clone(), descriptor.name.clone());
        if let Some(unit_id) = self.by_scope.get(&scope).cloned() {
            let handle = self
                .units
                .get_mut(&unit_id)
                .ok_or_else(|| storage_error("unknown-state", "unit handle disappeared"))?;
            if !handle.open {
                materialize_unit(&descriptor, &handle.path)?;
                handle.descriptor = descriptor;
                handle.open = true;
                return Ok(json!({ "unit_id": unit_id }));
            }
            if handle.descriptor == descriptor {
                return Ok(json!({ "unit_id": unit_id }));
            }
            return Err(descriptor_conflict(&handle.descriptor, &descriptor));
        }

        let path = unit_path(&self.root, &ctx.plugin_id, &descriptor.name);
        materialize_unit(&descriptor, &path)?;
        self.next_unit += 1;
        let unit_id = format!("unit-{:06}", self.next_unit);
        self.by_scope.insert(scope, unit_id.clone());
        self.units.insert(
            unit_id.clone(),
            UnitHandle {
                plugin_id: ctx.plugin_id.clone(),
                descriptor,
                path,
                open: true,
            },
        );
        Ok(json!({ "unit_id": unit_id }))
    }

    fn load_all(&mut self, args: &Value, ctx: &ServiceCallContext) -> Result<Value, ServiceError> {
        let args: UnitArgs = parse_args(args)?;
        let snapshot = self.resolve(&args.unit_id, &ctx.plugin_id)?;
        let file = read_unit_file(&snapshot.path)?
            .ok_or_else(|| storage_error("unknown-state", "unit medium is missing"))?;
        ensure_compatible(&snapshot.descriptor, &file)?;
        let mut result = json!({ "tables": file.tables });
        if let Some(global) = file.global {
            result["global"] = global;
        }
        let bytes = serde_json::to_vec(&result)
            .map_err(|error| storage_error("unknown-state", format!("serialize loadAll: {error}")))?
            .len() as u64;
        if bytes > MAX_RESULT_BYTES {
            return Err(ServiceError::new(
                "E_TOO_LARGE",
                format!(
                    "unit.loadAll serialized size {bytes} exceeds {MAX_RESULT_BYTES} bytes; v2 has no cursor"
                ),
            ));
        }
        Ok(result)
    }

    fn put_record(
        &mut self,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let args: PutRecordArgs = parse_args(args)?;
        validate_record_table(&args.table)?;
        validate_record_key(&args.key)?;
        let snapshot = self.resolve(&args.unit_id, &ctx.plugin_id)?;
        with_unit_file(&snapshot.descriptor, &snapshot.path, |file| {
            file.tables
                .entry(args.table.clone())
                .or_default()
                .insert(args.key.clone(), args.value.clone());
            file.stamp(&snapshot.descriptor);
            Ok((true, ()))
        })?;
        Ok(json!({}))
    }

    fn delete_record(
        &mut self,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let args: DeleteRecordArgs = parse_args(args)?;
        validate_record_table(&args.table)?;
        validate_record_key(&args.key)?;
        let snapshot = self.resolve(&args.unit_id, &ctx.plugin_id)?;
        let deleted = with_unit_file(&snapshot.descriptor, &snapshot.path, |file| {
            let Some(table) = file.tables.get_mut(&args.table) else {
                return Ok((false, 0));
            };
            if table.remove(&args.key).is_some() {
                file.stamp(&snapshot.descriptor);
                Ok((true, 1))
            } else {
                // 缺 key 的删除不重打版本戳（官方契约）。
                Ok((false, 0))
            }
        })?;
        Ok(json!({ "deleted": deleted }))
    }

    fn delete_records(
        &mut self,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let args: DeleteRecordsArgs = parse_args(args)?;
        validate_record_table(&args.table)?;
        for key in &args.keys {
            validate_record_key(key)?;
        }
        let snapshot = self.resolve(&args.unit_id, &ctx.plugin_id)?;
        let deleted = with_unit_file(&snapshot.descriptor, &snapshot.path, |file| {
            let Some(table) = file.tables.get_mut(&args.table) else {
                return Ok((false, 0));
            };
            let before = table.len();
            for key in &args.keys {
                table.remove(key);
            }
            let deleted = (before - table.len()) as u64;
            if deleted > 0 {
                file.stamp(&snapshot.descriptor);
                Ok((true, deleted))
            } else {
                Ok((false, 0))
            }
        })?;
        Ok(json!({ "deleted": deleted }))
    }

    fn set_global(
        &mut self,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let args: SetGlobalArgs = parse_args(args)?;
        let snapshot = self.resolve(&args.unit_id, &ctx.plugin_id)?;
        if !snapshot.descriptor.has_global {
            return Err(storage_error(
                "facet-unsupported",
                "unit descriptor declares has_global:false; setGlobal is not available",
            ));
        }
        if args.value.is_null() {
            return Err(ServiceError::args(
                "/args/value",
                "null is the medium's \"never written\" sentinel and cannot be stored as global",
            ));
        }
        with_unit_file(&snapshot.descriptor, &snapshot.path, |file| {
            file.global = Some(args.value.clone());
            file.stamp(&snapshot.descriptor);
            Ok((true, ()))
        })?;
        Ok(json!({}))
    }

    fn close_unit(
        &mut self,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let args: UnitArgs = parse_args(args)?;
        let Some(handle) = self.units.get(&args.unit_id) else {
            // close 幂等：未知 unit_id 视为已关闭。
            return Ok(json!({}));
        };
        if handle.plugin_id != ctx.plugin_id {
            return Err(scope_denied(&args.unit_id));
        }
        if let Some(handle) = self.units.get_mut(&args.unit_id) {
            handle.open = false;
        }
        Ok(json!({}))
    }

    fn resolve(&self, unit_id: &str, plugin_id: &str) -> Result<UnitSnapshot, ServiceError> {
        let Some(handle) = self.units.get(unit_id) else {
            return Err(unavailable(
                "unknown-state",
                format!("unknown unit `{unit_id}`"),
            ));
        };
        if handle.plugin_id != plugin_id {
            return Err(scope_denied(unit_id));
        }
        if !handle.open {
            return Err(unavailable("closed", format!("unit `{unit_id}` is closed")));
        }
        Ok(UnitSnapshot {
            descriptor: handle.descriptor.clone(),
            path: handle.path.clone(),
        })
    }
}

impl HostService for StorageKvService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        method: &str,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        validate_plugin_id(&ctx.plugin_id)?;
        match method {
            "unit.open" => self.open_unit(args, ctx),
            "unit.loadAll" => self.load_all(args, ctx),
            "unit.putRecord" => self.put_record(args, ctx),
            "unit.deleteRecord" => self.delete_record(args, ctx),
            "unit.deleteRecords" => self.delete_records(args, ctx),
            "unit.setGlobal" => self.set_global(args, ctx),
            "unit.close" => self.close_unit(args, ctx),
            other => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                format!("unknown storage-kv method `{other}`"),
            )),
        }
    }
}

fn parse_args<T: for<'de> Deserialize<'de>>(args: &Value) -> Result<T, ServiceError> {
    serde_json::from_value(args.clone())
        .map_err(|error| ServiceError::args("/args", format!("invalid args: {error}")))
}

fn validate_plugin_id(plugin_id: &str) -> Result<(), ServiceError> {
    let bytes = plugin_id.as_bytes();
    let valid = bytes.len() >= 2
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(ServiceError::new(
            "E_SERVICE_DENIED",
            format!("invalid plugin scope `{plugin_id}`"),
        ))
    }
}

fn validate_unit_name(name: &str, field: &str) -> Result<(), String> {
    let bytes = name.as_bytes();
    let valid = !bytes.is_empty()
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "{field}: `{name}` must match `^[a-z][a-z0-9_]*$` (UNIT_NAME_RE)"
        ))
    }
}

fn validate_record_table(table: &str) -> Result<(), ServiceError> {
    validate_unit_name(table, "/args/table")
        .map_err(|reason| ServiceError::args("/args/table", reason))
}

fn validate_record_key(key: &str) -> Result<(), ServiceError> {
    if key.is_empty() || key.len() > MAX_KEY_LENGTH {
        return Err(ServiceError::args(
            "/args/key",
            format!("record key must be 1..={MAX_KEY_LENGTH} bytes"),
        ));
    }
    Ok(())
}

fn unit_path(root: &Path, plugin_id: &str, unit_name: &str) -> PathBuf {
    root.join(plugin_id).join(format!("{unit_name}.json"))
}

fn storage_error(storage_code: &str, message: impl Into<String>) -> ServiceError {
    ServiceError::new("E_INTERNAL", message)
        .with_detail(json!({ "storage_code": storage_code }))
        .with_retryable(false)
}

/// unit 未打开/已关闭/未知：m3 服务表要求 `E_SERVICE_UNAVAILABLE`（spec §9.3 允许的
/// 信封码之一），实现自有的 `storage_code` 仍放 `payload.detail`。
fn unavailable(storage_code: &str, message: impl Into<String>) -> ServiceError {
    ServiceError::new("E_SERVICE_UNAVAILABLE", message)
        .with_detail(json!({ "storage_code": storage_code }))
        .with_retryable(false)
}

fn scope_denied(unit_id: &str) -> ServiceError {
    ServiceError::new(
        "E_SERVICE_DENIED",
        format!("unit `{unit_id}` belongs to another plugin scope"),
    )
    .with_detail(json!({ "storage_code": "scope-denied" }))
}

fn version_mismatch(descriptor: &StorageUnitDescriptor, stored_version: u32) -> ServiceError {
    storage_error(
        "version-mismatch",
        format!(
            "unit `{}` stores version {stored_version} but descriptor version {} does not accept it (compatible_versions: {:?})",
            descriptor.name, descriptor.version, descriptor.compatible_versions
        ),
    )
}

fn descriptor_conflict(
    existing: &StorageUnitDescriptor,
    requested: &StorageUnitDescriptor,
) -> ServiceError {
    if !requested.accepts_stored(existing.version) && !existing.accepts_stored(requested.version) {
        return version_mismatch(requested, existing.version);
    }
    storage_error(
        "already-open",
        format!(
            "unit `{}` is already open with a different descriptor (layout/tables/has_global)",
            existing.name
        ),
    )
}

fn ensure_compatible(
    descriptor: &StorageUnitDescriptor,
    file: &UnitFile,
) -> Result<(), ServiceError> {
    if descriptor.accepts_stored(file.version) {
        Ok(())
    } else {
        Err(version_mismatch(descriptor, file.version))
    }
}

/// 读介质；不存在 → `Ok(None)`；损坏 → `E_INTERNAL` + `unknown-state`。
fn read_unit_file(path: &Path) -> Result<Option<UnitFile>, ServiceError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(storage_error(
                "unknown-state",
                format!("read unit medium `{}`: {error}", path.display()),
            ))
        }
    };
    serde_json::from_slice::<UnitFile>(&bytes)
        .map(Some)
        .map_err(|error| {
            storage_error(
                "unknown-state",
                format!("malformed unit medium `{}`: {error}", path.display()),
            )
        })
}

/// 原子写：临时文件 + rename（同目录），不丢已写记录。
fn write_unit_file(path: &Path, file: &UnitFile) -> Result<(), ServiceError> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
    let parent = path.parent().ok_or_else(|| {
        storage_error(
            "unknown-state",
            format!("unit medium `{}` has no parent", path.display()),
        )
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        storage_error(
            "unknown-state",
            format!("create unit directory `{}`: {error}", parent.display()),
        )
    })?;
    let bytes = serde_json::to_vec_pretty(file).map_err(|error| {
        storage_error("unknown-state", format!("serialize unit medium: {error}"))
    })?;
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unit");
    let temp = parent.join(format!(
        ".{stem}.tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&temp, &bytes).map_err(|error| {
        storage_error(
            "unknown-state",
            format!("write unit medium `{}`: {error}", temp.display()),
        )
    })?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(storage_error(
                "unknown-state",
                format!("replace unit medium `{}`: {error}", path.display()),
            ))
        }
    }
}

/// 进程内 per-unit 锁：介质绝对路径 → mutex（`Arc` 让多个 service 实例共享）。
fn unit_path_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// 打开/重开时把介质对齐到 descriptor：不存在则创建；版本不兼容 → version-mismatch；
/// 补齐 descriptor 声明但介质里还没有的表（不重打版本戳）。
fn materialize_unit(descriptor: &StorageUnitDescriptor, path: &Path) -> Result<(), ServiceError> {
    let lock = unit_path_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match read_unit_file(path)? {
        Some(mut file) => {
            ensure_compatible(descriptor, &file)?;
            let mut changed = false;
            for table in &descriptor.tables {
                if !file.tables.contains_key(table) {
                    file.tables.insert(table.clone(), BTreeMap::new());
                    changed = true;
                }
            }
            if changed {
                write_unit_file(path, &file)?;
            }
            Ok(())
        }
        None => write_unit_file(path, &UnitFile::from_descriptor(descriptor)),
    }
}

/// 在 per-unit 锁内 load→mutate→（有变化才）temp+rename。
fn with_unit_file<T>(
    descriptor: &StorageUnitDescriptor,
    path: &Path,
    mutate: impl FnOnce(&mut UnitFile) -> Result<(bool, T), ServiceError>,
) -> Result<T, ServiceError> {
    let lock = unit_path_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut file = read_unit_file(path)?
        .ok_or_else(|| storage_error("unknown-state", "unit medium is missing"))?;
    ensure_compatible(descriptor, &file)?;
    let (changed, output) = mutate(&mut file)?;
    if changed {
        write_unit_file(path, &file)?;
    }
    Ok(output)
}

fn storage_kv_table() -> HostServiceTable {
    let unit_descriptor_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "version"],
        "properties": {
            "name": { "type": "string" },
            "version": { "type": "integer", "minimum": 0 },
            "compatible_versions": {
                "type": "array",
                "items": { "type": "integer", "minimum": 0 }
            },
            "layout": { "type": "string", "enum": ["single", "per-record"] },
            "tables": { "type": "array", "items": { "type": "string" } },
            "has_global": { "type": "boolean" }
        }
    });
    let empty_result = object_schema(json!({}), json!([]));
    let methods = vec![
        method(
            "unit.open",
            HostServiceKind::Write,
            object_schema(json!({ "unit": unit_descriptor_schema }), json!(["unit"])),
            object_schema(json!({ "unit_id": unit_id_schema() }), json!(["unit_id"])),
            15_000,
        ),
        method(
            "unit.loadAll",
            HostServiceKind::Read,
            object_schema(json!({ "unit_id": unit_id_schema() }), json!(["unit_id"])),
            object_schema(
                json!({
                    "tables": { "type": "object" },
                    "global": {}
                }),
                json!(["tables"]),
            ),
            30_000,
        ),
        method(
            "unit.putRecord",
            HostServiceKind::Write,
            object_schema(
                json!({
                    "unit_id": unit_id_schema(),
                    "table": { "type": "string" },
                    "key": { "type": "string" },
                    "value": {}
                }),
                json!(["unit_id", "table", "key", "value"]),
            ),
            empty_result.clone(),
            10_000,
        ),
        method(
            "unit.deleteRecord",
            HostServiceKind::Write,
            object_schema(
                json!({
                    "unit_id": unit_id_schema(),
                    "table": { "type": "string" },
                    "key": { "type": "string" }
                }),
                json!(["unit_id", "table", "key"]),
            ),
            object_schema(
                json!({ "deleted": { "type": "integer", "minimum": 0 } }),
                json!(["deleted"]),
            ),
            10_000,
        ),
        method(
            "unit.deleteRecords",
            HostServiceKind::Write,
            object_schema(
                json!({
                    "unit_id": unit_id_schema(),
                    "table": { "type": "string" },
                    "keys": { "type": "array", "items": { "type": "string" } }
                }),
                json!(["unit_id", "table", "keys"]),
            ),
            object_schema(
                json!({ "deleted": { "type": "integer", "minimum": 0 } }),
                json!(["deleted"]),
            ),
            10_000,
        ),
        method(
            "unit.setGlobal",
            HostServiceKind::Write,
            object_schema(
                json!({ "unit_id": unit_id_schema(), "value": {} }),
                json!(["unit_id", "value"]),
            ),
            empty_result.clone(),
            10_000,
        ),
        method(
            "unit.close",
            HostServiceKind::Write,
            object_schema(json!({ "unit_id": unit_id_schema() }), json!(["unit_id"])),
            empty_result,
            5_000,
        ),
    ];
    HostServiceTable::new(STORAGE_KV_V1, methods)
}

fn unit_id_schema() -> Value {
    json!({ "type": "string" })
}

fn object_schema(properties: Value, required: Value) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    })
}

fn method(
    name: &str,
    kind: HostServiceKind,
    args_schema: Value,
    result_schema: Value,
    timeout_ms: u64,
) -> HostServiceMethod {
    HostServiceMethod {
        name: name.to_owned(),
        kind,
        args_schema,
        result_schema,
        timeout_ms,
        idempotent: true,
        requires_session: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::HostServiceRouter;
    use dshr_compat_dsh_protocol::{
        Envelope, CAP_SERVICE_PROXY, KIND_HOST_SERVICE_REQUEST, PROTOCOL_MAX,
    };
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    const PLUGIN: &str = "example.dsh.tool";

    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "dshr-storage-kv-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn service(name: &str) -> (StorageKvService, PathBuf) {
        let root = temp_root(name);
        let service = StorageKvService::open(&root).expect("storage service opens");
        (service, root)
    }

    fn ctx(plugin: &str) -> ServiceCallContext {
        ServiceCallContext {
            plugin_id: plugin.to_owned(),
            generation: "node-0001".to_owned(),
            session_id: None,
            depth: 0,
        }
    }

    fn open_args(
        name: &str,
        version: u32,
        compatible_versions: &[u32],
        layout: StorageLayout,
        tables: &[&str],
        has_global: bool,
    ) -> Value {
        json!({ "unit": {
            "name": name,
            "version": version,
            "compatible_versions": compatible_versions,
            "layout": layout.as_str(),
            "tables": tables,
            "has_global": has_global,
        }})
    }

    fn open_unit(
        service: &mut StorageKvService,
        plugin: &str,
        args: Value,
    ) -> Result<String, ServiceError> {
        let reply = service.dispatch("unit.open", &args, &ctx(plugin))?;
        Ok(reply["unit_id"]
            .as_str()
            .expect("unit_id in reply")
            .to_owned())
    }

    fn put(
        service: &mut StorageKvService,
        plugin: &str,
        unit_id: &str,
        table: &str,
        key: &str,
        value: Value,
    ) -> Result<(), ServiceError> {
        service
            .dispatch(
                "unit.putRecord",
                &json!({ "unit_id": unit_id, "table": table, "key": key, "value": value }),
                &ctx(plugin),
            )
            .map(|_| ())
    }

    fn load_all(
        service: &mut StorageKvService,
        plugin: &str,
        unit_id: &str,
    ) -> Result<Value, ServiceError> {
        service.dispatch("unit.loadAll", &json!({ "unit_id": unit_id }), &ctx(plugin))
    }

    fn storage_code(error: &ServiceError) -> &str {
        error
            .detail
            .as_ref()
            .and_then(|detail| detail.get("storage_code"))
            .and_then(Value::as_str)
            .unwrap_or("<none>")
    }

    fn read_medium(root: &Path, plugin: &str, unit: &str) -> Value {
        let path = root.join(plugin).join(format!("{unit}.json"));
        serde_json::from_slice(&std::fs::read(&path).expect("medium file")).expect("medium json")
    }

    /// RPC-L2-301：`unit.open` 幂等；介质文件形状符合 spec §13.8。
    #[test]
    fn rpc_l2_301_open_is_idempotent_per_descriptor() {
        let (mut service, root) = service("301");
        let args = open_args("todos", 1, &[], StorageLayout::Single, &["items"], false);
        let first = open_unit(&mut service, PLUGIN, args.clone()).expect("open");
        let second = open_unit(&mut service, PLUGIN, args).expect("reopen");
        assert_eq!(first, second, "同 descriptor 重开必须返回同 unit_id");

        let file = read_medium(&root, PLUGIN, "todos");
        assert_eq!(file["version"], 1);
        assert_eq!(file["layout"], "single");
        assert_eq!(file["compatible_versions"], json!([]));
        assert!(file["tables"]["items"].is_object());
        assert!(file["global"].is_null());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-302：`putRecord` → `loadAll` 往返一致（single layout）。
    #[test]
    fn rpc_l2_302_put_record_round_trips_in_single_layout() {
        let (mut service, root) = service("302");
        let unit_id = open_unit(
            &mut service,
            PLUGIN,
            open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
        )
        .expect("open");
        put(
            &mut service,
            PLUGIN,
            &unit_id,
            "items",
            "a",
            json!({ "n": 1 }),
        )
        .expect("put a");
        put(
            &mut service,
            PLUGIN,
            &unit_id,
            "items",
            "b",
            json!({ "n": 2 }),
        )
        .expect("put b");

        let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
        assert_eq!(snapshot["tables"]["items"]["a"]["n"], 1);
        assert_eq!(snapshot["tables"]["items"]["b"]["n"], 2);
        assert!(
            snapshot.get("global").is_none(),
            "未写 global 时不返回该字段"
        );

        let file = read_medium(&root, PLUGIN, "todos");
        assert_eq!(file["tables"]["items"]["a"]["n"], 1);
        assert_eq!(file["version"], 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-303：`per-record` layout 的读写往返（两种 layout 语义各一例）。
    #[test]
    fn rpc_l2_303_per_record_layout_round_trips() {
        let (mut service, root) = service("303");
        let unit_id = open_unit(
            &mut service,
            PLUGIN,
            open_args(
                "index",
                3,
                &[1, 2],
                StorageLayout::PerRecord,
                &["pages", "meta"],
                false,
            ),
        )
        .expect("open");
        put(&mut service, PLUGIN, &unit_id, "pages", "p1", json!("one")).expect("put");
        put(&mut service, PLUGIN, &unit_id, "pages", "p2", json!("two")).expect("put");
        put(&mut service, PLUGIN, &unit_id, "meta", "count", json!(2)).expect("put");

        let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
        assert_eq!(snapshot["tables"]["pages"]["p1"], "one");
        assert_eq!(snapshot["tables"]["pages"]["p2"], "two");
        assert_eq!(snapshot["tables"]["meta"]["count"], 2);
        assert_eq!(read_medium(&root, PLUGIN, "index")["layout"], "per-record");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-304：版本戳不兼容 → `E_INTERNAL` + `detail.storage_code="version-mismatch"`；
    /// 声明 `compatible_versions` 后可读，写入时原子升级版本戳。
    #[test]
    fn rpc_l2_304_version_stamp_mismatch_and_upgrade() {
        let (mut writer, root) = service("304");
        let unit_id = open_unit(
            &mut writer,
            PLUGIN,
            open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
        )
        .expect("open v1");
        put(&mut writer, PLUGIN, &unit_id, "items", "a", json!(1)).expect("put");
        drop(writer);

        let mut reader = StorageKvService::open(&root).expect("reopen root");
        let error = open_unit(
            &mut reader,
            PLUGIN,
            open_args("todos", 2, &[], StorageLayout::Single, &["items"], false),
        )
        .expect_err("version 2 without compat must be rejected");
        assert_eq!(error.code, "E_INTERNAL");
        assert!(!error.retryable);
        assert_eq!(storage_code(&error), "version-mismatch");

        let upgraded = open_unit(
            &mut reader,
            PLUGIN,
            open_args("todos", 2, &[1], StorageLayout::Single, &["items"], false),
        )
        .expect("version 2 accepting version 1");
        let snapshot = load_all(&mut reader, PLUGIN, &upgraded).expect("loadAll");
        assert_eq!(
            snapshot["tables"]["items"]["a"], 1,
            "兼容版本必须能读旧数据"
        );
        put(&mut reader, PLUGIN, &upgraded, "items", "b", json!(2)).expect("put");
        assert_eq!(
            read_medium(&root, PLUGIN, "todos")["version"],
            2,
            "写操作原子打上当前版本戳"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-305：`deleteRecord` / `deleteRecords` 幂等、计数正确、缺 key 不报错。
    #[test]
    fn rpc_l2_305_deletes_are_idempotent() {
        let (mut service, root) = service("305");
        let unit_id = open_unit(
            &mut service,
            PLUGIN,
            open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
        )
        .expect("open");
        for key in ["a", "b", "c"] {
            put(&mut service, PLUGIN, &unit_id, "items", key, json!(key)).expect("put");
        }

        let reply = service
            .dispatch(
                "unit.deleteRecord",
                &json!({ "unit_id": unit_id, "table": "items", "key": "a" }),
                &ctx(PLUGIN),
            )
            .expect("delete a");
        assert_eq!(reply["deleted"], 1);
        let reply = service
            .dispatch(
                "unit.deleteRecord",
                &json!({ "unit_id": unit_id, "table": "items", "key": "a" }),
                &ctx(PLUGIN),
            )
            .expect("delete a again");
        assert_eq!(reply["deleted"], 0, "删不存在的 key 不报错");

        let reply = service
            .dispatch(
                "unit.deleteRecords",
                &json!({ "unit_id": unit_id, "table": "items", "keys": ["b", "missing"] }),
                &ctx(PLUGIN),
            )
            .expect("deleteRecords");
        assert_eq!(reply["deleted"], 1);
        let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
        assert!(snapshot["tables"]["items"].get("b").is_none());
        assert_eq!(snapshot["tables"]["items"]["c"], "c");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-306：`setGlobal` / `global` 语义；未写时 loadAll 不带 global；
    /// `has_global:false` → `facet-unsupported`。
    #[test]
    fn rpc_l2_306_global_semantics() {
        let (mut service, root) = service("306");
        let unit_id = open_unit(
            &mut service,
            PLUGIN,
            open_args("state", 1, &[], StorageLayout::Single, &["kv"], true),
        )
        .expect("open with global");
        let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
        assert!(
            snapshot.get("global").is_none(),
            "未写 global 时按 initial 处理"
        );

        service
            .dispatch(
                "unit.setGlobal",
                &json!({ "unit_id": unit_id, "value": { "theme": "dark" } }),
                &ctx(PLUGIN),
            )
            .expect("setGlobal");
        let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
        assert_eq!(snapshot["global"]["theme"], "dark");

        let error = service
            .dispatch(
                "unit.setGlobal",
                &json!({ "unit_id": unit_id, "value": null }),
                &ctx(PLUGIN),
            )
            .expect_err("null is the never-written sentinel");
        assert_eq!(error.code, "E_SERVICE_ARGS");

        let no_global = open_unit(
            &mut service,
            PLUGIN,
            open_args("no_global", 1, &[], StorageLayout::Single, &[], false),
        )
        .expect("open without global");
        let error = service
            .dispatch(
                "unit.setGlobal",
                &json!({ "unit_id": no_global, "value": 1 }),
                &ctx(PLUGIN),
            )
            .expect_err("facet unsupported");
        assert_eq!(error.code, "E_INTERNAL");
        assert_eq!(storage_code(&error), "facet-unsupported");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-307：`loadAll` > 768 KiB → `E_TOO_LARGE`，已有记录不被截断。
    #[test]
    fn rpc_l2_307_oversized_load_all_is_too_large() {
        let (mut service, root) = service("307");
        let unit_id = open_unit(
            &mut service,
            PLUGIN,
            open_args("big", 1, &[], StorageLayout::Single, &["items"], false),
        )
        .expect("open");
        let payload = "x".repeat(20_000);
        for index in 0..40 {
            put(
                &mut service,
                PLUGIN,
                &unit_id,
                "items",
                &format!("k{index}"),
                json!(payload),
            )
            .expect("put");
        }
        let error = load_all(&mut service, PLUGIN, &unit_id).expect_err("must exceed 768 KiB");
        assert_eq!(error.code, "E_TOO_LARGE");

        let keys: Vec<String> = (0..40).map(|index| format!("k{index}")).collect();
        let reply = service
            .dispatch(
                "unit.deleteRecords",
                &json!({ "unit_id": unit_id, "table": "items", "keys": keys }),
                &ctx(PLUGIN),
            )
            .expect("delete all");
        assert_eq!(reply["deleted"], 40, "超限读取不得截断/丢失记录");
        let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("small again");
        assert!(snapshot["tables"]["items"].as_object().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-308：同 unit 并发 `putRecord` 串行化，不丢写（per-unit mutex + 原子写）。
    #[test]
    fn rpc_l2_308_concurrent_puts_do_not_lose_writes() {
        let root = temp_root("308");
        let mut handles = Vec::new();
        for thread_index in 0..2 {
            let root = root.clone();
            handles.push(thread::spawn(move || {
                let mut service = StorageKvService::open(&root).expect("open");
                let unit_id = open_unit(
                    &mut service,
                    PLUGIN,
                    open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
                )
                .expect("unit.open");
                for index in 0..40 {
                    put(
                        &mut service,
                        PLUGIN,
                        &unit_id,
                        "items",
                        &format!("t{thread_index}-k{index}"),
                        json!(index),
                    )
                    .expect("put");
                }
            }));
        }
        for handle in handles {
            handle.join().expect("writer thread");
        }

        let mut verifier = StorageKvService::open(&root).expect("open");
        let unit_id = open_unit(
            &mut verifier,
            PLUGIN,
            open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
        )
        .expect("reopen");
        let snapshot = load_all(&mut verifier, PLUGIN, &unit_id).expect("loadAll");
        assert_eq!(
            snapshot["tables"]["items"].as_object().unwrap().len(),
            80,
            "并发写不能丢记录"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-309：跨插件作用域拒绝（同 unit 名各写各的域，不能借 unit_id 越权）。
    #[test]
    fn rpc_l2_309_cross_plugin_scopes_are_denied() {
        let (mut service, root) = service("309");
        let plugin_a = "example.dsh.tool";
        let plugin_b = "example.other.tool";
        let unit_a = open_unit(
            &mut service,
            plugin_a,
            open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
        )
        .expect("open A");
        put(
            &mut service,
            plugin_a,
            &unit_a,
            "items",
            "a",
            json!("secret"),
        )
        .expect("put A");

        let error = load_all(&mut service, plugin_b, &unit_a).expect_err("must deny");
        assert_eq!(error.code, "E_SERVICE_DENIED");

        let unit_b = open_unit(
            &mut service,
            plugin_b,
            open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
        )
        .expect("open B");
        assert_ne!(unit_a, unit_b, "不同插件作用域必须分配不同 unit");
        let snapshot = load_all(&mut service, plugin_b, &unit_b).expect("loadAll B");
        assert!(snapshot["tables"]["items"].as_object().unwrap().is_empty());
        assert!(
            root.join(plugin_a).join("todos.json").exists()
                && root.join(plugin_b).join("todos.json").exists()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-310：`close` 幂等；关闭后操作 → `closed`；重开/重启后能读回旧数据。
    #[test]
    fn rpc_l2_310_close_and_restart_keep_data() {
        let (mut service, root) = service("310");
        let unit_id = open_unit(
            &mut service,
            PLUGIN,
            open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
        )
        .expect("open");
        put(&mut service, PLUGIN, &unit_id, "items", "a", json!(1)).expect("put");
        for _ in 0..2 {
            service
                .dispatch("unit.close", &json!({ "unit_id": unit_id }), &ctx(PLUGIN))
                .expect("close is idempotent");
        }
        let error = load_all(&mut service, PLUGIN, &unit_id).expect_err("closed unit");
        assert_eq!(error.code, "E_SERVICE_UNAVAILABLE");
        assert_eq!(storage_code(&error), "closed");

        // 同实例重开：unit_id 不变、数据读回。
        let reopened = open_unit(
            &mut service,
            PLUGIN,
            open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
        )
        .expect("reopen");
        assert_eq!(reopened, unit_id);
        let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
        assert_eq!(snapshot["tables"]["items"]["a"], 1);
        drop(service);

        // 重启（新 service 实例）后持久化仍在。
        let mut restarted = StorageKvService::open(&root).expect("restart");
        let unit_id = open_unit(
            &mut restarted,
            PLUGIN,
            open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
        )
        .expect("open after restart");
        let snapshot = load_all(&mut restarted, PLUGIN, &unit_id).expect("loadAll");
        assert_eq!(snapshot["tables"]["items"]["a"], 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-311：descriptor/参数非法 → `E_SERVICE_ARGS`；未知 unit → `unknown-state`。
    #[test]
    fn rpc_l2_311_invalid_descriptor_and_unknown_unit() {
        let (mut service, root) = service("311");
        for args in [
            open_args("Bad-Name", 1, &[], StorageLayout::Single, &[], false),
            open_args("todos", 1, &[1], StorageLayout::Single, &[], false),
            open_args(
                "todos",
                1,
                &[],
                StorageLayout::Single,
                &["Bad-Table"],
                false,
            ),
            json!({ "unit": { "name": "todos", "version": -1 } }),
        ] {
            let error = open_unit(&mut service, PLUGIN, args).expect_err("invalid descriptor");
            assert_eq!(error.code, "E_SERVICE_ARGS", "{error:?}");
        }

        let error = load_all(&mut service, PLUGIN, "unit-999999").expect_err("unknown unit");
        assert_eq!(error.code, "E_SERVICE_UNAVAILABLE");
        assert_eq!(storage_code(&error), "unknown-state");

        let unit_id = open_unit(
            &mut service,
            PLUGIN,
            open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
        )
        .expect("open");
        let error = put(&mut service, PLUGIN, &unit_id, "Bad-Table", "a", json!(1))
            .expect_err("invalid table name");
        assert_eq!(error.code, "E_SERVICE_ARGS");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-312：同 scope 不同 descriptor 的并发 open 冲突 → `already-open`。
    #[test]
    fn rpc_l2_312_descriptor_conflict_is_already_open() {
        let (mut service, root) = service("312");
        let args = open_args("todos", 1, &[], StorageLayout::Single, &["items"], false);
        let unit_id = open_unit(&mut service, PLUGIN, args).expect("open");
        let different_layout =
            open_args("todos", 1, &[], StorageLayout::PerRecord, &["items"], false);
        let error = open_unit(&mut service, PLUGIN, different_layout).expect_err("conflict");
        assert_eq!(error.code, "E_INTERNAL");
        assert_eq!(storage_code(&error), "already-open");

        // 关闭后可用新 descriptor 重开（介质按新 descriptor 对齐）。
        service
            .dispatch("unit.close", &json!({ "unit_id": unit_id }), &ctx(PLUGIN))
            .expect("close");
        let reopened = open_unit(
            &mut service,
            PLUGIN,
            open_args("todos", 1, &[], StorageLayout::PerRecord, &["items"], false),
        )
        .expect("reopen with new layout");
        assert_eq!(reopened, unit_id);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-313：经 `HostServiceRouter` 的真实往返（args_schema/白名单/结果上限都生效）。
    #[test]
    fn rpc_l2_313_round_trips_through_router() {
        let root = temp_root("313");
        let mut router = HostServiceRouter::new(
            "node-0001",
            PROTOCOL_MAX,
            vec![CAP_SERVICE_PROXY.to_owned()],
        )
        .expect("router");
        router
            .register(Box::new(StorageKvService::open(&root).expect("storage")))
            .expect("register storage-kv");
        router.register_plugin_inject(PLUGIN, vec![STORAGE_KV_V1.to_owned()]);

        let call = |request_id: &str, method: &str, args: Value| {
            Envelope::host(
                KIND_HOST_SERVICE_REQUEST,
                request_id,
                json!({ "method": method, "args": args }),
            )
            .with_version(PROTOCOL_MAX)
            .with_generation("node-0001")
            .with_plugin(PLUGIN)
            .with_service_key(STORAGE_KV_V1)
            .with_depth(1)
        };

        let reply = router.handle_request(&call(
            "req-313a",
            "unit.open",
            open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
        ));
        assert!(reply.error.is_none(), "unit.open failed: {reply:?}");
        let unit_id = reply.payload["result"]["unit_id"]
            .as_str()
            .expect("unit_id")
            .to_owned();

        let reply = router.handle_request(&call(
            "req-313b",
            "unit.putRecord",
            json!({ "unit_id": unit_id, "table": "items", "key": "a", "value": { "n": 1 } }),
        ));
        assert!(reply.error.is_none(), "unit.putRecord failed: {reply:?}");

        let reply = router.handle_request(&call(
            "req-313c",
            "unit.loadAll",
            json!({ "unit_id": unit_id }),
        ));
        assert!(reply.error.is_none(), "unit.loadAll failed: {reply:?}");
        assert_eq!(reply.payload["result"]["tables"]["items"]["a"]["n"], 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
