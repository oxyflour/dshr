//! ADC-v1 元素模型与运行时降级（spec §6 / §9 / §10 / P4）。
//!
//! `parse_tree` 把插件提交的声明式 JSON 树解析为 `ElementNode`：
//!
//! - 未知元素 → `ElementKind::Placeholder` + `PlaceholderReason::UnknownType`（保留
//!   原 type 字符串与 testId），**不 panic、不丢整棵树**（P4）；
//! - 叶子带 `children` → 拒绝该字段（保留节点）并诊断；
//! - 深/节点/文本配额 → 裁剪并诊断（`MAX_DEPTH` / `MAX_NODES` / `MAX_TEXT_BYTES`）；
//! - 未知 props → 忽略并诊断；非法动作 → 降级为无动作并诊断；
//! - 交互元素缺 `a11yLabel` → 节点保留 + `A11yMissing` 诊断（`ADC-ELEM-A11Y-001`）。
//!
//! 归一化保证：`node.props` 恒为 JSON object，且已剔除未知/非法 prop、已剔除非法
//! 动作、已按 schema 截断超长字符串；渲染器只需处理合法数据。

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use thiserror::Error;

use crate::diagnostic::{Diagnostic, DiagnosticCode, Severity};
use crate::element_schema::{ArrayRule, ElementSpec, ElementVocabulary, ObjectRule, PropRule};
use crate::pattern::matches_pattern;

/// 树深上限（含根节点；spec §6.1 / §10）。
pub const MAX_DEPTH: usize = 8;
/// 单次提交节点数上限（spec §6.1 / §10）。
pub const MAX_NODES: usize = 200;
/// 单段文本字节数上限（spec §6.1 / §10）。
pub const MAX_TEXT_BYTES: usize = 8192;

/// 28 个元素（26 个规范元素 + 未知元素降级占位）。
///
/// `#[non_exhaustive]`：新增元素属于 minor 变更，渲染器应保留兜底分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ElementKind {
    /// 纵向布局容器。
    Stack,
    /// 横向布局容器。
    Row,
    /// 网格布局容器。
    Grid,
    /// 分隔线。
    Divider,
    /// 弹性空白。
    Spacer,
    /// 文本。
    Text,
    /// Markdown（受控子集）。
    Markdown,
    /// 代码块。
    Code,
    /// 徽标。
    Badge,
    /// 按钮。
    Button,
    /// 单行输入。
    Input,
    /// 多行输入。
    Textarea,
    /// 下拉选择。
    Select,
    /// 复选框。
    Checkbox,
    /// 开关。
    Switch,
    /// 滑杆。
    Slider,
    /// 列表。
    List,
    /// 表格。
    Table,
    /// 树。
    Tree,
    /// 键值对。
    KeyValue,
    /// 图片（内容寻址附件）。
    Image,
    /// 图标。
    Icon,
    /// 图表。
    Chart,
    /// 卡片容器。
    Card,
    /// 标签页容器。
    Tabs,
    /// 手风琴容器。
    Accordion,
    /// 未知元素 / 非法节点的占位（P4）。
    Placeholder,
}

impl ElementKind {
    /// 规范元素类型名（`Placeholder` 返回 `"Placeholder"`）。
    pub fn as_str(self) -> &'static str {
        match self {
            ElementKind::Stack => "Stack",
            ElementKind::Row => "Row",
            ElementKind::Grid => "Grid",
            ElementKind::Divider => "Divider",
            ElementKind::Spacer => "Spacer",
            ElementKind::Text => "Text",
            ElementKind::Markdown => "Markdown",
            ElementKind::Code => "Code",
            ElementKind::Badge => "Badge",
            ElementKind::Button => "Button",
            ElementKind::Input => "Input",
            ElementKind::Textarea => "Textarea",
            ElementKind::Select => "Select",
            ElementKind::Checkbox => "Checkbox",
            ElementKind::Switch => "Switch",
            ElementKind::Slider => "Slider",
            ElementKind::List => "List",
            ElementKind::Table => "Table",
            ElementKind::Tree => "Tree",
            ElementKind::KeyValue => "KeyValue",
            ElementKind::Image => "Image",
            ElementKind::Icon => "Icon",
            ElementKind::Chart => "Chart",
            ElementKind::Card => "Card",
            ElementKind::Tabs => "Tabs",
            ElementKind::Accordion => "Accordion",
            ElementKind::Placeholder => "Placeholder",
        }
    }

    /// 解析规范元素类型名；未知返回 `None`（由调用方降级为 Placeholder）。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Stack" => Some(ElementKind::Stack),
            "Row" => Some(ElementKind::Row),
            "Grid" => Some(ElementKind::Grid),
            "Divider" => Some(ElementKind::Divider),
            "Spacer" => Some(ElementKind::Spacer),
            "Text" => Some(ElementKind::Text),
            "Markdown" => Some(ElementKind::Markdown),
            "Code" => Some(ElementKind::Code),
            "Badge" => Some(ElementKind::Badge),
            "Button" => Some(ElementKind::Button),
            "Input" => Some(ElementKind::Input),
            "Textarea" => Some(ElementKind::Textarea),
            "Select" => Some(ElementKind::Select),
            "Checkbox" => Some(ElementKind::Checkbox),
            "Switch" => Some(ElementKind::Switch),
            "Slider" => Some(ElementKind::Slider),
            "List" => Some(ElementKind::List),
            "Table" => Some(ElementKind::Table),
            "Tree" => Some(ElementKind::Tree),
            "KeyValue" => Some(ElementKind::KeyValue),
            "Image" => Some(ElementKind::Image),
            "Icon" => Some(ElementKind::Icon),
            "Chart" => Some(ElementKind::Chart),
            "Card" => Some(ElementKind::Card),
            "Tabs" => Some(ElementKind::Tabs),
            "Accordion" => Some(ElementKind::Accordion),
            _ => None,
        }
    }

    /// 是否允许节点级 `children`（spec §6.1）。
    pub fn allows_children(self) -> bool {
        matches!(
            self,
            ElementKind::Stack
                | ElementKind::Row
                | ElementKind::Grid
                | ElementKind::Card
                | ElementKind::Tabs
        )
    }

    /// 是否交互元素（运行时 MUST 提供 `a11yLabel`，spec §9 / Q4）。
    pub fn is_interactive(self) -> bool {
        matches!(
            self,
            ElementKind::Button
                | ElementKind::Input
                | ElementKind::Textarea
                | ElementKind::Select
                | ElementKind::Checkbox
                | ElementKind::Switch
                | ElementKind::Slider
                | ElementKind::List
                | ElementKind::Table
                | ElementKind::Tree
                | ElementKind::Tabs
        )
    }
}

/// 语义色（只允许主题 token，插件不能指定色值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Tone {
    /// 中性。
    Neutral,
    /// 信息。
    Info,
    /// 成功。
    Success,
    /// 警告。
    Warning,
    /// 错误。
    Error,
}

impl Tone {
    /// spec 取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Tone::Neutral => "neutral",
            Tone::Info => "info",
            Tone::Success => "success",
            Tone::Warning => "warning",
            Tone::Error => "error",
        }
    }

    /// 解析 spec 取值。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "neutral" => Some(Tone::Neutral),
            "info" => Some(Tone::Info),
            "success" => Some(Tone::Success),
            "warning" => Some(Tone::Warning),
            "error" => Some(Tone::Error),
            _ => None,
        }
    }
}

/// 交叉轴对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Align {
    /// 起点。
    Start,
    /// 居中。
    Center,
    /// 终点。
    End,
    /// 拉伸。
    Stretch,
}

impl Align {
    /// spec 取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Align::Start => "start",
            Align::Center => "center",
            Align::End => "end",
            Align::Stretch => "stretch",
        }
    }

    /// 解析 spec 取值。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "start" => Some(Align::Start),
            "center" => Some(Align::Center),
            "end" => Some(Align::End),
            "stretch" => Some(Align::Stretch),
            _ => None,
        }
    }
}

/// 主轴分布。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Justify {
    /// 起点。
    Start,
    /// 居中。
    Center,
    /// 终点。
    End,
    /// 两端分布。
    SpaceBetween,
}

impl Justify {
    /// spec 取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Justify::Start => "start",
            Justify::Center => "center",
            Justify::End => "end",
            Justify::SpaceBetween => "space-between",
        }
    }

    /// 解析 spec 取值。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "start" => Some(Justify::Start),
            "center" => Some(Justify::Center),
            "end" => Some(Justify::End),
            "space-between" => Some(Justify::SpaceBetween),
            _ => None,
        }
    }
}

/// 文本样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum TextStyle {
    /// 正文。
    Body,
    /// 说明文字。
    Caption,
    /// 标题。
    Title,
    /// 等宽。
    Mono,
}

impl TextStyle {
    /// spec 取值。
    pub fn as_str(self) -> &'static str {
        match self {
            TextStyle::Body => "body",
            TextStyle::Caption => "caption",
            TextStyle::Title => "title",
            TextStyle::Mono => "mono",
        }
    }

    /// 解析 spec 取值。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "body" => Some(TextStyle::Body),
            "caption" => Some(TextStyle::Caption),
            "title" => Some(TextStyle::Title),
            "mono" => Some(TextStyle::Mono),
            _ => None,
        }
    }
}

/// 动作（spec §7.3）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Action {
    /// 调用本插件的 Host 方法。
    Call {
        /// 方法名。
        method: String,
        /// 参数（缺省为空对象）。
        args: Value,
    },
    /// 触发已注册的 command。
    Command {
        /// command id。
        id: String,
    },
    /// 打开外部链接（仅 https）。
    OpenExternal {
        /// 已校验的 https URL。
        url: String,
    },
    /// 复制文本（≤ 64 KB）。
    Copy {
        /// 文本。
        text: String,
    },
}

/// 动作解析错误（非法动作会被降级为无动作并记 `ActionDowngraded` 诊断）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ActionError {
    /// 动作不是 JSON 对象。
    #[error("action must be a JSON object")]
    NotObject,
    /// 动作不是 call/command/openExternal/copy 中恰好一种。
    #[error("action must contain exactly one of call/command/openExternal/copy")]
    UnknownShape,
    /// `call` 方法名不合法。
    #[error("call method must be a non-empty string matching ^[A-Za-z0-9._-]+$")]
    InvalidCall,
    /// `call.args` 不是对象。
    #[error("call.args must be a JSON object")]
    InvalidArgs,
    /// `command` id 不合法。
    #[error("command id must be a string of 1..=128 characters")]
    InvalidCommand,
    /// 外链不是 https。
    #[error("openExternal must be an https:// URL without whitespace")]
    InvalidExternalUrl,
    /// copy 文本超长或不是字符串。
    #[error("copy must be a string of at most 64 KiB")]
    InvalidCopy,
    /// 动作对象含多余字段。
    #[error("action field '{field}' is not allowed for this action kind")]
    UnexpectedField {
        /// 多余字段名。
        field: String,
    },
}

/// 解析并校验一个动作值。
pub fn parse_action(value: &Value) -> Result<Action, ActionError> {
    const MAX_COPY_BYTES: usize = 65536;
    let Some(object) = value.as_object() else {
        return Err(ActionError::NotObject);
    };
    let kinds: Vec<&str> = ["call", "command", "openExternal", "copy"]
        .into_iter()
        .filter(|kind| object.contains_key(*kind))
        .collect();
    if kinds.len() != 1 {
        return Err(ActionError::UnknownShape);
    }
    let kind = kinds[0];
    let allowed: &[&str] = match kind {
        "call" => &["call", "args"],
        _ => &[kind],
    };
    for field in object.keys() {
        if !allowed.contains(&field.as_str()) {
            return Err(ActionError::UnexpectedField {
                field: field.clone(),
            });
        }
    }
    match kind {
        "call" => {
            let Some(method) = object.get("call").and_then(Value::as_str) else {
                return Err(ActionError::InvalidCall);
            };
            if method.is_empty() || !matches_pattern("^[A-Za-z0-9._-]+$", method) {
                return Err(ActionError::InvalidCall);
            }
            let args = match object.get("args") {
                None | Some(Value::Null) => Value::Object(Map::new()),
                Some(Value::Object(map)) => Value::Object(map.clone()),
                Some(_) => return Err(ActionError::InvalidArgs),
            };
            Ok(Action::Call {
                method: method.to_owned(),
                args,
            })
        }
        "command" => {
            let Some(id) = object.get("command").and_then(Value::as_str) else {
                return Err(ActionError::InvalidCommand);
            };
            let len = id.chars().count();
            if !(1..=128).contains(&len) {
                return Err(ActionError::InvalidCommand);
            }
            Ok(Action::Command { id: id.to_owned() })
        }
        "openExternal" => {
            let Some(url) = object.get("openExternal").and_then(Value::as_str) else {
                return Err(ActionError::InvalidExternalUrl);
            };
            if !matches_pattern("^https://[^\\s]+$", url) {
                return Err(ActionError::InvalidExternalUrl);
            }
            Ok(Action::OpenExternal {
                url: url.to_owned(),
            })
        }
        "copy" => {
            let Some(text) = object.get("copy").and_then(Value::as_str) else {
                return Err(ActionError::InvalidCopy);
            };
            if text.len() > MAX_COPY_BYTES {
                return Err(ActionError::InvalidCopy);
            }
            Ok(Action::Copy {
                text: text.to_owned(),
            })
        }
        _ => Err(ActionError::UnknownShape),
    }
}

/// Placeholder 的原因（P4 降级的可机检依据）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PlaceholderReason {
    /// 节点不是 JSON 对象，或缺少 `type`。
    MissingType,
    /// 未知元素类型（保留原 type 字符串，供 Placeholder 标题/诊断使用）。
    UnknownType {
        /// 原 type 字符串。
        type_name: String,
    },
    /// 节点结构非法（如未知节点的字段形状错误）。
    InvalidNode {
        /// 说明。
        detail: String,
    },
    /// `props` 不是 JSON 对象。
    InvalidProps {
        /// 说明。
        detail: String,
    },
    /// 超过树深配额。
    DepthExceeded {
        /// 实际深度。
        depth: usize,
        /// 允许的最大深度。
        max_depth: usize,
    },
    /// 超过节点数配额。
    NodeBudgetExceeded {
        /// 允许的最大节点数。
        max_nodes: usize,
    },
}

/// 一个已解析的 ADC-v1 元素节点。
#[derive(Debug, Clone, PartialEq)]
pub struct ElementNode {
    /// 元素类型（未知/非法节点为 `Placeholder`）。
    pub kind: ElementKind,
    /// 节点级 `key`（diff 用；不是 props）。
    pub key: Option<String>,
    /// 是否可见（默认 true）。
    pub visible: bool,
    /// `props.testId`（测试/截图回归）。
    pub test_id: Option<String>,
    /// `props.a11yLabel`（交互元素 MUST 提供）。
    pub a11y_label: Option<String>,
    /// `props.tone`。
    pub tone: Option<Tone>,
    /// `props.span`（Grid 子项跨列数 1..=12）。
    pub span: Option<u8>,
    /// 归一化后的 props（恒为 JSON object）。
    pub props: Value,
    /// 节点级 `children`（叶子恒为空）。
    pub children: Vec<ElementNode>,
    /// 嵌套节点容器：仅 `Accordion` 使用，key = section key，value = 该 section 的 children。
    pub nested_children: BTreeMap<String, Vec<ElementNode>>,
    /// 降级原因（`kind == Placeholder` 时存在）。
    pub placeholder: Option<PlaceholderReason>,
}

impl Default for ElementNode {
    fn default() -> Self {
        Self::placeholder(PlaceholderReason::MissingType)
    }
}

impl ElementNode {
    /// 构造一个空的合法节点（`visible=true`、空 props、无 children）。
    pub fn new(kind: ElementKind) -> Self {
        Self {
            kind,
            key: None,
            visible: true,
            test_id: None,
            a11y_label: None,
            tone: None,
            span: None,
            props: Value::Object(Map::new()),
            children: Vec::new(),
            nested_children: BTreeMap::new(),
            placeholder: None,
        }
    }

    /// 构造一个 Placeholder 节点。
    pub fn placeholder(reason: PlaceholderReason) -> Self {
        Self {
            kind: ElementKind::Placeholder,
            placeholder: Some(reason),
            ..Self::new(ElementKind::Placeholder)
        }
    }

    /// 是否容器元素（允许 `children`）。
    pub fn is_container(&self) -> bool {
        self.kind.allows_children()
    }

    /// 是否交互元素（MUST 提供 `a11yLabel`）。
    pub fn is_interactive(&self) -> bool {
        self.kind.is_interactive()
    }

    /// a11y 是否满足（非交互元素恒满足）。
    pub fn a11y_ok(&self) -> bool {
        !self.is_interactive() || self.a11y_label.is_some()
    }

    /// 读取原始 prop。
    pub fn prop(&self, name: &str) -> Option<&Value> {
        self.props.as_object()?.get(name)
    }

    /// 读取字符串 prop。
    pub fn str_prop(&self, name: &str) -> Option<&str> {
        self.prop(name)?.as_str()
    }

    /// 读取布尔 prop。
    pub fn bool_prop(&self, name: &str) -> Option<bool> {
        self.prop(name)?.as_bool()
    }

    /// 读取浮点 prop。
    pub fn f64_prop(&self, name: &str) -> Option<f64> {
        self.prop(name)?.as_f64()
    }

    /// 读取整数 prop。
    pub fn i64_prop(&self, name: &str) -> Option<i64> {
        self.prop(name)?.as_i64()
    }

    /// 读取数组 prop。
    pub fn array_prop(&self, name: &str) -> Option<&Vec<Value>> {
        self.prop(name)?.as_array()
    }

    /// 读取并校验动作 prop（如 `onPress` / `onChange` / `onSelect`）。
    pub fn action(&self, prop_name: &str) -> Option<Action> {
        parse_action(self.prop(prop_name)?).ok()
    }

    /// 深度优先遍历（含 `children` 与 `nested_children`）。
    pub fn walk(&self, function: &mut impl FnMut(&ElementNode)) {
        function(self);
        for child in &self.children {
            child.walk(function);
        }
        for section in self.nested_children.values() {
            for child in section {
                child.walk(function);
            }
        }
    }

    /// 深度优先遍历并附带深度（根为 1）。
    pub fn walk_with_depth(&self, function: &mut impl FnMut(&ElementNode, usize)) {
        fn walk(node: &ElementNode, depth: usize, function: &mut impl FnMut(&ElementNode, usize)) {
            function(node, depth);
            for child in &node.children {
                walk(child, depth + 1, function);
            }
            for section in node.nested_children.values() {
                for child in section {
                    walk(child, depth + 1, function);
                }
            }
        }
        walk(self, 1, function);
    }
}

/// `parse_tree` 的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTree {
    /// 根节点（输入再坏也会返回一个 Placeholder 根，不会 `None`/panic）。
    pub root: ElementNode,
    /// 结构化诊断（降级、裁剪、a11y 等）。
    pub diagnostics: Vec<Diagnostic>,
}

impl ParsedTree {
    /// 交互元素缺 `a11yLabel` 的诊断列表（运行时拒绝标记）。
    pub fn a11y_missing(&self) -> Vec<&Diagnostic> {
        self.diagnostics_with_code(DiagnosticCode::A11yMissing)
            .collect()
    }

    /// 指定诊断码的全部诊断。
    pub fn diagnostics_with_code(
        &self,
        code: DiagnosticCode,
    ) -> impl Iterator<Item = &Diagnostic> + '_ {
        self.diagnostics
            .iter()
            .filter(move |item| item.code == code)
    }

    /// 是否存在 Error 级诊断。
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }

    /// 完整节点数（含嵌套容器内容）。
    pub fn node_count(&self) -> usize {
        let mut count = 0usize;
        self.root.walk(&mut |_| count += 1);
        count
    }
}

/// 解析一棵 ADC-v1 元素树（从 `serde_json::Value`）。
///
/// 永不 panic、永不返回 `Err`：坏输入降级为 Placeholder 根并记录诊断。
pub fn parse_tree(value: &Value) -> ParsedTree {
    let mut parser = Parser::new();
    let root = parser.parse_node(value, "$", 1);
    ParsedTree {
        root,
        diagnostics: parser.diagnostics,
    }
}

/// 解析一棵 ADC-v1 元素树（从 JSON 文本；非法 JSON 降级为 Placeholder 根）。
pub fn parse_tree_str(json: &str) -> ParsedTree {
    match serde_json::from_str::<Value>(json) {
        Ok(value) => parse_tree(&value),
        Err(error) => {
            let reason = PlaceholderReason::InvalidNode {
                detail: format!("invalid JSON: {error}"),
            };
            ParsedTree {
                root: ElementNode::placeholder(reason),
                diagnostics: vec![Diagnostic::new(
                    Severity::Error,
                    DiagnosticCode::InvalidJson,
                    "$",
                    format!("element tree is not valid JSON: {error}"),
                )],
            }
        }
    }
}

struct Parser {
    vocabulary: &'static ElementVocabulary,
    diagnostics: Vec<Diagnostic>,
    node_count: usize,
}

impl Parser {
    fn new() -> Self {
        Self {
            vocabulary: ElementVocabulary::builtin(),
            diagnostics: Vec::new(),
            node_count: 0,
        }
    }

    fn push(
        &mut self,
        severity: Severity,
        code: DiagnosticCode,
        path: &str,
        message: impl Into<String>,
        test_id: Option<&str>,
    ) {
        let mut diagnostic = Diagnostic::new(severity, code, path.to_owned(), message);
        if let Some(test_id) = test_id {
            diagnostic = diagnostic.with_test_id(test_id);
        }
        self.diagnostics.push(diagnostic);
    }

    fn parse_node(&mut self, value: &Value, path: &str, depth: usize) -> ElementNode {
        if depth > MAX_DEPTH {
            let reason = PlaceholderReason::DepthExceeded {
                depth,
                max_depth: MAX_DEPTH,
            };
            let code = reason.code();
            self.push(
                Severity::Warning,
                code,
                path,
                format!("element depth {depth} exceeds MAX_DEPTH {MAX_DEPTH}; subtree clipped"),
                None,
            );
            return ElementNode::placeholder(reason);
        }
        if self.node_count >= MAX_NODES {
            let reason = PlaceholderReason::NodeBudgetExceeded {
                max_nodes: MAX_NODES,
            };
            let code = reason.code();
            self.push(
                Severity::Warning,
                code,
                path,
                format!("element count exceeds MAX_NODES {MAX_NODES}; subtree clipped"),
                None,
            );
            return ElementNode::placeholder(reason);
        }
        self.node_count += 1;

        let Some(object) = value.as_object() else {
            let reason = PlaceholderReason::InvalidNode {
                detail: "element node must be a JSON object".to_owned(),
            };
            self.push(
                Severity::Error,
                DiagnosticCode::InvalidNode,
                path,
                "element node must be a JSON object",
                None,
            );
            return ElementNode::placeholder(reason);
        };
        let Some(type_name) = object.get("type").and_then(Value::as_str) else {
            let reason = PlaceholderReason::MissingType;
            self.push(
                Severity::Error,
                DiagnosticCode::MissingType,
                path,
                "element node has no string 'type' field",
                None,
            );
            return ElementNode::placeholder(reason);
        };
        if type_name.is_empty() {
            let reason = PlaceholderReason::MissingType;
            self.push(
                Severity::Error,
                DiagnosticCode::MissingType,
                path,
                "element node has an empty 'type' field",
                None,
            );
            return ElementNode::placeholder(reason);
        }
        let Some(kind) = ElementKind::from_name(type_name) else {
            return self.parse_unknown(object, type_name, path, depth);
        };
        let Some(spec) = self.vocabulary.spec(type_name) else {
            return self.parse_unknown(object, type_name, path, depth);
        };
        self.parse_known(object, kind, spec, path, depth)
    }

    fn parse_unknown(
        &mut self,
        object: &Map<String, Value>,
        type_name: &str,
        path: &str,
        depth: usize,
    ) -> ElementNode {
        let common = lenient_common(object);
        let mut children = Vec::new();
        if let Some(children_value) = object.get("children") {
            children =
                self.parse_children(Some(children_value), &format!("{path}.children"), depth + 1);
        }
        let mut node = ElementNode::placeholder(PlaceholderReason::UnknownType {
            type_name: type_name.to_owned(),
        });
        node.key = common.key;
        node.visible = common.visible;
        node.test_id = common.test_id;
        node.a11y_label = common.a11y_label;
        node.tone = common.tone;
        node.span = common.span;
        node.props = object
            .get("props")
            .and_then(Value::as_object)
            .map(|map| Value::Object(map.clone()))
            .unwrap_or_else(|| Value::Object(Map::new()));
        node.children = children;
        self.push(
            Severity::Warning,
            DiagnosticCode::UnknownElement,
            path,
            format!("unknown element type '{type_name}'; degraded to Placeholder"),
            node.test_id.as_deref(),
        );
        node
    }

    fn parse_known(
        &mut self,
        object: &Map<String, Value>,
        kind: ElementKind,
        spec: &ElementSpec,
        path: &str,
        depth: usize,
    ) -> ElementNode {
        for field in object.keys() {
            if !matches!(field.as_str(), "type" | "key" | "props" | "children") {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::UnknownNodeField,
                    path,
                    format!("unknown node-level field '{field}' ignored"),
                    None,
                );
            }
        }
        let mut node = ElementNode::new(kind);
        node.key = parse_node_key(object, path, &mut self.diagnostics);
        let raw_props = match object.get("props") {
            None => Map::new(),
            Some(Value::Object(map)) => map.clone(),
            Some(_) => {
                let reason = PlaceholderReason::InvalidProps {
                    detail: "'props' must be a JSON object".to_owned(),
                };
                self.push(
                    Severity::Error,
                    DiagnosticCode::InvalidProp,
                    &format!("{path}.props"),
                    "'props' must be a JSON object; node degraded to Placeholder",
                    None,
                );
                let mut placeholder = ElementNode::placeholder(reason);
                placeholder.key = node.key;
                return placeholder;
            }
        };

        let normalized = if kind == ElementKind::Accordion {
            let mut raw_without_sections = raw_props.clone();
            let sections = raw_without_sections.remove("sections");
            let mut normalized = self.normalize_props(&spec.props, &raw_without_sections, path);
            let (normalized_sections, nested) =
                self.parse_accordion_sections(sections, path, depth);
            if let Some(sections) = normalized_sections {
                normalized.insert("sections".to_owned(), sections);
            }
            node.nested_children = nested;
            normalized
        } else {
            self.normalize_props(&spec.props, &raw_props, path)
        };

        let common = common_from_props(&normalized);
        node.visible = common.visible;
        node.test_id = common.test_id;
        node.a11y_label = common.a11y_label;
        node.tone = common.tone;
        node.span = common.span;

        for required in &spec.required_props {
            if !normalized.contains_key(required) {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::MissingProp,
                    path,
                    format!(
                        "required prop '{required}' is missing for {}",
                        kind.as_str()
                    ),
                    node.test_id.as_deref(),
                );
            }
        }

        let children_value = object.get("children");
        if spec.allows_children {
            node.children =
                self.parse_children(children_value, &format!("{path}.children"), depth + 1);
            if spec.requires_children && node.children.is_empty() {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::MissingProp,
                    path,
                    format!("{} requires at least one child", kind.as_str()),
                    node.test_id.as_deref(),
                );
            }
        } else if let Some(children_value) = children_value {
            if !children_value.is_null() {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::LeafWithChildren,
                    &format!("{path}.children"),
                    format!(
                        "leaf element {} must not have children; field ignored",
                        kind.as_str()
                    ),
                    node.test_id.as_deref(),
                );
            }
        }

        if kind.is_interactive() && node.a11y_label.is_none() {
            self.push(
                Severity::Error,
                DiagnosticCode::A11yMissing,
                path,
                format!(
                    "interactive element {} requires a11yLabel (ADC-ELEM-A11Y-001)",
                    kind.as_str()
                ),
                node.test_id.as_deref(),
            );
        }

        node.props = Value::Object(normalized);
        node
    }

    fn parse_children(
        &mut self,
        value: Option<&Value>,
        path: &str,
        child_depth: usize,
    ) -> Vec<ElementNode> {
        let Some(value) = value else {
            return Vec::new();
        };
        let Some(items) = value.as_array() else {
            self.push(
                Severity::Warning,
                DiagnosticCode::InvalidNode,
                path,
                "children must be a JSON array; ignored",
                None,
            );
            return Vec::new();
        };
        let mut children = Vec::with_capacity(items.len().min(MAX_NODES));
        for (index, item) in items.iter().enumerate() {
            if self.node_count >= MAX_NODES {
                // 配额裁剪：丢弃剩余兄弟节点并只记一次诊断（不为配额缺口伪造节点，
                // 保证树内实际节点数 ≤ MAX_NODES）。
                self.push(
                    Severity::Warning,
                    DiagnosticCode::NodeBudgetExceeded,
                    &format!("{path}[{index}]"),
                    format!(
                        "element count exceeds MAX_NODES {MAX_NODES}; remaining siblings clipped"
                    ),
                    None,
                );
                break;
            }
            children.push(self.parse_node(item, &format!("{path}[{index}]"), child_depth));
        }
        children
    }

    fn normalize_props(
        &mut self,
        rules: &BTreeMap<String, PropRule>,
        raw: &Map<String, Value>,
        path: &str,
    ) -> Map<String, Value> {
        let mut normalized = Map::new();
        for (name, value) in raw {
            let prop_path = format!("{path}.props.{name}");
            let Some(rule) = rules.get(name) else {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::UnknownProp,
                    &prop_path,
                    format!("unknown prop '{name}' ignored"),
                    None,
                );
                continue;
            };
            if matches!(rule, PropRule::Action) {
                match parse_action(value) {
                    Ok(_) => {
                        normalized.insert(name.clone(), value.clone());
                    }
                    Err(error) => {
                        self.push(
                            Severity::Warning,
                            DiagnosticCode::ActionDowngraded,
                            &prop_path,
                            format!("action prop '{name}' downgraded to no action: {error}"),
                            None,
                        );
                    }
                }
                continue;
            }
            match self.check_prop_value(rule, value, &prop_path) {
                Ok(Some(value)) => {
                    normalized.insert(name.clone(), value);
                }
                Ok(None) => {}
                Err(reason) => {
                    self.push(
                        Severity::Warning,
                        DiagnosticCode::InvalidProp,
                        &prop_path,
                        format!("prop '{name}' rejected: {reason}"),
                        None,
                    );
                }
            }
        }
        normalized
    }

    fn check_prop_value(
        &mut self,
        rule: &PropRule,
        value: &Value,
        path: &str,
    ) -> Result<Option<Value>, String> {
        match rule {
            PropRule::Bool => {
                if value.is_boolean() {
                    Ok(Some(value.clone()))
                } else {
                    Err("expected boolean".to_owned())
                }
            }
            PropRule::String {
                min_length,
                max_length,
                pattern,
            } => {
                let Some(text) = value.as_str() else {
                    return Err("expected string".to_owned());
                };
                let len = text.chars().count();
                if let Some(min) = min_length {
                    if len < *min {
                        return Err(format!("string shorter than minLength {min}"));
                    }
                }
                if let Some(pattern) = pattern {
                    if !matches_pattern(pattern, text) {
                        return Err(format!("string does not match pattern {pattern}"));
                    }
                }
                let Some(max) = max_length else {
                    return Ok(Some(value.clone()));
                };
                if *max == MAX_TEXT_BYTES {
                    if text.len() <= *max {
                        return Ok(Some(value.clone()));
                    }
                    let truncated = truncate_utf8(text, *max).to_owned();
                    self.push(
                        Severity::Warning,
                        DiagnosticCode::TextTruncated,
                        path,
                        format!(
                            "text is {} bytes, exceeds MAX_TEXT_BYTES {MAX_TEXT_BYTES}; truncated",
                            text.len()
                        ),
                        None,
                    );
                    return Ok(Some(Value::String(truncated)));
                }
                if len <= *max {
                    return Ok(Some(value.clone()));
                }
                let truncated = truncate_chars(text, *max).to_owned();
                self.push(
                    Severity::Warning,
                    DiagnosticCode::TextTruncated,
                    path,
                    format!("string length {len} exceeds maxLength {max}; truncated"),
                    None,
                );
                Ok(Some(Value::String(truncated)))
            }
            PropRule::Integer { minimum, maximum } => {
                let Some(number) = value
                    .as_i64()
                    .or_else(|| value.as_u64().and_then(|raw| i64::try_from(raw).ok()))
                else {
                    return Err("expected integer".to_owned());
                };
                let as_float = number as f64;
                if let Some(min) = minimum {
                    if as_float < *min {
                        return Err(format!("integer smaller than minimum {min}"));
                    }
                }
                if let Some(max) = maximum {
                    if as_float > *max {
                        return Err(format!("integer greater than maximum {max}"));
                    }
                }
                Ok(Some(value.clone()))
            }
            PropRule::Number { minimum, maximum } => {
                let Some(number) = value.as_f64() else {
                    return Err("expected number".to_owned());
                };
                if !number.is_finite() {
                    return Err("expected finite number".to_owned());
                }
                if let Some(min) = minimum {
                    if number < *min {
                        return Err(format!("number smaller than minimum {min}"));
                    }
                }
                if let Some(max) = maximum {
                    if number > *max {
                        return Err(format!("number greater than maximum {max}"));
                    }
                }
                Ok(Some(value.clone()))
            }
            PropRule::Enum(values) => {
                if values.iter().any(|candidate| candidate == value) {
                    Ok(Some(value.clone()))
                } else {
                    Err("value is not one of the allowed enum values".to_owned())
                }
            }
            PropRule::Object(object_rule) => {
                self.check_object(object_rule, value, path)?;
                Ok(Some(value.clone()))
            }
            PropRule::Array(array_rule) => self.check_array(array_rule, value, path),
            PropRule::Action | PropRule::Opaque => Ok(Some(value.clone())),
        }
    }

    fn check_object(&mut self, rule: &ObjectRule, value: &Value, path: &str) -> Result<(), String> {
        let Some(object) = value.as_object() else {
            return Err("expected object".to_owned());
        };
        for required in &rule.required {
            if !object.contains_key(required) {
                return Err(format!("missing field '{required}'"));
            }
        }
        for (name, nested) in object {
            match rule.props.get(name) {
                Some(nested_rule) => {
                    self.check_prop_value(nested_rule, nested, &format!("{path}.{name}"))?;
                }
                None if !rule.additional => {
                    return Err(format!("unexpected field '{name}'"));
                }
                None => {}
            }
        }
        Ok(())
    }

    fn check_array(
        &mut self,
        rule: &ArrayRule,
        value: &Value,
        path: &str,
    ) -> Result<Option<Value>, String> {
        let Some(items) = value.as_array() else {
            return Err("expected array".to_owned());
        };
        if let Some(min) = rule.min_items {
            if items.len() < min {
                return Err(format!("array has fewer than minItems {min} elements"));
            }
        }
        let mut normalized = items.clone();
        if let Some(max) = rule.max_items {
            if normalized.len() > max {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::PropTruncated,
                    path,
                    format!(
                        "array length {} exceeds maxItems {max}; truncated",
                        normalized.len()
                    ),
                    None,
                );
                normalized.truncate(max);
            }
        }
        if let Some(item_rule) = &rule.items {
            for (index, item) in normalized.iter().enumerate() {
                self.check_prop_value(item_rule, item, &format!("{path}[{index}]"))?;
            }
        }
        Ok(Some(Value::Array(normalized)))
    }

    /// 解析 Accordion 的 `sections`：返回归一化 `sections`（仅 key/title）与按 section key
    /// 分组的已解析 children。
    fn parse_accordion_sections(
        &mut self,
        value: Option<Value>,
        path: &str,
        depth: usize,
    ) -> (Option<Value>, BTreeMap<String, Vec<ElementNode>>) {
        const MAX_SECTIONS: usize = 12;
        const MAX_SECTION_CHILDREN: usize = 50;
        let Some(value) = value else {
            self.push(
                Severity::Warning,
                DiagnosticCode::MissingProp,
                &format!("{path}.props.sections"),
                "Accordion requires props.sections",
                None,
            );
            return (None, BTreeMap::new());
        };
        let Some(items) = value.as_array() else {
            self.push(
                Severity::Warning,
                DiagnosticCode::InvalidProp,
                &format!("{path}.props.sections"),
                "Accordion props.sections must be an array; ignored",
                None,
            );
            return (None, BTreeMap::new());
        };
        if items.is_empty() {
            self.push(
                Severity::Warning,
                DiagnosticCode::MissingProp,
                &format!("{path}.props.sections"),
                "Accordion requires at least one section",
                None,
            );
            return (None, BTreeMap::new());
        }
        let mut sections = Vec::new();
        let mut nested = BTreeMap::new();
        if items.len() > MAX_SECTIONS {
            self.push(
                Severity::Warning,
                DiagnosticCode::PropTruncated,
                &format!("{path}.props.sections"),
                format!("more than {MAX_SECTIONS} Accordion sections; extra sections dropped"),
                None,
            );
        }
        for (index, item) in items.iter().take(MAX_SECTIONS).enumerate() {
            let section_path = format!("{path}.props.sections[{index}]");
            let Some(section) = item.as_object() else {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::InvalidProp,
                    &section_path,
                    "Accordion section must be an object; dropped",
                    None,
                );
                continue;
            };
            let Some(key) = section.get("key").and_then(Value::as_str) else {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::InvalidProp,
                    &section_path,
                    "Accordion section requires a string 'key'; dropped",
                    None,
                );
                continue;
            };
            let key_len = key.chars().count();
            if !(1..=128).contains(&key_len) {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::InvalidProp,
                    &section_path,
                    "Accordion section key must be 1..=128 characters; dropped",
                    None,
                );
                continue;
            }
            if nested.contains_key(key) {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::InvalidProp,
                    &section_path,
                    format!("duplicate Accordion section key '{key}'; dropped"),
                    None,
                );
                continue;
            }
            let Some(title) = section.get("title").and_then(Value::as_str) else {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::InvalidProp,
                    &section_path,
                    "Accordion section requires a string 'title'; dropped",
                    None,
                );
                continue;
            };
            let title_len = title.chars().count();
            if !(1..=128).contains(&title_len) {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::InvalidProp,
                    &section_path,
                    "Accordion section title must be 1..=128 characters; dropped",
                    None,
                );
                continue;
            }
            let Some(children_value) = section.get("children") else {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::InvalidProp,
                    &section_path,
                    "Accordion section requires 'children'; section dropped",
                    None,
                );
                continue;
            };
            let Some(children_items) = children_value.as_array() else {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::InvalidProp,
                    &section_path,
                    "Accordion section children must be an array; section dropped",
                    None,
                );
                continue;
            };
            if children_items.len() > MAX_SECTION_CHILDREN {
                self.push(
                    Severity::Warning,
                    DiagnosticCode::PropTruncated,
                    &format!("{section_path}.children"),
                    format!(
                        "Accordion section has more than {MAX_SECTION_CHILDREN} children; truncated"
                    ),
                    None,
                );
            }
            let mut section_items = children_items.clone();
            section_items.truncate(MAX_SECTION_CHILDREN);
            let children_value = Value::Array(section_items);
            let children = self.parse_children(
                Some(&children_value),
                &format!("{section_path}.children"),
                depth + 1,
            );
            nested.insert(key.to_owned(), children);
            sections.push(Value::Object(Map::from_iter([
                ("key".to_owned(), Value::String(key.to_owned())),
                ("title".to_owned(), Value::String(title.to_owned())),
            ])));
        }
        if sections.is_empty() {
            (None, nested)
        } else {
            (Some(Value::Array(sections)), nested)
        }
    }
}

struct Common {
    key: Option<String>,
    visible: bool,
    test_id: Option<String>,
    a11y_label: Option<String>,
    tone: Option<Tone>,
    span: Option<u8>,
}

fn parse_node_key(
    object: &Map<String, Value>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    let value = object.get("key")?;
    let Some(key) = value.as_str() else {
        diagnostics.push(Diagnostic::new(
            Severity::Warning,
            DiagnosticCode::InvalidNode,
            format!("{path}.key"),
            "node key must be a string; ignored",
        ));
        return None;
    };
    let len = key.chars().count();
    if !(1..=128).contains(&len) {
        diagnostics.push(Diagnostic::new(
            Severity::Warning,
            DiagnosticCode::InvalidNode,
            format!("{path}.key"),
            "node key must be 1..=128 characters; ignored",
        ));
        return None;
    }
    Some(key.to_owned())
}

fn common_from_props(props: &Map<String, Value>) -> Common {
    Common {
        key: None,
        visible: props
            .get("visible")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        test_id: props
            .get("testId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        a11y_label: props
            .get("a11yLabel")
            .and_then(Value::as_str)
            .map(str::to_owned),
        tone: props
            .get("tone")
            .and_then(Value::as_str)
            .and_then(Tone::from_name),
        span: props
            .get("span")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok()),
    }
}

/// 未知元素的公共字段尽力提取（不信任 schema，但保留 testId 供 Placeholder 使用）。
fn lenient_common(object: &Map<String, Value>) -> Common {
    let props = object.get("props").and_then(Value::as_object);
    let key = object.get("key").and_then(Value::as_str).map(str::to_owned);
    Common {
        key,
        visible: props
            .and_then(|map| map.get("visible"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        test_id: props
            .and_then(|map| map.get("testId"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        a11y_label: props
            .and_then(|map| map.get("a11yLabel"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        tone: props
            .and_then(|map| map.get("tone"))
            .and_then(Value::as_str)
            .and_then(Tone::from_name),
        span: props
            .and_then(|map| map.get("span"))
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok()),
    }
}

fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn truncate_chars(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}
