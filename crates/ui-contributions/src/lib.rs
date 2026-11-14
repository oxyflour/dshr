//! ADC-v1 服务端：slot 注册表、Contribution 校验、元素词汇表。
//!
//! 本 crate 是 ADC-v1（Agent Desktop Client API）的**服务端**实现，也是渲染器
//! （`dshr-ui-gpui`）依赖的唯一契约层：
//!
//! - [`catalog`]：slot 目录。`schema/adc-v1/slots.v1.json` 在编译期嵌入，
//!   是 slot、kind、scope、注册契约与字段类型的唯一事实源（spec §5.1）。
//! - [`registry`]：`ContributionRegistry`。按 slot kind（single/list/keyed/chain）
//!   实现注册、撤销、分派、generation 失效与结构化拒绝（spec §4.4/§4.5）。
//! - [`element`]：元素词汇表与运行时降级。`parse_tree` 把插件 JSON 树解析为
//!   [`ElementNode`]，未知元素降级为 Placeholder（P4），并统一处理 props 归一化、
//!   动作校验、a11y 与配额（spec §6/§9/§10）。
//! - [`Diagnostic`]：注册与解析共用的结构化诊断。
//!
//! 本 crate 不修改 schema/docs，不做 I/O，不依赖网络；`#![forbid(unsafe_code)]`。

#![forbid(unsafe_code)]

mod diagnostic;
mod element_schema;
mod pattern;

pub mod catalog;
pub mod element;
pub mod registry;

pub use catalog::{
    CatalogError, CatalogIssue, ContributionGroup, FieldTypeSpec, KeyDomain, KeyDomainMode,
    KindSpec, RegistrationContract, ReplacementRisk, SlotCatalog, SlotKind, SlotScope, SlotSpec,
};
pub use diagnostic::{Diagnostic, DiagnosticCode, Severity};
pub use element::{
    parse_action, parse_tree, parse_tree_str, Action, ActionError, Align, ElementKind, ElementNode,
    Justify, ParsedTree, PlaceholderReason, TextStyle, Tone, MAX_DEPTH, MAX_NODES, MAX_TEXT_BYTES,
};
pub use registry::{
    ContributionRegistry, Generation, RegistrationError, RegistrationHandle, RegistrationOutcome,
    RegistrationRecord, ResolvedSlot,
};

/// ADC-v1 元素类型名全集（spec §6 顺序；26 个规范元素）。
pub const ELEMENT_TYPE_NAMES: [&str; 26] = [
    "Stack",
    "Row",
    "Grid",
    "Divider",
    "Spacer",
    "Text",
    "Markdown",
    "Code",
    "Badge",
    "Button",
    "Input",
    "Textarea",
    "Select",
    "Checkbox",
    "Switch",
    "Slider",
    "List",
    "Table",
    "Tree",
    "KeyValue",
    "Image",
    "Icon",
    "Chart",
    "Card",
    "Tabs",
    "Accordion",
];

/// ADC-v1 slot 数量（spec §5 的 12 个首批 slot）。
pub const SLOT_COUNT: usize = 12;
