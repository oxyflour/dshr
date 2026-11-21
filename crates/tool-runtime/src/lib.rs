//! Tool Registry：注册、查找、Authority 校验（Policy + 参数 schema）。
//!
//! 设计要点（对应 PLAN §2.1 与 spec/cpp-v1.md §7）：
//! - 工具来源是**一等的**：`Native` / `Sidecar` / `Mcp` 三种，注册表里必须能区分；
//! - `Sidecar` 工具绑定 `generation`，generation 失效时整批撤销（规范 §5）；
//! - 所有调用先过 `ToolPolicy` 再过参数 schema——**Policy 不能只在 UI 层做**。
//!
//! 执行本身（并发、超时、取消）不在这里：本 crate 只回答"能不能调用"。

#![forbid(unsafe_code)]

pub mod approval;

pub use approval::{
    ApprovalBroker, ApprovalOutcome, ApprovalRecord, LiveApproval, PendingApproval,
};

use std::collections::BTreeMap;

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOrigin {
    Native {
        plugin_id: String,
    },
    Sidecar {
        plugin_id: String,
        registration_id: String,
        generation: String,
    },
    Mcp {
        server: String,
    },
}

impl ToolOrigin {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Native { .. } => "native",
            Self::Sidecar { .. } => "sidecar",
            Self::Mcp { .. } => "mcp",
        }
    }

    pub fn generation(&self) -> Option<&str> {
        match self {
            Self::Sidecar { generation, .. } => Some(generation),
            _ => None,
        }
    }
}

/// 结果卡片类型：对齐 DSH 的 `presentResult` 视图。
///
/// 这里只做**枚举化**：字符串来自插件（可能是新版本带来的未知卡片），
/// 未知值一律退回 `Generic`，不允许因为卡片类型不认识就丢掉结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presentation {
    Generic,
    Terminal,
    Diff,
    Search,
    Read,
    Web,
}

impl Presentation {
    pub const ALL: [Self; 6] = [
        Self::Generic,
        Self::Terminal,
        Self::Diff,
        Self::Search,
        Self::Read,
        Self::Web,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Terminal => "terminal",
            Self::Diff => "diff",
            Self::Search => "search",
            Self::Read => "read",
            Self::Web => "web",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|card| card.as_str().eq_ignore_ascii_case(name))
    }
}

/// 工具声明的最小超时：1 s（spec/cpp-v1.md §3.5）。
pub const TOOL_TIMEOUT_MIN_MS: u64 = 1_000;
/// 工具声明的最大超时：600 s（spec/cpp-v1.md §3.5）。
pub const TOOL_TIMEOUT_MAX_MS: u64 = 600_000;
/// 未声明时的默认工具超时：120 s（spec/cpp-v1.md §7.1）。
pub const DEFAULT_TOOL_TIMEOUT_MS: u64 = 120_000;

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub origin: ToolOrigin,
    /// 完整 JSON Schema 子集：type / required / properties / additionalProperties / enum / items。
    pub input_schema: Value,
    /// 结果 schema（对齐 DSH 的 `output.schema`；L1 暂不校验，但必须能穿过协议）。
    pub output_schema: Option<Value>,
    /// 展示卡片类型（对齐 DSH 的 `presentResult` 视图：generic/terminal/diff/search/read/web）。
    pub presentation: Option<String>,
    /// 工具声明的超时（ms）；执行方应通过 [`ToolDefinition::effective_timeout_ms`]
    /// 取值，保证 1 s–600 s 的上下限（DSH `dsh-tool-call-timeout-policy` 语义）。
    pub timeout_ms: u64,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        origin: ToolOrigin,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            origin,
            input_schema: Value::Object(Default::default()),
            output_schema: None,
            presentation: None,
            timeout_ms: DEFAULT_TOOL_TIMEOUT_MS,
        }
    }

    pub fn with_output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    pub fn with_presentation(mut self, card: impl Into<String>) -> Self {
        self.presentation = Some(card.into());
        self
    }

    pub fn with_schema(mut self, schema: Value) -> Self {
        self.input_schema = schema;
        self
    }

    /// 显式声明工具超时（per-tool timeout 透传的注册侧入口）。
    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    /// 实际执行超时：把声明值 clamp 到 spec 的 1 s–600 s（无声明时的默认值也合法）。
    ///
    /// 宿主/执行器应使用这个值，保证"按工具声明 timeout 执行"（DSH
    /// `dsh-tool-call-timeout-policy`）不会因脏数据变成 0 或无限等待。
    pub fn effective_timeout_ms(&self) -> u64 {
        self.timeout_ms
            .clamp(TOOL_TIMEOUT_MIN_MS, TOOL_TIMEOUT_MAX_MS)
    }

    /// 执行超时的 `Duration` 形式。
    pub fn effective_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.effective_timeout_ms())
    }

    /// 把字符串卡片类型解析成枚举；未知/缺省一律 `Generic`。
    pub fn card(&self) -> Presentation {
        self.presentation
            .as_deref()
            .and_then(Presentation::parse)
            .unwrap_or(Presentation::Generic)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    /// 需要用户确认；Registry 不弹窗，只把决定交回调用方。
    Ask,
    Deny(String),
}

pub trait ToolPolicy: Send {
    fn check(&self, tool: &ToolDefinition, arguments: &Value) -> PolicyDecision;
}

/// 默认策略：全部允许（M1 用；M2 换成真实的审批策略）。
#[derive(Debug, Default)]
pub struct AllowAll;

impl ToolPolicy for AllowAll {
    fn check(&self, _tool: &ToolDefinition, _arguments: &Value) -> PolicyDecision {
        PolicyDecision::Allow
    }
}

/// 桌面默认策略：读类工具直接放行，写入/执行类需要用户确认。
///
/// 判定顺序：显式 deny 名单 → read-only 的 terminal/diff 拒绝 → 名字命中写入/执行
/// 名单 → 展示类型是 `terminal`/`diff` → 其余 Allow。名单可通过
/// [`MUTATING_TOOL_NAMES`] 扩展，保持"默认拒绝破坏性操作"。
#[derive(Debug, Default)]
pub struct ApprovalPolicy {
    pub deny: Vec<String>,
    pub ask: Vec<String>,
    /// 是否把所有 `terminal` / `diff` 展示的工具都归入 Ask。
    pub ask_terminal_and_diff: bool,
    /// 无条件 Ask（自检/演示用）。
    pub ask_everything: bool,
    /// `read-only` 模式：terminal/diff 等执行类展示类型直接 Deny。
    pub deny_terminal_and_diff: bool,
}

/// DSH 的权限模式（`DSH_PERMISSION_MODE`：`read-only` / `workspace-write` /
/// `danger-full-access`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl PermissionMode {
    /// 环境变量名；`ApprovalPolicy::from_env` 读取它。
    pub const ENV: &'static str = "DSHR_PERMISSION_MODE";

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "read-only" | "read_only" | "readonly" => Some(Self::ReadOnly),
            "workspace-write" | "workspace_write" | "workspacewrite" => Some(Self::WorkspaceWrite),
            "danger-full-access" | "danger_full_access" | "dangerfullaccess" => {
                Some(Self::DangerFullAccess)
            }
            _ => None,
        }
    }

    /// 从 `DSHR_PERMISSION_MODE` 读取；未配置默认 `workspace-write`（与 DSH 一致）；
    /// 非法值记录警告后回退默认，不失败。
    pub fn from_env() -> Self {
        let raw = match std::env::var(Self::ENV) {
            Ok(raw) => raw,
            Err(_) => return Self::WorkspaceWrite,
        };
        match Self::parse(&raw) {
            Some(mode) => mode,
            None => {
                eprintln!(
                    "[tool-runtime] ignoring invalid {}={raw} (allowed: read-only/workspace-write/danger-full-access)",
                    Self::ENV
                );
                Self::WorkspaceWrite
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

impl ApprovalPolicy {
    /// 默认：常见写入/执行工具名要求确认，terminal/diff 卡片也要求确认。
    pub fn desktop() -> Self {
        Self {
            deny: Vec::new(),
            ask: MUTATING_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            ask_terminal_and_diff: true,
            ask_everything: false,
            deny_terminal_and_diff: false,
        }
    }

    /// `workspace-write` 模式 = 现有的桌面默认（写入/执行类 Ask）。
    pub fn workspace_write() -> Self {
        Self::desktop()
    }

    /// `read-only`：所有已知写入/执行工具与 terminal/diff 卡片直接拒绝，读类放行。
    pub fn read_only() -> Self {
        Self {
            deny: MUTATING_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            ask: Vec::new(),
            ask_terminal_and_diff: false,
            ask_everything: false,
            deny_terminal_and_diff: true,
        }
    }

    /// `danger-full-access`：全部放行（对齐 DSH `approval: never`）。
    pub fn danger_full_access() -> Self {
        Self {
            deny: Vec::new(),
            ask: Vec::new(),
            ask_terminal_and_diff: false,
            ask_everything: false,
            deny_terminal_and_diff: false,
        }
    }

    /// 按权限模式构造策略。
    pub fn for_mode(mode: PermissionMode) -> Self {
        match mode {
            PermissionMode::ReadOnly => Self::read_only(),
            PermissionMode::WorkspaceWrite => Self::workspace_write(),
            PermissionMode::DangerFullAccess => Self::danger_full_access(),
        }
    }

    /// 从 `DSHR_PERMISSION_MODE` 构造（缺省 workspace-write，默认行为不变）。
    pub fn from_env() -> Self {
        Self::for_mode(PermissionMode::from_env())
    }

    /// 全部工具都要求确认（自检/演示用）。
    pub fn ask_all() -> Self {
        Self {
            deny: Vec::new(),
            ask: Vec::new(),
            ask_terminal_and_diff: false,
            ask_everything: true,
            deny_terminal_and_diff: false,
        }
    }
}

impl ToolPolicy for ApprovalPolicy {
    fn check(&self, tool: &ToolDefinition, _arguments: &Value) -> PolicyDecision {
        if self.deny.iter().any(|name| name == &tool.name) {
            return PolicyDecision::Deny(format!("工具 {} 在拒绝名单中", tool.name));
        }
        if self.deny_terminal_and_diff
            && matches!(tool.card(), Presentation::Terminal | Presentation::Diff)
        {
            return PolicyDecision::Deny(format!("read-only 模式拒绝执行类工具 {}", tool.name));
        }
        if self.ask_everything {
            return PolicyDecision::Ask;
        }
        if self.ask.iter().any(|name| name == &tool.name) {
            return PolicyDecision::Ask;
        }
        if self.ask_terminal_and_diff
            && matches!(tool.card(), Presentation::Terminal | Presentation::Diff)
        {
            return PolicyDecision::Ask;
        }
        PolicyDecision::Allow
    }
}

/// 名字命中这些模式的工具默认需要用户确认。
pub const MUTATING_TOOL_NAMES: &[&str] = &[
    "write",
    "edit",
    "apply_patch",
    "applyPatch",
    "bash",
    "pwsh",
    "shell",
    "delete",
    "remove",
    "mkdir",
    "move",
    "copy",
    "rename",
    "chmod",
    "git_commit",
    "git_push",
];

/// 测试与演示用：按工具名拒绝。
#[derive(Debug, Default)]
pub struct DenyList(pub Vec<String>);

impl ToolPolicy for DenyList {
    fn check(&self, tool: &ToolDefinition, _arguments: &Value) -> PolicyDecision {
        if self.0.iter().any(|name| name == &tool.name) {
            PolicyDecision::Deny(format!("tool {} is denied by policy", tool.name))
        } else {
            PolicyDecision::Allow
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("tool `{0}` is already registered")]
    Duplicate(String),
    #[error("tool `{0}` is not registered")]
    NotFound(String),
    #[error("policy denied `{tool}`: {reason}")]
    Denied { tool: String, reason: String },
    #[error("tool `{tool}` needs approval before running")]
    NeedsApproval { tool: String },
    #[error("invalid arguments for `{tool}`: {reason}")]
    InvalidArguments { tool: String, reason: String },
}

pub struct ToolRegistry {
    tools: BTreeMap<String, ToolDefinition>,
    policy: Box<dyn ToolPolicy>,
}

impl ToolRegistry {
    pub fn new(policy: impl ToolPolicy + 'static) -> Self {
        Self {
            tools: BTreeMap::new(),
            policy: Box::new(policy),
        }
    }

    pub fn set_policy(&mut self, policy: impl ToolPolicy + 'static) {
        self.policy = Box::new(policy);
    }

    pub fn register(&mut self, definition: ToolDefinition) -> Result<(), RegistryError> {
        if self.tools.contains_key(&definition.name) {
            return Err(RegistryError::Duplicate(definition.name));
        }
        self.tools.insert(definition.name.clone(), definition);
        Ok(())
    }

    pub fn unregister(&mut self, name: &str) -> bool {
        self.tools.remove(name).is_some()
    }

    /// Sidecar 重启：丢弃该 generation 的全部注册，返回被移除的数量。
    pub fn invalidate_generation(&mut self, generation: &str) -> usize {
        let before = self.tools.len();
        self.tools
            .retain(|_, tool| tool.origin.generation() != Some(generation));
        before - self.tools.len()
    }

    pub fn get(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.get(name)
    }

    pub fn list(&self) -> Vec<&ToolDefinition> {
        self.tools.values().collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 调用前的唯一入口：先策略、再 schema，最后返回定义。
    pub fn authorize(
        &self,
        name: &str,
        arguments: &Value,
    ) -> Result<&ToolDefinition, RegistryError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| RegistryError::NotFound(name.to_owned()))?;

        self.authorize_definition(tool, arguments)
    }

    /// Apply this registry's policy to a definition resolved by the caller's scope.
    pub fn authorize_definition<'a>(
        &self,
        tool: &'a ToolDefinition,
        arguments: &Value,
    ) -> Result<&'a ToolDefinition, RegistryError> {
        let name = &tool.name;
        match self.policy.check(tool, arguments) {
            PolicyDecision::Allow => {}
            PolicyDecision::Ask => {
                return Err(RegistryError::NeedsApproval {
                    tool: name.to_owned(),
                })
            }
            PolicyDecision::Deny(reason) => {
                return Err(RegistryError::Denied {
                    tool: name.to_owned(),
                    reason,
                })
            }
        }

        Self::validate_definition(tool, arguments)?;
        Ok(tool)
    }

    /// 已经过策略审批后的二次入口：只做 schema 校验，不再问策略。
    ///
    /// 用于"Ask → 用户允许 → 继续执行"：策略已经放行一次，不能因为再次 authorize
    /// 又回到 NeedsApproval（那会变成死循环/永远无法执行）。
    pub fn authorize_approved(
        &self,
        name: &str,
        arguments: &Value,
    ) -> Result<&ToolDefinition, RegistryError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| RegistryError::NotFound(name.to_owned()))?;
        Self::validate_definition(tool, arguments)?;
        Ok(tool)
    }

    pub fn validate_definition(
        tool: &ToolDefinition,
        arguments: &Value,
    ) -> Result<(), RegistryError> {
        validate(&tool.input_schema, arguments).map_err(|reason| RegistryError::InvalidArguments {
            tool: tool.name.clone(),
            reason,
        })
    }
}

/// 一个故意做小的 JSON Schema 子集校验器：
/// 只支持 type / required / properties / additionalProperties / enum / items。
/// 完整 JSON Schema 在 M2 换成成熟实现（见 spec/README.md 的依赖策略）。
fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    let Some(object) = schema.as_object() else {
        return Ok(());
    };
    if object.is_empty() {
        return Ok(());
    }

    if let Some(expected) = object.get("type").and_then(Value::as_str) {
        let matches = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "number" => value.is_number(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "null" => value.is_null(),
            other => return Err(format!("unsupported schema type `{other}`")),
        };
        if !matches {
            return Err(format!("expected {expected}"));
        }
    }

    if let Some(allowed) = object.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            return Err(format!("value not in enum: {value}"));
        }
    }

    if let Value::Object(map) = value {
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !map.contains_key(name) {
                    return Err(format!("missing required property `{name}`"));
                }
            }
        }
        if let Some(properties) = object.get("properties").and_then(Value::as_object) {
            for (key, sub) in properties {
                if let Some(sub_value) = map.get(key) {
                    validate(sub, sub_value)?;
                }
            }
            if object.get("additionalProperties") == Some(&Value::Bool(false)) {
                for key in map.keys() {
                    if !properties.contains_key(key) {
                        return Err(format!("unexpected property `{key}`"));
                    }
                }
            }
        }
    }

    if let (Some(items), Some(array)) = (object.get("items"), value.as_array()) {
        for item in array {
            validate(items, item)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn native(name: &str) -> ToolDefinition {
        ToolDefinition::new(
            name,
            "native probe tool",
            ToolOrigin::Native {
                plugin_id: "builtin.probe".to_owned(),
            },
        )
    }

    fn sidecar(name: &str, generation: &str) -> ToolDefinition {
        ToolDefinition::new(
            name,
            "sidecar tool",
            ToolOrigin::Sidecar {
                plugin_id: "probe.plugin".to_owned(),
                registration_id: "reg-1".to_owned(),
                generation: generation.to_owned(),
            },
        )
    }

    #[test]
    fn rejects_duplicate_registration() {
        let mut registry = ToolRegistry::new(AllowAll);
        registry.register(native("probe_native")).unwrap();
        assert_eq!(
            registry.register(native("probe_native")).unwrap_err(),
            RegistryError::Duplicate("probe_native".to_owned())
        );
    }

    #[test]
    fn unregister_is_idempotent() {
        let mut registry = ToolRegistry::new(AllowAll);
        registry.register(native("a")).unwrap();
        assert!(registry.unregister("a"));
        assert!(!registry.unregister("a"));
        assert!(registry.is_empty());
    }

    #[test]
    fn generation_invalidation_drops_only_that_runtime() {
        let mut registry = ToolRegistry::new(AllowAll);
        registry.register(native("native_a")).unwrap();
        registry.register(sidecar("js_a", "node-0001")).unwrap();
        registry.register(sidecar("js_b", "node-0002")).unwrap();

        assert_eq!(registry.invalidate_generation("node-0001"), 1);
        assert!(registry.get("native_a").is_some());
        assert!(registry.get("js_a").is_none());
        assert!(registry.get("js_b").is_some());
    }

    #[test]
    fn policy_denies_before_schema_runs() {
        let mut registry = ToolRegistry::new(DenyList(vec!["probe_native".to_owned()]));
        registry
            .register(native("probe_native").with_schema(json!({ "type": "object" })))
            .unwrap();
        let err = registry.authorize("probe_native", &json!({})).unwrap_err();
        assert!(matches!(err, RegistryError::Denied { .. }));
    }

    #[test]
    fn missing_required_argument_is_rejected() {
        let mut registry = ToolRegistry::new(AllowAll);
        registry
            .register(native("probe_native").with_schema(json!({
                "type": "object",
                "required": ["path"],
                "properties": { "path": { "type": "string" } }
            })))
            .unwrap();
        let err = registry.authorize("probe_native", &json!({})).unwrap_err();
        assert!(matches!(err, RegistryError::InvalidArguments { .. }));
    }

    #[test]
    fn wrong_argument_type_is_rejected() {
        let mut registry = ToolRegistry::new(AllowAll);
        registry
            .register(native("probe_native").with_schema(json!({
                "type": "object",
                "properties": { "limit": { "type": "integer" } }
            })))
            .unwrap();
        let err = registry
            .authorize("probe_native", &json!({ "limit": "ten" }))
            .unwrap_err();
        assert!(matches!(err, RegistryError::InvalidArguments { .. }));
    }

    #[test]
    fn additional_properties_are_rejected_when_forbidden() {
        let mut registry = ToolRegistry::new(AllowAll);
        registry
            .register(native("probe_native").with_schema(json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "additionalProperties": false
            })))
            .unwrap();
        let err = registry
            .authorize("probe_native", &json!({ "path": "a", "extra": 1 }))
            .unwrap_err();
        assert!(matches!(err, RegistryError::InvalidArguments { .. }));
    }

    #[test]
    fn authorize_returns_definition_and_lists_sorted() {
        let mut registry = ToolRegistry::new(AllowAll);
        registry.register(native("b")).unwrap();
        registry.register(native("a")).unwrap();

        let tool = registry.authorize("a", &json!({})).unwrap();
        assert_eq!(tool.name, "a");
        let names: Vec<_> = registry.list().iter().map(|t| t.name.clone()).collect();
        assert_eq!(names, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn unknown_tool_is_reported() {
        let registry = ToolRegistry::new(AllowAll);
        assert_eq!(
            registry.authorize("nope", &json!({})).unwrap_err(),
            RegistryError::NotFound("nope".to_owned())
        );
    }

    #[test]
    fn presentation_round_trips_and_defaults_to_generic() {
        for card in Presentation::ALL {
            assert_eq!(Presentation::parse(card.as_str()), Some(card));
            assert_eq!(
                Presentation::parse(&card.as_str().to_uppercase()),
                Some(card)
            );
        }
        assert_eq!(Presentation::parse("markdown"), None);

        let plain = native("a");
        assert_eq!(plain.card(), Presentation::Generic);

        let terminal = native("b").with_presentation("terminal");
        assert_eq!(terminal.card(), Presentation::Terminal);
        assert_eq!(terminal.presentation.as_deref(), Some("terminal"));

        // 未知卡片不能丢掉结果：退回 Generic，原始字符串仍保留以便诊断。
        let unknown = native("c").with_presentation("markdown");
        assert_eq!(unknown.card(), Presentation::Generic);
        assert_eq!(unknown.presentation.as_deref(), Some("markdown"));
    }

    #[test]
    fn desktop_approval_policy_asks_for_mutating_tools() {
        let policy = ApprovalPolicy::desktop();
        let write = native("write");
        assert_eq!(policy.check(&write, &json!({})), PolicyDecision::Ask);

        let terminal = native("anything").with_presentation("terminal");
        assert_eq!(policy.check(&terminal, &json!({})), PolicyDecision::Ask);

        let read = native("read").with_presentation("read");
        assert_eq!(policy.check(&read, &json!({})), PolicyDecision::Allow);
    }

    #[test]
    fn deny_list_wins_over_ask_and_allow() {
        let policy = ApprovalPolicy {
            deny: vec!["bash".to_owned()],
            ask: vec!["bash".to_owned()],
            ask_terminal_and_diff: false,
            ask_everything: false,
            deny_terminal_and_diff: false,
        };
        assert!(matches!(
            policy.check(&native("bash"), &json!({})),
            PolicyDecision::Deny(_)
        ));
    }

    #[test]
    fn ask_everything_policy_asks_for_read_tools_too() {
        let policy = ApprovalPolicy::ask_all();
        assert_eq!(
            policy.check(&native("read"), &json!({})),
            PolicyDecision::Ask
        );
    }

    #[test]
    fn authorize_approved_skips_policy_but_keeps_schema_validation() {
        let mut registry = ToolRegistry::new(ApprovalPolicy::ask_all());
        registry
            .register(native("write").with_schema(json!({
                "type": "object",
                "required": ["path"],
                "properties": { "path": { "type": "string" } }
            })))
            .unwrap();
        assert!(matches!(
            registry.authorize("write", &json!({ "path": "a" })),
            Err(RegistryError::NeedsApproval { .. })
        ));
        assert!(registry
            .authorize_approved("write", &json!({ "path": "a" }))
            .is_ok());
        assert!(matches!(
            registry.authorize_approved("write", &json!({})),
            Err(RegistryError::InvalidArguments { .. })
        ));
    }

    // ------------------------------------------------------------------
    // M3-DEFAULT-050..056：approval 模式（read-only / danger-full-access）
    // ------------------------------------------------------------------

    #[test]
    fn m3_default_050_read_only_denies_mutating_tools() {
        let policy = ApprovalPolicy::read_only();
        for name in ["write", "edit", "bash", "pwsh", "delete", "git_push"] {
            assert!(
                matches!(
                    policy.check(&native(name), &json!({})),
                    PolicyDecision::Deny(_)
                ),
                "read-only 必须拒绝 {name}"
            );
        }
        // terminal/diff 卡片（执行类）同样拒绝，即使工具名不在名单里。
        for card in ["terminal", "diff"] {
            let tool = native("custom_exec").with_presentation(card);
            assert!(
                matches!(policy.check(&tool, &json!({})), PolicyDecision::Deny(_)),
                "read-only 必须拒绝 {card} 卡片"
            );
        }
    }

    #[test]
    fn m3_default_051_read_only_allows_read_tools() {
        let policy = ApprovalPolicy::read_only();
        assert_eq!(
            policy.check(&native("read").with_presentation("read"), &json!({})),
            PolicyDecision::Allow
        );
        assert_eq!(
            policy.check(&native("grep"), &json!({})),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn m3_default_052_danger_full_access_allows_everything() {
        let policy = ApprovalPolicy::danger_full_access();
        for name in ["write", "bash", "delete", "anything"] {
            assert_eq!(
                policy.check(&native(name), &json!({})),
                PolicyDecision::Allow,
                "danger-full-access 应放行 {name}"
            );
        }
        assert_eq!(
            policy.check(&native("exec").with_presentation("terminal"), &json!({})),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn m3_default_053_desktop_policy_is_unchanged_and_is_workspace_write() {
        let desktop = ApprovalPolicy::desktop();
        assert_eq!(
            desktop.check(&native("write"), &json!({})),
            PolicyDecision::Ask
        );
        assert_eq!(
            desktop.check(&native("read").with_presentation("read"), &json!({})),
            PolicyDecision::Allow
        );
        assert_eq!(
            ApprovalPolicy::workspace_write().check(&native("bash"), &json!({})),
            PolicyDecision::Ask
        );
        // from_env 缺省 = workspace-write（默认行为不回归）。
        assert_eq!(
            ApprovalPolicy::for_mode(PermissionMode::from_env())
                .check(&native("write"), &json!({})),
            PolicyDecision::Ask
        );
    }

    #[test]
    fn m3_default_054_permission_mode_parses_dsh_values() {
        assert_eq!(
            PermissionMode::parse("read-only"),
            Some(PermissionMode::ReadOnly)
        );
        assert_eq!(
            PermissionMode::parse("READ_ONLY"),
            Some(PermissionMode::ReadOnly)
        );
        assert_eq!(
            PermissionMode::parse("workspace-write"),
            Some(PermissionMode::WorkspaceWrite)
        );
        assert_eq!(
            PermissionMode::parse("danger-full-access"),
            Some(PermissionMode::DangerFullAccess)
        );
        assert_eq!(PermissionMode::parse("bogus"), None);
        assert_eq!(PermissionMode::ENV, "DSHR_PERMISSION_MODE");
        for mode in [
            PermissionMode::ReadOnly,
            PermissionMode::WorkspaceWrite,
            PermissionMode::DangerFullAccess,
        ] {
            assert_eq!(PermissionMode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn m3_default_055_approval_policy_for_mode_maps_constructors() {
        assert!(matches!(
            ApprovalPolicy::for_mode(PermissionMode::ReadOnly).check(&native("bash"), &json!({})),
            PolicyDecision::Deny(_)
        ));
        assert_eq!(
            ApprovalPolicy::for_mode(PermissionMode::DangerFullAccess)
                .check(&native("bash"), &json!({})),
            PolicyDecision::Allow
        );
        assert_eq!(
            ApprovalPolicy::for_mode(PermissionMode::WorkspaceWrite)
                .check(&native("bash"), &json!({})),
            PolicyDecision::Ask
        );
    }

    #[test]
    fn m3_default_056_env_mode_resolution_is_pure_for_tests() {
        // 环境读取只发生在 `from_env`；这里断言纯映射与默认回退，避免并行 env 干扰。
        assert_eq!(
            ApprovalPolicy::for_mode(PermissionMode::WorkspaceWrite)
                .check(&native("write"), &json!({})),
            PolicyDecision::Ask
        );
        assert!(PermissionMode::parse("").is_none());
    }

    // ------------------------------------------------------------------
    // M3-DEFAULT-060..063：per-tool timeout 注册侧透传
    // ------------------------------------------------------------------

    #[test]
    fn m3_default_060_tool_timeout_defaults_to_120s() {
        let tool = native("read");
        assert_eq!(tool.timeout_ms, DEFAULT_TOOL_TIMEOUT_MS);
        assert_eq!(tool.timeout_ms, 120_000);
        assert_eq!(tool.effective_timeout_ms(), 120_000);
    }

    #[test]
    fn m3_default_061_per_tool_timeout_is_preserved_by_registry() {
        let mut registry = ToolRegistry::new(AllowAll);
        registry
            .register(native("slow_tool").with_timeout_ms(45_000))
            .unwrap();
        let registered = registry.get("slow_tool").expect("registered");
        assert_eq!(registered.timeout_ms, 45_000, "注册表不得吞掉声明值");
        assert_eq!(registered.effective_timeout_ms(), 45_000);
    }

    #[test]
    fn m3_default_062_effective_timeout_is_clamped_to_spec_bounds() {
        assert_eq!(
            native("too_fast").with_timeout_ms(0).effective_timeout_ms(),
            TOOL_TIMEOUT_MIN_MS
        );
        assert_eq!(
            native("too_fast")
                .with_timeout_ms(500)
                .effective_timeout_ms(),
            TOOL_TIMEOUT_MIN_MS
        );
        assert_eq!(
            native("too_slow")
                .with_timeout_ms(TOOL_TIMEOUT_MAX_MS + 1)
                .effective_timeout_ms(),
            TOOL_TIMEOUT_MAX_MS
        );
        assert_eq!(TOOL_TIMEOUT_MIN_MS, 1_000);
        assert_eq!(TOOL_TIMEOUT_MAX_MS, 600_000);
    }

    #[test]
    fn m3_default_063_effective_timeout_duration_matches_ms() {
        let tool = native("slow").with_timeout_ms(2_500);
        assert_eq!(
            tool.effective_timeout(),
            std::time::Duration::from_millis(2_500)
        );
        assert_eq!(tool.effective_timeout().as_millis(), 2_500);
    }
}
