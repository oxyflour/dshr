//! Contribution 注册表（ADC-v1 §4.4 / §4.5 / §5.1）。
//!
//! 一个注册表实例承载一个 scope 的注册（root 一份；session 每个会话一份）。
//! 注册载荷是 JSON（插件经 IPC 提交），注册表按目录做字段级校验，并按 slot kind
//! 实现注册 / 撤销 / 分派语义：
//!
//! - `single`：后注册者替换（`exclusive` 时拒绝）；
//! - `list`：按 `(order, registration_id)` 升序，同 id 重复拒绝；
//! - `keyed`：同 key 后注册者替换，闭合 key 域拒绝未知 key；
//! - `chain`：按 `order` 形成管道，`resolve_chain` 返回顺序（执行留给调用方）。
//!
//! 撤销幂等；generation 失效时该代全部注册按逆注册序撤销。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use thiserror::Error;

use crate::catalog::{KeyDomainMode, SlotCatalog, SlotKind};
use crate::diagnostic::{Diagnostic, DiagnosticCode, Severity};

/// 注册代（插件重载 / 会话切换时递增）。
pub type Generation = u64;

/// 宿主签发的注册句柄（单调递增，用于撤销）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegistrationHandle(u64);

impl RegistrationHandle {
    /// 数值形式（日志 / 调试）。
    pub fn value(self) -> u64 {
        self.0
    }
}

/// 一条已接受的注册。
#[derive(Debug, Clone, PartialEq)]
pub struct RegistrationRecord {
    /// 宿主句柄。
    pub handle: RegistrationHandle,
    /// 所属 slot。
    pub slot_id: String,
    /// 插件 ID（可选；由 IPC 层按 manifest 强制）。
    pub plugin_id: Option<String>,
    /// 插件声明的注册 ID（list/single 必填，keyed-view 可缺省）。
    pub registration_id: Option<String>,
    /// keyed slot 的分派键。
    pub key: Option<String>,
    /// list/chain 排序键（缺省 0）。
    pub order: f64,
    /// 注册所属 generation。
    pub generation: Generation,
    /// 已通过校验的原始字段。
    pub fields: BTreeMap<String, Value>,
    /// `view` 字段（contract 含 view 时存在）。
    pub view: Option<String>,
    /// `run` 字段（command / menu.item）。
    pub run: Option<String>,
    /// `label` 字段。
    pub label: Option<String>,
}

impl RegistrationRecord {
    /// 读取任意注册字段。
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.fields.get(name)
    }

    /// 稳定排序键：`(order, registration_id or "", handle)`。
    fn sort_key(&self) -> (f64, &str, u64) {
        (
            self.order,
            self.registration_id.as_deref().unwrap_or(""),
            self.handle.0,
        )
    }
}

/// 注册结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RegistrationOutcome {
    /// 新注册句柄。
    pub handle: RegistrationHandle,
    /// 被替换的旧注册（single / keyed 覆盖时）。
    pub replaced: Option<RegistrationHandle>,
    /// 覆盖诊断（`replaced` 为 `Some` 时存在）。
    pub diagnostic: Option<Diagnostic>,
}

/// 注册失败原因（结构化）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistrationError {
    /// slot 不在目录中，或插件试图声明新 slot。
    #[error("unknown slot '{slot_id}'")]
    UnknownSlot {
        /// slot 标识。
        slot_id: String,
    },
    /// 注册契约必填字段缺失。
    #[error("missing required field '{field}' for slot '{slot_id}'")]
    MissingField {
        /// slot 标识。
        slot_id: String,
        /// 字段名。
        field: String,
    },
    /// 字段类型 / 枚举 / 正则 / 取值范围不符，或出现契约外字段。
    #[error("invalid field '{field}' for slot '{slot_id}': {reason}")]
    InvalidField {
        /// slot 标识。
        slot_id: String,
        /// 字段名（契约外字段使用其自身名字，载荷级问题使用 `$`）。
        field: String,
        /// 可读原因。
        reason: String,
    },
    /// `list` / `chain` 同作用域内注册 id 重复。
    #[error("duplicate registration id '{id}' in slot '{slot_id}'")]
    DuplicateId {
        /// slot 标识。
        slot_id: String,
        /// 重复的注册 id。
        id: String,
    },
    /// 注册被策略拒绝（exclusive / 闭合 key 域不可配置等）。
    #[error("registration forbidden for slot '{slot_id}': {reason}")]
    Forbidden {
        /// slot 标识。
        slot_id: String,
        /// 可读原因。
        reason: String,
    },
    /// 覆盖注册的提示载体：成功路径不返回该错误，而是写入 `RegistrationOutcome`。
    /// 保留该 variant 供调用方按策略把覆盖升级为错误。
    #[error("slot '{slot_id}': '{previous_id}' was replaced by '{new_id}'")]
    Replaced {
        /// slot 标识。
        slot_id: String,
        /// 旧注册标识。
        previous_id: String,
        /// 新注册标识。
        new_id: String,
    },
}

impl RegistrationError {
    /// 对应的诊断码。
    pub fn code(&self) -> DiagnosticCode {
        match self {
            RegistrationError::UnknownSlot { .. } => DiagnosticCode::UnknownSlot,
            RegistrationError::MissingField { .. } => DiagnosticCode::MissingField,
            RegistrationError::InvalidField { .. } => DiagnosticCode::InvalidField,
            RegistrationError::DuplicateId { .. } => DiagnosticCode::DuplicateId,
            RegistrationError::Forbidden { .. } => DiagnosticCode::Forbidden,
            RegistrationError::Replaced { .. } => DiagnosticCode::Replaced,
        }
    }

    /// 严重级别（覆盖提示为 Warning，其余为 Error）。
    pub fn severity(&self) -> Severity {
        match self {
            RegistrationError::Replaced { .. } => Severity::Warning,
            _ => Severity::Error,
        }
    }

    /// slot 标识（载荷级错误可能为空）。
    pub fn slot_id(&self) -> &str {
        match self {
            RegistrationError::UnknownSlot { slot_id }
            | RegistrationError::MissingField { slot_id, .. }
            | RegistrationError::InvalidField { slot_id, .. }
            | RegistrationError::DuplicateId { slot_id, .. }
            | RegistrationError::Forbidden { slot_id, .. }
            | RegistrationError::Replaced { slot_id, .. } => slot_id,
        }
    }

    /// 转为结构化诊断。
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::new(
            self.severity(),
            self.code(),
            format!("$.registry[{}]", self.slot_id()),
            self.to_string(),
        )
    }
}

/// 解析后的 slot 分派结果。
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedSlot<'a> {
    /// `single`：0 或 1 条。
    Single(Option<&'a RegistrationRecord>),
    /// `list`：按 `(order, registration_id)` 升序。
    List(Vec<&'a RegistrationRecord>),
    /// `keyed`：按 key 升序。
    Keyed(Vec<(&'a str, &'a RegistrationRecord)>),
    /// `chain`：按 `order` 升序的处理器管道。
    Chain(Vec<&'a RegistrationRecord>),
}

/// 单个 scope 的 slot 状态。
#[derive(Debug, Clone, Default)]
enum SlotState {
    #[default]
    Empty,
    Single(Option<RegistrationHandle>),
    List(Vec<RegistrationHandle>),
    Keyed(BTreeMap<String, RegistrationHandle>),
    Chain(Vec<RegistrationHandle>),
}

/// Contribution 注册表。
#[derive(Debug, Clone)]
pub struct ContributionRegistry {
    catalog: SlotCatalog,
    slots: BTreeMap<String, SlotState>,
    records: BTreeMap<RegistrationHandle, RegistrationRecord>,
    next_handle: u64,
    diagnostics: Vec<Diagnostic>,
    closed_keys: BTreeMap<String, BTreeSet<String>>,
}

impl Default for ContributionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ContributionRegistry {
    /// 使用编译期嵌入的 ADC-v1 目录。
    pub fn new() -> Self {
        Self::with_catalog(SlotCatalog::builtin().clone())
    }

    /// 使用自定义目录（测试 / 宿主未来扩展 slot）。
    pub fn with_catalog(catalog: SlotCatalog) -> Self {
        Self {
            catalog,
            slots: BTreeMap::new(),
            records: BTreeMap::new(),
            next_handle: 0,
            diagnostics: Vec::new(),
            closed_keys: BTreeMap::new(),
        }
    }

    /// 当前目录。
    pub fn catalog(&self) -> &SlotCatalog {
        &self.catalog
    }

    /// 已记录的全部诊断（注册拒绝、覆盖提示等）。
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// 指定诊断码的诊断。
    pub fn diagnostics_with_code(
        &self,
        code: DiagnosticCode,
    ) -> impl Iterator<Item = &Diagnostic> + '_ {
        self.diagnostics
            .iter()
            .filter(move |item| item.code == code)
    }

    /// 清空诊断缓冲。
    pub fn clear_diagnostics(&mut self) {
        self.diagnostics.clear();
    }

    /// 取走诊断缓冲。
    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// 已接受的注册总数。
    pub fn registration_count(&self) -> usize {
        self.records.len()
    }

    /// 按句柄取注册。
    pub fn registration(&self, handle: RegistrationHandle) -> Option<&RegistrationRecord> {
        self.records.get(&handle)
    }

    /// 配置闭合 key 域（仅 closed keyed slot，合法值来自 Host 目录）。
    ///
    /// `menu.item` 的 key 域在目录中标记为 `closed` + `source=host`，具体菜单名
    /// 由宿主菜单目录在启动时注入；未注入时所有 key 均被拒绝。
    pub fn set_closed_key_domain(
        &mut self,
        slot_id: &str,
        keys: impl IntoIterator<Item = String>,
    ) -> Result<(), RegistrationError> {
        let spec = self
            .catalog
            .get(slot_id)
            .ok_or_else(|| RegistrationError::UnknownSlot {
                slot_id: slot_id.to_owned(),
            })?;
        if spec.kind != SlotKind::Keyed {
            return Err(RegistrationError::Forbidden {
                slot_id: slot_id.to_owned(),
                reason: "closed key domain is only meaningful for keyed slots".to_owned(),
            });
        }
        let is_closed = spec
            .key_domain
            .as_ref()
            .is_some_and(|domain| domain.mode == KeyDomainMode::Closed);
        if !is_closed {
            return Err(RegistrationError::Forbidden {
                slot_id: slot_id.to_owned(),
                reason: "key domain is open; closed key list cannot be configured".to_owned(),
            });
        }
        self.closed_keys
            .insert(slot_id.to_owned(), keys.into_iter().collect());
        Ok(())
    }

    /// 读取闭合 key 域（未配置时为 `None`）。
    pub fn closed_key_domain(&self, slot_id: &str) -> Option<&BTreeSet<String>> {
        self.closed_keys.get(slot_id)
    }

    /// 注册一个 Contribution（JSON 载荷，字段级校验见目录 `registration` / `field_types`）。
    pub fn register(
        &mut self,
        payload: &Value,
        generation: Generation,
    ) -> Result<RegistrationOutcome, RegistrationError> {
        let Some(object) = payload.as_object() else {
            return Err(self.reject(RegistrationError::InvalidField {
                slot_id: String::new(),
                field: "$".to_owned(),
                reason: "registration payload must be a JSON object".to_owned(),
            }));
        };
        let Some(slot_id_value) = object.get("slot_id") else {
            return Err(self.reject(RegistrationError::MissingField {
                slot_id: String::new(),
                field: "slot_id".to_owned(),
            }));
        };
        let Some(slot_id) = slot_id_value.as_str() else {
            return Err(self.reject(RegistrationError::InvalidField {
                slot_id: String::new(),
                field: "slot_id".to_owned(),
                reason: "slot_id must be a string".to_owned(),
            }));
        };
        if slot_id.is_empty() {
            return Err(self.reject(RegistrationError::InvalidField {
                slot_id: String::new(),
                field: "slot_id".to_owned(),
                reason: "slot_id must not be empty".to_owned(),
            }));
        }
        let Some(spec) = self.catalog.get(slot_id) else {
            return Err(self.reject(RegistrationError::UnknownSlot {
                slot_id: slot_id.to_owned(),
            }));
        };
        let spec = spec.clone();
        let slot_id = spec.slot_id.clone();

        let mut allowed: BTreeSet<&str> = BTreeSet::new();
        for field in spec.registration.fields() {
            allowed.insert(field);
        }
        allowed.insert("slot_id");
        allowed.insert("plugin_id");
        for key in object.keys() {
            if !allowed.contains(key.as_str()) {
                return Err(self.reject(RegistrationError::InvalidField {
                    slot_id: slot_id.clone(),
                    field: key.clone(),
                    reason: "unexpected field; not in slot registration contract".to_owned(),
                }));
            }
        }

        for field in &spec.registration.required {
            match object.get(field.as_str()) {
                None | Some(Value::Null) => {
                    return Err(self.reject(RegistrationError::MissingField {
                        slot_id: slot_id.clone(),
                        field: field.clone(),
                    }));
                }
                Some(_) => {}
            }
        }

        for field in spec.registration.fields() {
            if let Some(value) = object.get(field) {
                if let Err(reason) = self.catalog.check_field(field, value) {
                    return Err(self.reject(RegistrationError::InvalidField {
                        slot_id: slot_id.clone(),
                        field: field.to_owned(),
                        reason,
                    }));
                }
            }
        }
        if let Some(plugin_id) = object.get("plugin_id") {
            if let Err(reason) = self.catalog.check_field("plugin_id", plugin_id) {
                return Err(self.reject(RegistrationError::InvalidField {
                    slot_id: slot_id.clone(),
                    field: "plugin_id".to_owned(),
                    reason,
                }));
            }
        }

        if spec.kind == SlotKind::Keyed {
            if let Some(domain) = &spec.key_domain {
                if domain.mode == KeyDomainMode::Closed {
                    let key = object
                        .get("key")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let known = self
                        .closed_keys
                        .get(&slot_id)
                        .is_some_and(|domain| domain.contains(key));
                    if !known {
                        return Err(self.reject(RegistrationError::InvalidField {
                            slot_id: slot_id.clone(),
                            field: "key".to_owned(),
                            reason: format!(
                                "key '{key}' is not in the closed key domain of slot '{slot_id}'"
                            ),
                        }));
                    }
                }
            }
        }

        let registration_id = object.get("id").and_then(Value::as_str).map(str::to_owned);
        let key = object.get("key").and_then(Value::as_str).map(str::to_owned);
        let order = object.get("order").and_then(Value::as_f64).unwrap_or(0.0);
        let fields: BTreeMap<String, Value> = object
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        if spec.kind.has_registration_id() && registration_id.is_none() {
            // 目录 required 已保证存在，这里兜底防止目录写错。
            return Err(self.reject(RegistrationError::MissingField {
                slot_id: slot_id.clone(),
                field: "id".to_owned(),
            }));
        }
        if spec.kind.has_key() && key.is_none() {
            return Err(self.reject(RegistrationError::MissingField {
                slot_id: slot_id.clone(),
                field: "key".to_owned(),
            }));
        }

        // list / chain：同 id 重复拒绝（撤销幂等，重复注册不幂等）。
        if matches!(spec.kind, SlotKind::List | SlotKind::Chain) {
            if let Some(id) = &registration_id {
                let duplicate = self.slots.get(&slot_id).is_some_and(|state| match state {
                    SlotState::List(handles) | SlotState::Chain(handles) => {
                        handles.iter().any(|handle| {
                            self.records
                                .get(handle)
                                .and_then(|record| record.registration_id.as_ref())
                                == Some(id)
                        })
                    }
                    _ => false,
                });
                if duplicate {
                    return Err(self.reject(RegistrationError::DuplicateId {
                        slot_id: slot_id.clone(),
                        id: id.clone(),
                    }));
                }
            }
        }

        // exclusive single：先做策略检查，避免在持有 slot 状态借用时返回错误。
        if spec.kind == SlotKind::Single && spec.exclusive {
            let existing = match self.slots.get(&slot_id) {
                Some(SlotState::Single(Some(previous))) => Some(*previous),
                _ => None,
            };
            if let Some(previous) = existing {
                let previous_label = self.record_label(previous);
                return Err(self.reject(RegistrationError::Forbidden {
                    slot_id: slot_id.clone(),
                    reason: format!(
                        "slot is exclusive; existing registration '{previous_label}' is not replaceable"
                    ),
                }));
            }
        }

        let handle = RegistrationHandle(self.next_handle);
        self.next_handle += 1;
        let record = RegistrationRecord {
            handle,
            slot_id: slot_id.clone(),
            plugin_id: object
                .get("plugin_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            registration_id,
            key: key.clone(),
            order,
            generation,
            fields,
            view: object
                .get("view")
                .and_then(Value::as_str)
                .map(str::to_owned),
            run: object.get("run").and_then(Value::as_str).map(str::to_owned),
            label: object
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        // 先入 records，保证 list/chain 排序时能看到新注册。
        self.records.insert(handle, record);

        let mut replaced: Option<RegistrationHandle> = None;
        let mut state_error = false;
        match spec.kind {
            SlotKind::Single => {
                let state = self.slots.entry(slot_id.clone()).or_default();
                if matches!(state, SlotState::Empty) {
                    *state = SlotState::Single(None);
                }
                match state {
                    SlotState::Single(existing) => {
                        replaced = existing.replace(handle);
                    }
                    _ => state_error = true,
                }
            }
            SlotKind::List => {
                let state = self.slots.entry(slot_id.clone()).or_default();
                if matches!(state, SlotState::Empty) {
                    *state = SlotState::List(Vec::new());
                }
                match state {
                    SlotState::List(handles) => handles.push(handle),
                    _ => state_error = true,
                }
                if !state_error {
                    if let Some(SlotState::List(handles)) = self.slots.get_mut(&slot_id) {
                        Self::sort_handles(handles, &self.records);
                    }
                }
            }
            SlotKind::Keyed => {
                let state = self.slots.entry(slot_id.clone()).or_default();
                if matches!(state, SlotState::Empty) {
                    *state = SlotState::Keyed(BTreeMap::new());
                }
                match state {
                    SlotState::Keyed(map) => {
                        if let Some(key) = key.clone() {
                            replaced = map.insert(key, handle);
                        }
                    }
                    _ => state_error = true,
                }
            }
            SlotKind::Chain => {
                let state = self.slots.entry(slot_id.clone()).or_default();
                if matches!(state, SlotState::Empty) {
                    *state = SlotState::Chain(Vec::new());
                }
                match state {
                    SlotState::Chain(handles) => handles.push(handle),
                    _ => state_error = true,
                }
                if !state_error {
                    if let Some(SlotState::Chain(handles)) = self.slots.get_mut(&slot_id) {
                        Self::sort_handles(handles, &self.records);
                    }
                }
            }
        }

        if state_error {
            self.records.remove(&handle);
            return Err(self.reject(RegistrationError::InvalidField {
                slot_id: slot_id.clone(),
                field: "$".to_owned(),
                reason: "internal registry state mismatch".to_owned(),
            }));
        }
        if let Some(previous) = replaced {
            self.records.remove(&previous);
        }
        let diagnostic = replaced.map(|previous| {
            Diagnostic::new(
                Severity::Info,
                DiagnosticCode::Replaced,
                format!("$.registry[{slot_id}]"),
                format!(
                    "registration {} replaced registration {} in slot '{slot_id}'",
                    handle.value(),
                    previous.value()
                ),
            )
            .with_test_id(slot_id.clone())
        });
        if let Some(diagnostic) = &diagnostic {
            self.diagnostics.push(diagnostic.clone());
        }
        Ok(RegistrationOutcome {
            handle,
            replaced,
            diagnostic,
        })
    }

    /// 撤销单个注册；幂等（重复撤销返回 `false`，不报错）。
    pub fn unregister(&mut self, handle: RegistrationHandle) -> bool {
        let Some(record) = self.records.remove(&handle) else {
            return false;
        };
        let slot_id = record.slot_id;
        match self.slots.get_mut(&slot_id) {
            Some(SlotState::Single(existing)) => {
                if existing.is_some_and(|value| value == handle) {
                    *existing = None;
                }
            }
            Some(SlotState::List(handles)) | Some(SlotState::Chain(handles)) => {
                handles.retain(|candidate| *candidate != handle);
            }
            Some(SlotState::Keyed(map)) => {
                map.retain(|_, candidate| *candidate != handle);
            }
            Some(SlotState::Empty) | None => {}
        }
        true
    }

    /// 撤销某 generation 的全部注册，返回撤销数量（逆注册序）。
    pub fn invalidate_generation(&mut self, generation: Generation) -> usize {
        self.invalidate_generation_handles(generation).len()
    }

    /// 撤销某 generation 的全部注册，返回按逆注册序排列的句柄。
    pub fn invalidate_generation_handles(
        &mut self,
        generation: Generation,
    ) -> Vec<RegistrationHandle> {
        let mut handles: Vec<RegistrationHandle> = self
            .records
            .values()
            .filter(|record| record.generation == generation)
            .map(|record| record.handle)
            .collect();
        // 逆注册序：句柄单调递增，降序即后注册先撤销。
        handles.sort_unstable_by(|left, right| right.cmp(left));
        let mut removed = Vec::with_capacity(handles.len());
        for handle in handles {
            if self.unregister(handle) {
                removed.push(handle);
            }
        }
        removed
    }

    /// 解析 `single` slot。
    pub fn resolve_single(&self, slot_id: &str) -> Option<&RegistrationRecord> {
        match self.slots.get(slot_id)? {
            SlotState::Single(Some(handle)) => self.records.get(handle),
            _ => None,
        }
    }

    /// 解析 `list` slot（按 `(order, registration_id)` 升序）。
    pub fn resolve_list(&self, slot_id: &str) -> Vec<&RegistrationRecord> {
        match self.slots.get(slot_id) {
            Some(SlotState::List(handles)) => handles
                .iter()
                .filter_map(|handle| self.records.get(handle))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// 解析 `keyed` slot 的某个 key。
    pub fn resolve_keyed(&self, slot_id: &str, key: &str) -> Option<&RegistrationRecord> {
        match self.slots.get(slot_id)? {
            SlotState::Keyed(map) => map.get(key).and_then(|handle| self.records.get(handle)),
            _ => None,
        }
    }

    /// 解析 `chain` slot（按 order 升序的处理器管道）。
    pub fn resolve_chain(&self, slot_id: &str) -> Vec<&RegistrationRecord> {
        match self.slots.get(slot_id) {
            Some(SlotState::Chain(handles)) => handles
                .iter()
                .filter_map(|handle| self.records.get(handle))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// 解析任意 slot（按目录 kind 选择分发形态）。
    pub fn resolve(&self, slot_id: &str) -> Option<ResolvedSlot<'_>> {
        let spec = self.catalog.get(slot_id)?;
        match spec.kind {
            SlotKind::Single => Some(ResolvedSlot::Single(self.resolve_single(slot_id))),
            SlotKind::List => Some(ResolvedSlot::List(self.resolve_list(slot_id))),
            SlotKind::Keyed => {
                let entries = match self.slots.get(slot_id) {
                    Some(SlotState::Keyed(map)) => map
                        .iter()
                        .filter_map(|(key, handle)| {
                            self.records
                                .get(handle)
                                .map(|record| (key.as_str(), record))
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                Some(ResolvedSlot::Keyed(entries))
            }
            SlotKind::Chain => Some(ResolvedSlot::Chain(self.resolve_chain(slot_id))),
        }
    }

    /// 某 slot 的全部注册（list/chain 已排序；keyed 按 key 升序）。
    pub fn registrations_for(&self, slot_id: &str) -> Vec<&RegistrationRecord> {
        match self.catalog.get(slot_id).map(|spec| spec.kind) {
            Some(SlotKind::Single) => self.resolve_single(slot_id).into_iter().collect(),
            Some(SlotKind::List) => self.resolve_list(slot_id),
            Some(SlotKind::Chain) => self.resolve_chain(slot_id),
            Some(SlotKind::Keyed) => match self.resolve(slot_id) {
                Some(ResolvedSlot::Keyed(entries)) => {
                    entries.into_iter().map(|(_, record)| record).collect()
                }
                _ => Vec::new(),
            },
            None => Vec::new(),
        }
    }

    fn reject(&mut self, error: RegistrationError) -> RegistrationError {
        self.diagnostics.push(error.to_diagnostic());
        error
    }

    fn record_label(&self, handle: RegistrationHandle) -> String {
        match self.records.get(&handle) {
            Some(record) => record
                .registration_id
                .clone()
                .or_else(|| record.key.clone())
                .unwrap_or_else(|| format!("#{}", handle.value())),
            None => format!("#{}", handle.value()),
        }
    }

    fn sort_handles(
        handles: &mut [RegistrationHandle],
        records: &BTreeMap<RegistrationHandle, RegistrationRecord>,
    ) {
        handles.sort_by(|left, right| {
            let left_key = records.get(left).map(RegistrationRecord::sort_key);
            let right_key = records.get(right).map(RegistrationRecord::sort_key);
            match (left_key, right_key) {
                (Some(left_key), Some(right_key)) => left_key
                    .0
                    .total_cmp(&right_key.0)
                    .then_with(|| left_key.1.cmp(right_key.1))
                    .then_with(|| left_key.2.cmp(&right_key.2)),
                _ => left.cmp(right),
            }
        });
    }
}
