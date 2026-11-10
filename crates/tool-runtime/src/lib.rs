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

pub use approval::{ApprovalBroker, ApprovalOutcome, ApprovalRecord, PendingApproval};

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
            timeout_ms: 120_000,
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
/// 判定顺序：显式 deny 名单 → 名字命中写入/执行名单 → 展示类型是 `terminal`/`diff`
/// → 其余 Allow。名单可通过 `MUTATING_TOOL_NAMES` 扩展，保持"默认拒绝破坏性操作"。
#[derive(Debug, Default)]
pub struct ApprovalPolicy {
    pub deny: Vec<String>,
    pub ask: Vec<String>,
    /// 是否把所有 `terminal` / `diff` 展示的工具都归入 Ask。
    pub ask_terminal_and_diff: bool,
    /// 无条件 Ask（自检/演示用）。
    pub ask_everything: bool,
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
        }
    }

    /// 全部工具都要求确认（自检/演示用）。
    pub fn ask_all() -> Self {
        Self {
            deny: Vec::new(),
            ask: Vec::new(),
            ask_terminal_and_diff: false,
            ask_everything: true,
        }
    }
}

impl ToolPolicy for ApprovalPolicy {
    fn check(&self, tool: &ToolDefinition, _arguments: &Value) -> PolicyDecision {
        if self.deny.iter().any(|name| name == &tool.name) {
            return PolicyDecision::Deny(format!("工具 {} 在拒绝名单中", tool.name));
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

        validate(&tool.input_schema, arguments).map_err(|reason| {
            RegistryError::InvalidArguments {
                tool: name.to_owned(),
                reason,
            }
        })?;
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
        validate(&tool.input_schema, arguments).map_err(|reason| {
            RegistryError::InvalidArguments {
                tool: name.to_owned(),
                reason,
            }
        })?;
        Ok(tool)
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
}
