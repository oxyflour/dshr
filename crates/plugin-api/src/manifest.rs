//! CPP-v1 manifest 的 typed 表示与装载期校验（spec/cpp-v1.md §3）。
//!
//! 分层（宿主装载时按顺序调用）：
//! 1. [`Manifest::from_json_str`] / [`Manifest::from_path`]：**严格**解析，未知字段直接失败；
//! 2. [`Manifest::validate`]：`api`、`min_host_version`、配额、`inject`、tool、ui 的语义校验。
//!
//! 本模块只回答"这份 manifest 能不能装"，不做任何注册、不依赖 `plugin-host`。
//! 版本比较自带一个极简 SemVer 实现（`x.y.z[-pre][+build]`），不引入新 crate；
//! build metadata 按 SemVer §10 接受但不参与比较。

use std::cmp::Ordering;
use std::fmt;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::PluginError;

/// manifest 唯一支持的 API 版本（spec/cpp-v1.md §3.3）。
pub const CPP_API: &str = "cordis-portable/v1";

/// WASM fuel 上限：1e10（spec/cpp-v1.md §3.5）。
pub const FUEL_LIMIT: u64 = 10_000_000_000;
/// WASM 线性内存上限：512 MB（spec/cpp-v1.md §3.5）。
pub const MAX_MEMORY_MB_LIMIT: u64 = 512;
/// 单次运行超时下限：1 s（schema `runtime.timeout_ms` / `tools[].timeout_ms`）。
pub const TIMEOUT_MS_MIN: u64 = 1_000;
/// 单次运行超时上限：600 s（spec/cpp-v1.md §3.5）。
pub const TIMEOUT_MS_LIMIT: u64 = 600_000;

/// 插件作者写的 manifest（`plugin.toml` 序列化成 JSON 后的形态）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub plugin: PluginMeta,
    pub runtime: RuntimeSpec,
    #[serde(default)]
    pub inject: Option<InjectSpec>,
    #[serde(default)]
    pub permissions: Option<Permissions>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    pub ui: Vec<UiEntry>,
}

impl Manifest {
    /// 解析 JSON 文本；字段未知、类型错误、`api` 不匹配都在这里失败。
    pub fn from_json_str(raw: &str) -> Result<Self, PluginError> {
        let manifest =
            serde_json::from_str::<Manifest>(raw).map_err(|err| PluginError::Manifest {
                path: "<inline>".to_owned(),
                reason: err.to_string(),
            })?;
        manifest.check_api()?;
        Ok(manifest)
    }

    /// 读取 manifest 文件再解析；IO 错误与解析错误都带文件路径。
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, PluginError> {
        let path = path.as_ref();
        let path_text = path.display().to_string();
        let raw = fs::read_to_string(path).map_err(|err| PluginError::Manifest {
            path: path_text.clone(),
            reason: format!("cannot read manifest file: {err}"),
        })?;
        let manifest =
            serde_json::from_str::<Manifest>(&raw).map_err(|err| PluginError::Manifest {
                path: path_text,
                reason: err.to_string(),
            })?;
        manifest.check_api()?;
        Ok(manifest)
    }

    /// 装载期语义校验；`host` 是当前宿主版本（`min_host_version` 的比较基准）。
    pub fn validate(&self, host: &HostVersion) -> Result<(), PluginError> {
        self.check_api()?;

        // 插件自身版本只校验格式，不参与宿主兼容判断。
        Version::parse(&self.plugin.version)
            .map_err(|reason| invalid_manifest(format!("plugin.version: {reason}")))?;

        if let Some(required_raw) = &self.plugin.min_host_version {
            let required = Version::parse(required_raw)
                .map_err(|reason| invalid_manifest(format!("plugin.min_host_version: {reason}")))?;
            if host.version() < &required {
                return Err(PluginError::HostTooOld {
                    required: required_raw.clone(),
                    actual: host.to_string(),
                });
            }
        }

        self.runtime.validate()?;
        if let Some(inject) = &self.inject {
            inject.validate()?;
        }
        self.validate_tools()?;
        self.validate_ui()?;
        Ok(())
    }

    fn check_api(&self) -> Result<(), PluginError> {
        if self.plugin.api == CPP_API {
            Ok(())
        } else {
            Err(PluginError::UnsupportedApi {
                expected: CPP_API.to_owned(),
                actual: self.plugin.api.clone(),
            })
        }
    }

    fn validate_tools(&self) -> Result<(), PluginError> {
        let mut seen: Vec<&str> = Vec::new();
        for (index, tool) in self.tools.iter().enumerate() {
            tool.validate()
                .map_err(|reason| invalid_manifest(format!("tools[{index}].{reason}")))?;
            if seen.contains(&tool.name.as_str()) {
                return Err(invalid_manifest(format!(
                    "tools[{index}].name: duplicate tool name `{}`",
                    tool.name
                )));
            }
            seen.push(tool.name.as_str());
        }
        Ok(())
    }

    fn validate_ui(&self) -> Result<(), PluginError> {
        for (index, entry) in self.ui.iter().enumerate() {
            if entry.slot.trim().is_empty() {
                return Err(invalid_manifest(format!(
                    "ui[{index}].slot: must not be empty"
                )));
            }
            for (field, value) in [
                ("id", &entry.id),
                ("label", &entry.label),
                ("view", &entry.view),
            ] {
                if value.trim().is_empty() {
                    return Err(invalid_manifest(format!(
                        "ui[{index}].{field}: must not be empty"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// `[plugin]` 段。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginMeta {
    pub id: String,
    pub version: String,
    pub api: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 最低宿主版本（SemVer）；宿主更旧时 `validate` 返回 `HostTooOld`。
    #[serde(default)]
    pub min_host_version: Option<String>,
}

/// `[runtime]` 段。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSpec {
    pub kind: RuntimeKind,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub fuel: Option<u64>,
    #[serde(default)]
    pub max_memory_mb: Option<u64>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl RuntimeSpec {
    fn validate(&self) -> Result<(), PluginError> {
        if let Some(fuel) = self.fuel {
            if fuel > FUEL_LIMIT {
                return Err(invalid_manifest(format!(
                    "runtime.fuel: {fuel} exceeds maximum {FUEL_LIMIT}"
                )));
            }
        }
        if let Some(memory) = self.max_memory_mb {
            if memory == 0 || memory > MAX_MEMORY_MB_LIMIT {
                return Err(invalid_manifest(format!(
                    "runtime.max_memory_mb: {memory} must be within 1..={MAX_MEMORY_MB_LIMIT}"
                )));
            }
        }
        if let Some(timeout) = self.timeout_ms {
            if !(TIMEOUT_MS_MIN..=TIMEOUT_MS_LIMIT).contains(&timeout) {
                return Err(invalid_manifest(format!(
                    "runtime.timeout_ms: {timeout} must be within {TIMEOUT_MS_MIN}..={TIMEOUT_MS_LIMIT}"
                )));
            }
        }
        Ok(())
    }
}

/// `runtime.kind`（spec/cpp-v1.md §3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeKind {
    RustStatic,
    WasmComponent,
    ExternalProcess,
}

/// `[inject]` 段：声明的 Service key 列表（spec/cpp-v1.md §5.1）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InjectSpec {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
}

impl InjectSpec {
    fn validate(&self) -> Result<(), PluginError> {
        for (index, key) in self.required.iter().enumerate() {
            validate_service_key(key).map_err(|reason| {
                invalid_manifest(format!("inject.required[{index}]: {reason}"))
            })?;
        }
        for (index, key) in self.optional.iter().enumerate() {
            validate_service_key(key).map_err(|reason| {
                invalid_manifest(format!("inject.optional[{index}]: {reason}"))
            })?;
        }
        Ok(())
    }
}

/// `[permissions]` 段；缺省全部为 `false`（spec/cpp-v1.md §3.4）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Permissions {
    pub workspace_read: bool,
    pub workspace_write: bool,
    pub network: bool,
    pub network_allow: Vec<String>,
    pub process_spawn: bool,
    pub credential_use: bool,
}

/// `[[tools]]` 声明。注意这里**不是**运行时的 `ToolDefinition`：
/// 校验通过后由宿主决定如何转成 Registry 的注册项。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: String,
    pub output_schema: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub cancelable: Option<bool>,
    /// 未知卡片类型不拒绝（运行期退回 `generic`，spec/cpp-v1.md §7.1）。
    #[serde(default)]
    pub presentation: Option<String>,
}

impl ToolSpec {
    /// 校验单个工具声明；返回的原因以字段名开头（宿主可补 `tools[i].` 前缀）。
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_tool_name(&self.name) {
            return Err(format!(
                "name: `{}` must match `^[a-z][a-z0-9_]{{0,63}}$`",
                self.name
            ));
        }
        let description_len = self.description.chars().count();
        if description_len == 0 || description_len > 1024 {
            return Err(format!(
                "description: length {description_len} must be within 1..=1024"
            ));
        }
        if !self.input_schema.ends_with(".json") {
            return Err("input_schema: must be a `.json` path".to_owned());
        }
        if !self.output_schema.ends_with(".json") {
            return Err("output_schema: must be a `.json` path".to_owned());
        }
        if let Some(timeout) = self.timeout_ms {
            if !(TIMEOUT_MS_MIN..=TIMEOUT_MS_LIMIT).contains(&timeout) {
                return Err(format!(
                    "timeout_ms: {timeout} must be within {TIMEOUT_MS_MIN}..={TIMEOUT_MS_LIMIT}"
                ));
            }
        }
        Ok(())
    }
}

/// `[[ui]]` 声明（语法糖，等价于 `ctx.ui.register`）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiEntry {
    pub slot: String,
    pub id: String,
    pub label: String,
    pub view: String,
    /// 对齐 adc-v1 `workspace.panel` 的 location。
    #[serde(default)]
    pub location: Option<UiLocation>,
    #[serde(default)]
    pub order: Option<f64>,
}

/// UI 面板位置，对齐 `schema/adc-v1/slot-registration.schema.json`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiLocation {
    Left,
    Right,
    Bottom,
    Center,
}

/// 宿主版本；`Manifest::validate` 用它判断 `min_host_version` 是否满足。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostVersion(Version);

impl HostVersion {
    /// 用 major/minor/patch 构造（build/预发布留空）。
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self(Version {
            major,
            minor,
            patch,
            prerelease: None,
        })
    }

    /// 解析 `x.y.z[-pre][+build]`。
    pub fn parse(raw: &str) -> Result<Self, String> {
        Version::parse(raw).map(Self)
    }

    pub fn version(&self) -> &Version {
        &self.0
    }
}

impl fmt::Display for HostVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Version> for HostVersion {
    fn from(version: Version) -> Self {
        Self(version)
    }
}

/// 极简 SemVer 版本（`x.y.z[-pre][+build]`）。
///
/// - 数字段按数值比较；预发布 < 正式；预发布标识符按 SemVer §11 的优先级比较；
/// - 前导零按 SemVer 视为非法；build metadata 接受但不保留、不参与比较。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: Option<String>,
}

impl Version {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let (core_and_pre, build) = match raw.split_once('+') {
            Some((left, build)) => (left, Some(build)),
            None => (raw, None),
        };
        if let Some(build) = build {
            if !valid_build_identifiers(build) {
                return Err(format!("invalid build metadata `{build}`"));
            }
        }
        let (core, prerelease) = match core_and_pre.split_once('-') {
            Some((core, prerelease)) => (core, Some(prerelease)),
            None => (core_and_pre, None),
        };
        if let Some(prerelease) = prerelease {
            if !valid_prerelease_identifiers(prerelease) {
                return Err(format!("invalid prerelease `{prerelease}`"));
            }
        }
        let mut segments = core.split('.');
        let major = parse_core_number(segments.next(), "major")?;
        let minor = parse_core_number(segments.next(), "minor")?;
        let patch = parse_core_number(segments.next(), "patch")?;
        if segments.next().is_some() {
            return Err(format!("`{raw}` has more than three numeric segments"));
        }
        Ok(Self {
            major,
            minor,
            patch,
            prerelease: prerelease.map(str::to_owned),
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(prerelease) = &self.prerelease {
            write!(f, "-{prerelease}")?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.prerelease, &other.prerelease) {
                (None, None) => Ordering::Equal,
                // 有预发布后缀的版本低于同名正式版。
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => compare_prerelease(left, right),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_prerelease(left: &str, right: &str) -> Ordering {
    let mut left_parts = left.split('.');
    let mut right_parts = right.split('.');
    loop {
        match (left_parts.next(), right_parts.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left_id), Some(right_id)) => {
                let ordering = match (
                    is_numeric_identifier(left_id),
                    is_numeric_identifier(right_id),
                ) {
                    // 数字标识符没有前导零，所以"更长 = 数值更大"。
                    (true, true) => left_id
                        .len()
                        .cmp(&right_id.len())
                        .then_with(|| left_id.cmp(right_id)),
                    // 数字标识符优先级低于字母标识符。
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => left_id.cmp(right_id),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

fn parse_core_number(raw: Option<&str>, name: &str) -> Result<u64, String> {
    let Some(raw) = raw else {
        return Err(format!("missing {name} version segment"));
    };
    if !is_numeric_identifier(raw) {
        return Err(format!("{name} segment `{raw}` is not numeric"));
    }
    if raw.len() > 1 && raw.starts_with('0') {
        return Err(format!("{name} segment `{raw}` has a leading zero"));
    }
    raw.parse::<u64>()
        .map_err(|_| format!("{name} segment `{raw}` does not fit in u64"))
}

fn valid_prerelease_identifiers(raw: &str) -> bool {
    !raw.is_empty()
        && raw.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !(is_numeric_identifier(identifier)
                    && identifier.len() > 1
                    && identifier.starts_with('0'))
        })
}

fn valid_build_identifiers(raw: &str) -> bool {
    !raw.is_empty()
        && raw.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn is_numeric_identifier(raw: &str) -> bool {
    !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit())
}

/// Service key 形如 `<name>/v<major>`（spec/cpp-v1.md §5.1），
/// 与 schema 的 `^[a-z0-9-]+/v[0-9]+$` 对齐。
fn validate_service_key(key: &str) -> Result<(), String> {
    let Some((name, version)) = key.rsplit_once('/') else {
        return Err(format!("service key `{key}` must be `<name>/v<major>`"));
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!(
            "service key `{key}` has invalid name part; expected `[a-z0-9-]+`"
        ));
    }
    let Some(major) = version.strip_prefix('v') else {
        return Err(format!("service key `{key}` must end with `/v<major>`"));
    };
    if major.is_empty() || !major.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "service key `{key}` has invalid major part; expected `v<digits>`"
        ));
    }
    Ok(())
}

/// tool 名：`[a-z][a-z0-9_]*`，长度 ≤ 64（spec/cpp-v1.md §7.1）。
fn is_valid_tool_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn invalid_manifest(reason: impl Into<String>) -> PluginError {
    PluginError::InvalidManifest(reason.into())
}
