//! slot 目录（ADC-v1 §5 / §5.1）。
//!
//! `schema/adc-v1/slots.v1.json` 是唯一事实源：本模块在编译期用 `include_str!`
//! 嵌入该文件，运行时解析为 `SlotCatalog`。目录不合法时 `builtin()` 以明确信息
//! panic（属于打包错误，不是插件输入），插件输入一律返回结构化错误。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::pattern::matches_pattern;

const SLOTS_JSON: &str = include_str!("../../../schema/adc-v1/slots.v1.json");

/// slot 的冲突解决语义（spec §4.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SlotKind {
    /// 单席：后注册者替换前者（`exclusive` 时拒绝）。
    Single,
    /// 有序列表：`order` 升序，同值按 `registration_id` 升序。
    List,
    /// 按 key 分派：同 key 后注册者替换。
    Keyed,
    /// 链式管道：按 `order` 形成处理器链。
    Chain,
}

impl SlotKind {
    /// 稳定字符串形式（与目录 JSON 一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            SlotKind::Single => "single",
            SlotKind::List => "list",
            SlotKind::Keyed => "keyed",
            SlotKind::Chain => "chain",
        }
    }

    /// 解析目录中的 kind 字符串。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "single" => Some(SlotKind::Single),
            "list" => Some(SlotKind::List),
            "keyed" => Some(SlotKind::Keyed),
            "chain" => Some(SlotKind::Chain),
            _ => None,
        }
    }

    /// 该 kind 的注册签名是否要求 `id`。
    pub fn has_registration_id(self) -> bool {
        matches!(self, SlotKind::Single | SlotKind::List)
    }

    /// 该 kind 的注册签名是否要求 `key`。
    pub fn has_key(self) -> bool {
        self == SlotKind::Keyed
    }
}

/// slot 生命周期作用域（spec §4.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SlotScope {
    /// 应用级，全局一份。
    Root,
    /// 每个 Session 一份。
    Session,
    /// 同 `session`，但允许空态。
    SessionMaybe,
}

impl SlotScope {
    /// 稳定字符串形式。
    pub fn as_str(self) -> &'static str {
        match self {
            SlotScope::Root => "root",
            SlotScope::Session => "session",
            SlotScope::SessionMaybe => "session-maybe",
        }
    }

    /// 解析目录中的 scope 字符串。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "root" => Some(SlotScope::Root),
            "session" => Some(SlotScope::Session),
            "session-maybe" => Some(SlotScope::SessionMaybe),
            _ => None,
        }
    }
}

/// 替换风险（spec §5.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReplacementRisk {
    /// 无风险。
    None,
    /// 会遮挡宿主自带 UI。
    ShadowsShippedUi,
}

impl ReplacementRisk {
    /// 稳定字符串形式。
    pub fn as_str(self) -> &'static str {
        match self {
            ReplacementRisk::None => "none",
            ReplacementRisk::ShadowsShippedUi => "shadows-shipped-ui",
        }
    }

    /// 解析目录中的 replacement_risk 字符串。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "none" => Some(ReplacementRisk::None),
            "shadows-shipped-ui" => Some(ReplacementRisk::ShadowsShippedUi),
            _ => None,
        }
    }
}

/// keyed slot 的 key 域模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyDomainMode {
    /// 开放域：由对应注册表决定合法值。
    Open,
    /// 闭合域：合法值来自 Host 菜单目录，未知 key 拒绝。
    Closed,
}

impl KeyDomainMode {
    /// 稳定字符串形式。
    pub fn as_str(self) -> &'static str {
        match self {
            KeyDomainMode::Open => "open",
            KeyDomainMode::Closed => "closed",
        }
    }

    /// 解析目录中的 mode 字符串。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "open" => Some(KeyDomainMode::Open),
            "closed" => Some(KeyDomainMode::Closed),
            _ => None,
        }
    }
}

/// keyed slot 的 key 域声明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyDomain {
    /// open / closed。
    pub mode: KeyDomainMode,
    /// 闭合域的值来源（v1 只有 `host`）。
    pub source: Option<String>,
    /// 说明文字。
    pub doc: Option<String>,
}

/// 注册契约：必填与可选字段名（字段类型见目录 `field_types`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RegistrationContract {
    /// 必填字段。
    pub required: Vec<String>,
    /// 可选字段。
    pub optional: Vec<String>,
}

impl RegistrationContract {
    /// 该字段是否在契约内（必填或可选）。
    pub fn contains(&self, field: &str) -> bool {
        self.required.iter().any(|item| item == field)
            || self.optional.iter().any(|item| item == field)
    }

    /// 所有契约字段（必填在前）。
    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.required
            .iter()
            .chain(self.optional.iter())
            .map(String::as_str)
    }
}

/// 目录 `field_types` 中的一个字段类型。
#[derive(Debug, Clone, PartialEq)]
pub enum FieldTypeSpec {
    /// 字符串。
    String {
        /// 最小长度（字符数）。
        min_length: Option<usize>,
        /// 最大长度（字符数）。
        max_length: Option<usize>,
        /// 完整匹配的受限正则子集（见 `pattern` 模块）。
        pattern: Option<String>,
    },
    /// 数值。
    Number {
        /// 下界（含）。
        minimum: Option<f64>,
        /// 上界（含）。
        maximum: Option<f64>,
    },
    /// 枚举（字符串值）。
    Enum { values: Vec<String> },
    /// 字符串数组。
    StringArray {
        /// 最大元素数。
        max_items: Option<usize>,
        /// 单个元素必须匹配的模式。
        item_pattern: Option<String>,
    },
}

/// 目录 `kinds` 中一个 kind 的声明（用于目录自检）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KindSpec {
    /// 席位说明（`one` / `many` / `one-per-key` / `pipeline`）。
    pub seats: Option<String>,
    /// 该 kind 的注册签名必含字段。
    pub required_signature: Vec<String>,
    /// 冲突解决说明。
    pub conflict: Option<String>,
}

/// 一个 Contribution 分组（文档 / 权限归类）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContributionGroup {
    /// 说明。
    pub doc: Option<String>,
    /// 该分组包含的 slot。
    pub slots: Vec<String>,
}

/// 目录中的单个 slot。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSpec {
    /// slot 标识（点分小写，`command` 为历史短名）。
    pub slot_id: String,
    /// kind 语义。
    pub kind: SlotKind,
    /// 生命周期作用域。
    pub scope: SlotScope,
    /// 是否拒绝后注册者（仅影响 single 覆盖语义）。
    pub exclusive: bool,
    /// 替换风险。
    pub replacement_risk: ReplacementRisk,
    /// Contribution 分组名。
    pub contribution: String,
    /// 注册契约。
    pub registration: RegistrationContract,
    /// keyed slot 的 key 域；非 keyed 为 `None`。
    pub key_domain: Option<KeyDomain>,
    /// 目录附注。
    pub note: Option<String>,
}

/// 目录自检问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogIssue {
    /// 相关 slot（目录级问题为 `None`）。
    pub slot_id: Option<String>,
    /// 问题描述。
    pub message: String,
}

impl std::fmt::Display for CatalogIssue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.slot_id {
            Some(slot_id) => write!(formatter, "slot '{slot_id}': {}", self.message),
            None => write!(formatter, "{}", self.message),
        }
    }
}

/// 目录解析 / 自检错误。
#[derive(Debug, Error)]
pub enum CatalogError {
    /// JSON 解析失败。
    #[error("slot catalog JSON is invalid: {source}")]
    Parse {
        /// serde 错误。
        #[from]
        source: serde_json::Error,
    },
    /// slot 声明非法。
    #[error("slot '{slot_id}': {message}")]
    InvalidSlot {
        /// slot 标识。
        slot_id: String,
        /// 说明。
        message: String,
    },
    /// 字段类型声明非法。
    #[error("field type '{field}': {message}")]
    InvalidFieldType {
        /// 字段名。
        field: String,
        /// 说明。
        message: String,
    },
}

/// ADC-v1 slot 目录。
#[derive(Debug, Clone)]
pub struct SlotCatalog {
    catalog: Option<String>,
    version: u32,
    status: Option<String>,
    spec: Option<String>,
    kinds: BTreeMap<String, KindSpec>,
    scopes: BTreeMap<String, String>,
    contributions: BTreeMap<String, ContributionGroup>,
    field_types: BTreeMap<String, FieldTypeSpec>,
    slots: Vec<SlotSpec>,
    by_id: BTreeMap<String, usize>,
}

impl SlotCatalog {
    /// 解析一个目录 JSON（插件不能声明 slot，此入口主要供测试 / 自检 / 未来宿主加载）。
    pub fn from_json(json: &str) -> Result<Self, CatalogError> {
        let raw: RawCatalog = serde_json::from_str(json)?;
        let mut kinds = BTreeMap::new();
        for (name, raw_kind) in raw.kinds {
            kinds.insert(
                name,
                KindSpec {
                    seats: raw_kind.seats,
                    required_signature: raw_kind.required_signature,
                    conflict: raw_kind.conflict,
                },
            );
        }
        let mut field_types = BTreeMap::new();
        for (name, raw_field) in raw.field_types {
            field_types.insert(name, convert_field_type(raw_field)?);
        }
        let mut contributions = BTreeMap::new();
        for (name, raw_group) in raw.contributions {
            contributions.insert(
                name,
                ContributionGroup {
                    doc: raw_group.doc,
                    slots: raw_group.slots,
                },
            );
        }
        let mut slots = Vec::with_capacity(raw.slots.len());
        let mut by_id = BTreeMap::new();
        for raw_slot in raw.slots {
            let slot = convert_slot(raw_slot)?;
            if by_id.contains_key(&slot.slot_id) {
                return Err(CatalogError::InvalidSlot {
                    slot_id: slot.slot_id.clone(),
                    message: "duplicate slot_id in catalog".to_owned(),
                });
            }
            by_id.insert(slot.slot_id.clone(), slots.len());
            slots.push(slot);
        }
        Ok(Self {
            catalog: raw.catalog,
            version: raw.version,
            status: raw.status,
            spec: raw.spec,
            kinds,
            scopes: raw.scopes,
            contributions,
            field_types,
            slots,
            by_id,
        })
    }

    /// 编译期嵌入的 ADC-v1 目录。
    ///
    /// 解析失败意味着仓库打包错误（schema 与代码不同步），此时 panic 并给出明确信息；
    /// 该函数只应在启动期调用一次。
    pub fn builtin() -> &'static SlotCatalog {
        static CATALOG: OnceLock<SlotCatalog> = OnceLock::new();
        CATALOG.get_or_init(|| match Self::from_json(SLOTS_JSON) {
            Ok(catalog) => catalog,
            Err(error) => panic!(
                "ADC-v1 slot catalog schema/adc-v1/slots.v1.json is invalid: {error}. \
                 The catalog is compiled into dshr-ui-contributions; fix the schema file."
            ),
        })
    }

    /// 目录名（`adc.slots/v1`）。
    pub fn catalog_name(&self) -> Option<&str> {
        self.catalog.as_deref()
    }

    /// 目录版本（v1 = 1）。
    pub fn version(&self) -> u32 {
        self.version
    }

    /// 目录状态（Draft / Review / Frozen）。
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// 目录对应的 spec 锚点。
    pub fn spec(&self) -> Option<&str> {
        self.spec.as_deref()
    }

    /// 按 slot_id 查 slot。
    pub fn get(&self, slot_id: &str) -> Option<&SlotSpec> {
        self.by_id.get(slot_id).map(|index| &self.slots[*index])
    }

    /// 全部 slot（按目录顺序）。
    pub fn all(&self) -> &[SlotSpec] {
        &self.slots
    }

    /// slot 数量。
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// 目录是否为空。
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// 全部字段类型。
    pub fn field_types(&self) -> &BTreeMap<String, FieldTypeSpec> {
        &self.field_types
    }

    /// 目录声明的 scope 说明（`root` / `session` / `session-maybe`）。
    pub fn scopes(&self) -> &BTreeMap<String, String> {
        &self.scopes
    }

    /// 单个字段类型。
    pub fn field_type(&self, field: &str) -> Option<&FieldTypeSpec> {
        self.field_types.get(field)
    }

    /// 全部 Contribution 分组。
    pub fn contributions(&self) -> &BTreeMap<String, ContributionGroup> {
        &self.contributions
    }

    /// kind 声明。
    pub fn kind_spec(&self, kind: SlotKind) -> Option<&KindSpec> {
        self.kinds.get(kind.as_str())
    }

    /// 目录自检：目录 JSON 必须内部自洽（不校验 spec 表格，那由测试保证）。
    pub fn validate(&self) -> Result<(), Vec<CatalogIssue>> {
        let mut issues = Vec::new();
        if self.version != 1 {
            issues.push(CatalogIssue {
                slot_id: None,
                message: format!("unsupported catalog version {} (expected 1)", self.version),
            });
        }
        const SLOT_ID_DOC: &str = "^[a-z0-9]+(\\.[a-z0-9-]+)*$";
        let mut seen = BTreeSet::new();
        for slot in &self.slots {
            let slot_id = slot.slot_id.clone();
            if !seen.insert(slot_id.clone()) {
                issues.push(CatalogIssue {
                    slot_id: Some(slot_id),
                    message: "duplicate slot_id".to_owned(),
                });
                continue;
            }
            if !is_valid_slot_id(&slot_id) {
                issues.push(CatalogIssue {
                    slot_id: Some(slot_id.clone()),
                    message: format!("slot_id does not match {SLOT_ID_DOC}"),
                });
            }
            let mut contract_fields = BTreeSet::new();
            for field in slot.registration.fields() {
                if !contract_fields.insert(field.to_owned()) {
                    issues.push(CatalogIssue {
                        slot_id: Some(slot_id.clone()),
                        message: format!("field '{field}' is both required and optional"),
                    });
                }
                if !self.field_types.contains_key(field) {
                    issues.push(CatalogIssue {
                        slot_id: Some(slot_id.clone()),
                        message: format!("contract field '{field}' missing from field_types"),
                    });
                }
            }
            if let Some(kind_spec) = self.kind_spec(slot.kind) {
                for signature in &kind_spec.required_signature {
                    if !slot.registration.required.contains(signature) {
                        issues.push(CatalogIssue {
                            slot_id: Some(slot_id.clone()),
                            message: format!(
                                "kind '{}' requires signature field '{signature}' in registration.required",
                                slot.kind.as_str()
                            ),
                        });
                    }
                }
            }
            match (slot.kind, &slot.key_domain) {
                (SlotKind::Keyed, None) => issues.push(CatalogIssue {
                    slot_id: Some(slot_id.clone()),
                    message: "keyed slot must declare key_domain".to_owned(),
                }),
                (kind, Some(_)) if kind != SlotKind::Keyed => issues.push(CatalogIssue {
                    slot_id: Some(slot_id.clone()),
                    message: format!("{} slot must not declare key_domain", kind.as_str()),
                }),
                (SlotKind::Keyed, Some(domain))
                    if domain.mode == KeyDomainMode::Closed && domain.source.is_none() =>
                {
                    issues.push(CatalogIssue {
                        slot_id: Some(slot_id.clone()),
                        message: "closed key_domain must declare a source".to_owned(),
                    });
                }
                _ => {}
            }
            if !self.contributions.is_empty()
                && !self.contributions.contains_key(&slot.contribution)
            {
                issues.push(CatalogIssue {
                    slot_id: Some(slot_id.clone()),
                    message: format!("unknown contribution group '{}'", slot.contribution),
                });
            }
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }

    /// 按 `field_types` 校验单个字段值。
    pub fn check_field(&self, field: &str, value: &Value) -> Result<(), String> {
        let Some(spec) = self.field_types.get(field) else {
            return Err(format!("no field type declared for '{field}'"));
        };
        check_field_value(spec, value)
    }
}

fn convert_field_type(raw: RawFieldType) -> Result<FieldTypeSpec, CatalogError> {
    match raw {
        RawFieldType::String {
            min_length,
            max_length,
            pattern,
        } => Ok(FieldTypeSpec::String {
            min_length,
            max_length,
            pattern,
        }),
        RawFieldType::Number { minimum, maximum } => {
            if let (Some(min), Some(max)) = (minimum, maximum) {
                if min > max {
                    return Err(CatalogError::InvalidFieldType {
                        field: "<number>".to_owned(),
                        message: format!("minimum {min} > maximum {max}"),
                    });
                }
            }
            Ok(FieldTypeSpec::Number { minimum, maximum })
        }
        RawFieldType::Enum { values } => Ok(FieldTypeSpec::Enum { values }),
        RawFieldType::StringArray { max_items, items } => Ok(FieldTypeSpec::StringArray {
            max_items,
            item_pattern: items.and_then(|item| item.pattern),
        }),
    }
}

fn convert_slot(raw: RawSlot) -> Result<SlotSpec, CatalogError> {
    let kind = SlotKind::from_name(&raw.kind).ok_or_else(|| CatalogError::InvalidSlot {
        slot_id: raw.slot_id.clone(),
        message: format!("unknown kind '{}'", raw.kind),
    })?;
    let scope = SlotScope::from_name(&raw.scope).ok_or_else(|| CatalogError::InvalidSlot {
        slot_id: raw.slot_id.clone(),
        message: format!("unknown scope '{}'", raw.scope),
    })?;
    let replacement_risk = ReplacementRisk::from_name(&raw.replacement_risk).ok_or_else(|| {
        CatalogError::InvalidSlot {
            slot_id: raw.slot_id.clone(),
            message: format!("unknown replacement_risk '{}'", raw.replacement_risk),
        }
    })?;
    let key_domain = match raw.key_domain {
        None => None,
        Some(domain) => {
            let mode = KeyDomainMode::from_name(&domain.mode).ok_or_else(|| {
                CatalogError::InvalidSlot {
                    slot_id: raw.slot_id.clone(),
                    message: format!("unknown key_domain.mode '{}'", domain.mode),
                }
            })?;
            Some(KeyDomain {
                mode,
                source: domain.source,
                doc: domain.doc,
            })
        }
    };
    Ok(SlotSpec {
        slot_id: raw.slot_id,
        kind,
        scope,
        exclusive: raw.exclusive,
        replacement_risk,
        contribution: raw.contribution,
        registration: RegistrationContract {
            required: raw.registration.required,
            optional: raw.registration.optional,
        },
        key_domain,
        note: raw.note,
    })
}

fn check_field_value(spec: &FieldTypeSpec, value: &Value) -> Result<(), String> {
    match spec {
        FieldTypeSpec::String {
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
            if let Some(max) = max_length {
                if len > *max {
                    return Err(format!("string longer than maxLength {max}"));
                }
            }
            if let Some(pattern) = pattern {
                if !matches_pattern(pattern, text) {
                    return Err(format!("string does not match pattern {pattern}"));
                }
            }
            Ok(())
        }
        FieldTypeSpec::Number { minimum, maximum } => {
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
            Ok(())
        }
        FieldTypeSpec::Enum { values } => {
            let Some(text) = value.as_str() else {
                return Err("expected string enum value".to_owned());
            };
            if values.iter().any(|candidate| candidate == text) {
                Ok(())
            } else {
                Err(format!("value '{text}' is not one of {values:?}"))
            }
        }
        FieldTypeSpec::StringArray {
            max_items,
            item_pattern,
        } => {
            let Some(items) = value.as_array() else {
                return Err("expected string array".to_owned());
            };
            if let Some(max) = max_items {
                if items.len() > *max {
                    return Err(format!("array has more than maxItems {max} elements"));
                }
            }
            for (index, item) in items.iter().enumerate() {
                let Some(text) = item.as_str() else {
                    return Err(format!("item[{index}] is not a string"));
                };
                if let Some(pattern) = item_pattern {
                    if !matches_pattern(pattern, text) {
                        return Err(format!("item[{index}] does not match pattern {pattern}"));
                    }
                }
            }
            Ok(())
        }
    }
}

/// `slot-registration.schema.json` 的 slotId 形态：点分小写段，段内允许数字与连字符。
fn is_valid_slot_id(slot_id: &str) -> bool {
    !slot_id.is_empty()
        && slot_id.split('.').all(|segment| {
            !segment.is_empty()
                && segment.chars().all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
                })
        })
}

// ---------------------------------------------------------------------------
// 原始 JSON 结构
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawCatalog {
    #[serde(default)]
    catalog: Option<String>,
    version: u32,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    spec: Option<String>,
    #[serde(default)]
    kinds: BTreeMap<String, RawKind>,
    #[serde(default)]
    scopes: BTreeMap<String, String>,
    #[serde(default)]
    contributions: BTreeMap<String, RawContributionGroup>,
    field_types: BTreeMap<String, RawFieldType>,
    slots: Vec<RawSlot>,
}

#[derive(Debug, Deserialize)]
struct RawKind {
    #[serde(default)]
    seats: Option<String>,
    #[serde(default)]
    required_signature: Vec<String>,
    #[serde(default)]
    conflict: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawContributionGroup {
    #[serde(default)]
    doc: Option<String>,
    #[serde(default)]
    slots: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum RawFieldType {
    #[serde(rename = "string")]
    String {
        #[serde(default)]
        min_length: Option<usize>,
        #[serde(default)]
        max_length: Option<usize>,
        #[serde(default)]
        pattern: Option<String>,
    },
    #[serde(rename = "number")]
    Number {
        #[serde(default)]
        minimum: Option<f64>,
        #[serde(default)]
        maximum: Option<f64>,
    },
    #[serde(rename = "enum")]
    Enum { values: Vec<String> },
    #[serde(rename = "string[]")]
    StringArray {
        #[serde(default)]
        max_items: Option<usize>,
        #[serde(default)]
        items: Option<RawArrayItem>,
    },
}

#[derive(Debug, Deserialize)]
struct RawArrayItem {
    #[serde(default)]
    pattern: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSlot {
    slot_id: String,
    kind: String,
    scope: String,
    #[serde(default)]
    exclusive: bool,
    #[serde(default = "default_replacement_risk")]
    replacement_risk: String,
    contribution: String,
    registration: RawContract,
    #[serde(default)]
    key_domain: Option<RawKeyDomain>,
    #[serde(default)]
    note: Option<String>,
}

fn default_replacement_risk() -> String {
    "none".to_owned()
}

#[derive(Debug, Deserialize)]
struct RawContract {
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    optional: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawKeyDomain {
    mode: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    doc: Option<String>,
}
