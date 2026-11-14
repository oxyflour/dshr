//! 结构化诊断。
//!
//! ADC-v1 的每一步降级（未知元素、未知属性、非法动作、配额裁剪）都必须留下
//! 可机检、可读的诊断，而不是 panic 或静默丢弃。诊断同时用于注册表与元素解析。

use crate::element::PlaceholderReason;

/// 诊断严重级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    /// 提示（例如覆盖注册）。
    Info,
    /// 已降级但可继续渲染 / 继续运行。
    Warning,
    /// 拒绝该注册或该元素节点；调用方必须处理。
    Error,
}

impl Severity {
    /// 稳定字符串形式（用于日志与 conformance 报告）。
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// 诊断码。测试 ID 与 `docs/spec/adc-v1.md` §11 的 conformance 前缀对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum DiagnosticCode {
    // ---- 元素（ADC-ELEM-*） ----
    /// 未知元素类型，已降级为 `Placeholder`（P4）。
    UnknownElement,
    /// 节点缺少 `type` 或不是 JSON 对象。
    MissingType,
    /// 节点结构非法（`children` 不是数组等）。
    InvalidNode,
    /// 节点级未知字段（运行时忽略）。
    UnknownNodeField,
    /// 叶子元素带了 `children`（拒绝该字段，保留节点）。
    LeafWithChildren,
    /// 未知 props 键（已剔除）。
    UnknownProp,
    /// props 值类型 / 取值非法（已剔除）。
    InvalidProp,
    /// 必填 props 缺失（保留节点，渲染器用默认值）。
    MissingProp,
    /// 交互元素缺 `a11yLabel`（运行时拒绝标记，`ADC-ELEM-A11Y-001`）。
    A11yMissing,
    /// 动作非法，已降级为无动作。
    ActionDowngraded,
    /// 超过树深配额（已裁剪）。
    DepthExceeded,
    /// 超过节点数配额（已裁剪）。
    NodeBudgetExceeded,
    /// 文本超长被截断。
    TextTruncated,
    /// 其它 prop 数组/字符串超配额被截断。
    PropTruncated,
    /// 输入不是合法 JSON（`parse_tree_str`）。
    InvalidJson,
    // ---- 注册表（ADC-SLOT-*） ----
    /// 未知 slot。
    UnknownSlot,
    /// 缺少注册契约要求的必填字段。
    MissingField,
    /// 字段类型/枚举/正则不符。
    InvalidField,
    /// 同一作用域内注册 id 重复。
    DuplicateId,
    /// 注册被拒绝（exclusive / 权限 / 域限制）。
    Forbidden,
    /// 已有注册被后注册者替换（提示级）。
    Replaced,
    /// slot 目录自身不合法。
    InvalidCatalog,
}

impl DiagnosticCode {
    /// 稳定 snake_case 字符串（日志 / conformance 报告使用）。
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticCode::UnknownElement => "unknown_element",
            DiagnosticCode::MissingType => "missing_type",
            DiagnosticCode::InvalidNode => "invalid_node",
            DiagnosticCode::UnknownNodeField => "unknown_node_field",
            DiagnosticCode::LeafWithChildren => "leaf_with_children",
            DiagnosticCode::UnknownProp => "unknown_prop",
            DiagnosticCode::InvalidProp => "invalid_prop",
            DiagnosticCode::MissingProp => "missing_prop",
            DiagnosticCode::A11yMissing => "a11y_missing",
            DiagnosticCode::ActionDowngraded => "action_downgraded",
            DiagnosticCode::DepthExceeded => "depth_exceeded",
            DiagnosticCode::NodeBudgetExceeded => "node_budget_exceeded",
            DiagnosticCode::TextTruncated => "text_truncated",
            DiagnosticCode::PropTruncated => "prop_truncated",
            DiagnosticCode::InvalidJson => "invalid_json",
            DiagnosticCode::UnknownSlot => "unknown_slot",
            DiagnosticCode::MissingField => "missing_field",
            DiagnosticCode::InvalidField => "invalid_field",
            DiagnosticCode::DuplicateId => "duplicate_id",
            DiagnosticCode::Forbidden => "forbidden",
            DiagnosticCode::Replaced => "replaced",
            DiagnosticCode::InvalidCatalog => "invalid_catalog",
        }
    }
}

/// 一条结构化诊断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// 严重级别。
    pub severity: Severity,
    /// 诊断码。
    pub code: DiagnosticCode,
    /// 位置：元素树用 `$` / `$.children[0].props.value`，注册表用 `$.registry[slot_id]`。
    pub path: String,
    /// 人可读信息（不是稳定契约；稳定字段见 `code`/`path`）。
    pub message: String,
    /// 相关元素的 `testId`（Placeholder 的测试钩子，spec §9）。
    pub test_id: Option<String>,
}

impl Diagnostic {
    /// 构造一条无 `testId` 的诊断。
    pub fn new(
        severity: Severity,
        code: DiagnosticCode,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            code,
            path: path.into(),
            message: message.into(),
            test_id: None,
        }
    }

    /// 附上元素的 `testId`。
    pub fn with_test_id(mut self, test_id: impl Into<String>) -> Self {
        self.test_id = Some(test_id.into());
        self
    }

    /// 是否为 Error 级。
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl PlaceholderReason {
    /// 该 Placeholder 原因对应的诊断码。
    pub fn code(&self) -> DiagnosticCode {
        match self {
            PlaceholderReason::MissingType => DiagnosticCode::MissingType,
            PlaceholderReason::UnknownType { .. } => DiagnosticCode::UnknownElement,
            PlaceholderReason::InvalidNode { .. } => DiagnosticCode::InvalidNode,
            PlaceholderReason::InvalidProps { .. } => DiagnosticCode::InvalidProp,
            PlaceholderReason::DepthExceeded { .. } => DiagnosticCode::DepthExceeded,
            PlaceholderReason::NodeBudgetExceeded { .. } => DiagnosticCode::NodeBudgetExceeded,
        }
    }
}
