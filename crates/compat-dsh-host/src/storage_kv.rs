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
        let UnitFile { tables, global, .. } = file;
        // 官方 dsh-storage-domain 用 `snapshot.global === null` 判定"从未写过"并回落到
        // schema 的 `initial`；所以未写 global 时也必须显式返回 null，不能省略字段。
        let result = json!({
            "tables": tables,
            "global": global.unwrap_or(Value::Null),
        });
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
mod tests;
