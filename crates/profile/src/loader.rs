//! Loader YAML 子集：bundle 条目列表 + patch 层 + `include`/group 展开。
//!
//! 只实现我们需要的方言（见 `cordis-composition-reference` 与 `docs/m2-loader.md`），
//! 不追求完整 YAML：
//! - 顶层条目列表：`- name: ...` / `id:` / `config:` / `inject:` / `disabled:` / `group:`；
//! - `include`：行级 `include: <path>`（字符串或数组）或真实 DSH 的
//!   `- name: cordis:include` + `config.path`/`config.paths`；也接受 bundle 顶层
//!   `include: [...]` 映射。相对路径基准是**声明它的文件所在目录**，递归展开；
//!   缺失文件/循环/非法路径都是 warning（不致命），`!!js` 不求值；
//! - `group`：`- group: true` + `config: [rows]`（真实 DSH 形状），或 `group: [rows]`；
//!   展开为顺序条目，嵌套深度上限 [`MAX_GROUP_DEPTH`]；
//! - patch 文件：每项是 `insert: [rows]`、`id` + `insert`（追加进同名 group）或带 `id`
//!   的覆盖映射（`config` 整体替换，不深合并；`name` 是断言而非重命名）；
//! - 标量支持字符串（引号）、布尔、整数/浮点、null、`[a, b]` 数组；`!!js` 视为字符串原文；
//! - 注释、空行、`---` 文档分隔符。
//!
//! 与真实 DSH Loader 的已知差异（详见任务报告）：`!!js` 不求值（按原文字符串透传，
//! `disabled: !!js` 一律按真值处理）、patch 的 include 无文件基准、`isolate`/`intercept`
//! 只保留不生效、`name` 相对路径不锚定 patch 文件。

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use crate::PluginEntry;

/// group 嵌套深度上限（防滥用；超过的部分跳过并告警）。
pub const MAX_GROUP_DEPTH: usize = 16;
/// include 链深度上限（防深链递归；超过的部分跳过并告警）。
pub const MAX_INCLUDE_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq)]
enum Yaml {
    Null,
    Bool(bool),
    Number(i64),
    Float(f64),
    Str(String),
    /// `!!js` 表达式原文：不求值，按字符串透传给上层（`!!js ` 之后的部分，含引号）。
    Js(String),
    List(Vec<Yaml>),
    Map(Vec<(String, Yaml)>),
}

/// bundle 里的一个条目（group 行的 `children` 是它的嵌套条目）。
#[derive(Debug, Clone, PartialEq)]
pub struct LoaderRow {
    pub id: Option<String>,
    pub name: Option<String>,
    pub group: bool,
    pub disabled: bool,
    pub inject: Option<Vec<String>>,
    pub config: Value,
    /// `include` 引用的相对路径（空表示普通条目）。
    pub includes: Vec<String>,
    /// group 行展开来源（`config: [rows]` / `group: [rows]` 解析结果）。
    pub children: Vec<LoaderRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowPatch {
    pub id: String,
    pub name: Option<String>,
    pub inject: Option<Vec<String>>,
    pub config: Option<Value>,
    pub disabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatchOp {
    /// `insert: [rows]`：追加到顶层条目末尾。
    Insert(Vec<LoaderRow>),
    /// `id` + `insert: [rows]`：追加进同名 `group: true` 行（真实 DSH 语义）。
    InsertInto { id: String, rows: Vec<LoaderRow> },
    /// 按 `id` 覆盖既有条目。
    Override(RowPatch),
}

/// 解析 bundle 文件（顶层条目列表或 `include` 映射）；`include` 保持未展开。
pub fn parse_bundle(yaml: &str) -> Result<Vec<LoaderRow>, String> {
    let document = parse_document(yaml)?;
    rows_from_document(document)
}

/// 解析 patch 文件（顶层是 patch 映射列表）。
pub fn parse_patch(yaml: &str) -> Result<Vec<PatchOp>, String> {
    let document = parse_document(yaml)?;
    parse_patch_document(document)
}

/// Read the subset of DSH row overrides that the headless host can apply.
pub(crate) fn parse_static_overrides(yaml: &str) -> Result<Vec<PatchOp>, String> {
    let document = parse_document(yaml)?;
    let items = match &document {
        Yaml::Null => return Ok(Vec::new()),
        Yaml::List(items) => items,
        _ => return Err("patch 顶层必须是列表".into()),
    };
    for item in items {
        let Yaml::Map(fields) = item else {
            return Err("patch 项必须是映射".into());
        };
        let id = fields
            .iter()
            .find_map(|(key, value)| match (key.as_str(), value) {
                ("id", Yaml::Str(id)) if !id.is_empty() => Some(id.as_str()),
                _ => None,
            })
            .ok_or("patch 条目需要字符串 id")?;
        let mut seen = std::collections::HashSet::new();
        for (key, value) in fields {
            if !seen.insert(key) {
                return Err(format!("条目 `{id}` 重复声明 `{key}`"));
            }
            if count_js_expressions(value) != 0 {
                return Err(format!("条目 `{id}` 的 `{key}` 不支持 !!js"));
            }
            match (key.as_str(), value) {
                ("id", Yaml::Str(_)) | ("disabled", Yaml::Bool(_)) => {}
                ("config", Yaml::Map(values)) => {
                    let mut keys = std::collections::HashSet::new();
                    for (name, _) in values {
                        if !keys.insert(name) {
                            return Err(format!("条目 `{id}` 的 config 重复声明 `{name}`"));
                        }
                    }
                }
                ("disabled" | "config", _) => {
                    return Err(format!("条目 `{id}` 的 `{key}` 类型错误"));
                }
                _ => return Err(format!("条目 `{id}` 不支持字段 `{key}`")),
            }
        }
    }
    parse_patch_document(document)
}

/// 应用 patch 层；返回 warning（目标不存在、group 目标不匹配等）。
pub fn apply_patch(rows: &mut Vec<LoaderRow>, ops: &[PatchOp]) -> Vec<String> {
    let mut warnings = Vec::new();
    for op in ops {
        match op {
            PatchOp::Insert(entries) => {
                rows.extend(entries.iter().cloned());
            }
            PatchOp::InsertInto { id, rows: entries } => {
                let Some(target) = find_row_mut(rows, id) else {
                    warnings.push(format!("patch insert 目标 `{id}` 不存在，已跳过"));
                    continue;
                };
                if !target.group {
                    warnings.push(format!("patch insert 目标 `{id}` 不是 group，已跳过"));
                    continue;
                }
                target.children.extend(entries.iter().cloned());
            }
            PatchOp::Override(patch) => {
                let Some(row) = find_row_mut(rows, &patch.id) else {
                    warnings.push(format!("patch 目标 `{}` 不存在，已跳过", patch.id));
                    continue;
                };
                if let Some(name) = &patch.name {
                    // 语义是「断言名字」而不是重命名；不一致时保留原值并告警。
                    if row.name.as_deref() != Some(name.as_str()) {
                        warnings.push(format!(
                            "patch `{}` 的 name 与现有条目不一致，已忽略",
                            patch.id
                        ));
                    }
                }
                if let Some(config) = &patch.config {
                    row.config = config.clone(); // config 整体替换，不深合并
                }
                if let Some(disabled) = patch.disabled {
                    row.disabled = disabled;
                }
                if let Some(inject) = &patch.inject {
                    row.inject = Some(inject.clone());
                }
            }
        }
    }
    warnings
}

/// bundle + patch → `PluginEntry`；不解析 `include`（没有文件基准，逐条告警）。
pub fn entries_from_loader(
    bundle: &str,
    patches: &[String],
) -> Result<(Vec<PluginEntry>, Vec<String>), String> {
    build_entries(bundle, None, patches)
}

/// 同 [`entries_from_loader`]，但 `include` 按 `base_dir` 解析（bundle 文本来自该目录）。
pub fn entries_from_loader_with_base(
    bundle: &str,
    base_dir: &Path,
    patches: &[String],
) -> Result<(Vec<PluginEntry>, Vec<String>), String> {
    build_entries(bundle, Some(base_dir), patches)
}

/// 从 bundle/patch 文件派生条目：`include` 基准目录 = 各声明文件所在目录。
pub fn entries_from_loader_files(
    bundle: &Path,
    patches: &[PathBuf],
) -> Result<(Vec<PluginEntry>, Vec<String>), String> {
    let base = bundle
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf);
    let bundle_text = fs::read_to_string(bundle)
        .map_err(|err| format!("读取 bundle {} 失败: {err}", bundle.display()))?;
    let mut patch_texts = Vec::new();
    for patch in patches {
        patch_texts.push(
            fs::read_to_string(patch)
                .map_err(|err| format!("读取 patch {} 失败: {err}", patch.display()))?,
        );
    }
    build_entries(&bundle_text, base.as_deref(), &patch_texts)
}

fn build_entries(
    bundle: &str,
    base: Option<&Path>,
    patch_texts: &[String],
) -> Result<(Vec<PluginEntry>, Vec<String>), String> {
    let document = parse_document(bundle)?;
    let mut diagnostics = Diagnostics::default();
    diagnostics.js_expressions += count_js_expressions(&document);

    let rows = rows_from_document(document)?;
    let mut rows = diagnostics.resolve_includes(rows, base, 0)?;

    let mut ops = Vec::new();
    for text in patch_texts {
        let (parsed, js_count) = parse_patch_with_js(text)?;
        diagnostics.js_expressions += js_count;
        ops.extend(parsed);
    }
    diagnostics.warnings.extend(apply_patch(&mut rows, &ops));

    let flat = diagnostics.flatten(&rows, 0);
    let entries = diagnostics.build_entries(flat);
    diagnostics.finish();
    Ok((entries, diagnostics.warnings))
}

/// 解析过程中的诊断与展开状态。
#[derive(Debug, Default)]
struct Diagnostics {
    warnings: Vec<String>,
    /// 正在展开的 include 文件（canonical 路径），用于循环检测。
    active_includes: Vec<PathBuf>,
    js_expressions: usize,
}

impl Diagnostics {
    /// 递归展开 include：返回保留 group 层级、但已把 include 行换成其内容的条目树。
    fn resolve_includes(
        &mut self,
        rows: Vec<LoaderRow>,
        base: Option<&Path>,
        include_depth: usize,
    ) -> Result<Vec<LoaderRow>, String> {
        let mut resolved = Vec::new();
        for row in rows {
            if !row.includes.is_empty() {
                for raw in &row.includes {
                    self.resolve_include(raw, base, include_depth, &mut resolved)?;
                }
                continue;
            }
            if row.group {
                let mut row = row;
                row.children =
                    self.resolve_includes(std::mem::take(&mut row.children), base, include_depth)?;
                resolved.push(row);
                continue;
            }
            resolved.push(row);
        }
        Ok(resolved)
    }

    fn resolve_include(
        &mut self,
        raw: &str,
        base: Option<&Path>,
        include_depth: usize,
        resolved: &mut Vec<LoaderRow>,
    ) -> Result<(), String> {
        let Some(base) = base else {
            self.warnings
                .push(format!("include `{raw}` 没有文件基准目录，已跳过"));
            return Ok(());
        };
        if include_depth >= MAX_INCLUDE_DEPTH {
            self.warnings.push(format!(
                "include 嵌套超过 {MAX_INCLUDE_DEPTH} 层，已跳过 `{raw}`"
            ));
            return Ok(());
        }
        let relative = match normalize_include_path(raw) {
            Ok(relative) => relative,
            Err(reason) => {
                self.warnings
                    .push(format!("跳过非法 include 路径 `{raw}`: {reason}"));
                return Ok(());
            }
        };
        let path = base.join(relative);
        let Ok(canonical) = path.canonicalize() else {
            self.warnings
                .push(format!("include `{}` 不存在，已跳过", path.display()));
            return Ok(());
        };
        if self.active_includes.contains(&canonical) {
            self.warnings
                .push(format!("include 循环，已跳过 `{}`", canonical.display()));
            return Ok(());
        }

        let text = fs::read_to_string(&canonical)
            .map_err(|err| format!("读取 include `{}` 失败: {err}", canonical.display()))?;
        let document = parse_include_document(&canonical, &text)
            .map_err(|err| format!("include `{}` 解析失败: {err}", canonical.display()))?;
        self.js_expressions += count_js_expressions(&document);
        let rows = rows_from_document(document)
            .map_err(|err| format!("include `{}`: {err}", canonical.display()))?;

        let include_base = canonical.parent().map(Path::to_path_buf);
        self.active_includes.push(canonical);
        let expanded = self.resolve_includes(rows, include_base.as_deref(), include_depth + 1);
        self.active_includes.pop();
        resolved.extend(expanded?);
        Ok(())
    }

    /// group 展平：按 bundle 顺序深度优先，group 行自身不产生条目。
    fn flatten(&mut self, rows: &[LoaderRow], group_depth: usize) -> Vec<LoaderRow> {
        let mut flat = Vec::new();
        for row in rows {
            if !row.includes.is_empty() {
                // bundle 侧 include 已在上一步展开；能走到这里的是 patch 插入的 include。
                self.warnings.push(format!(
                    "patch 插入的 include `{}` 没有文件基准目录，已跳过",
                    row.includes.join("`, `")
                ));
                continue;
            }
            if row.group {
                if group_depth >= MAX_GROUP_DEPTH {
                    self.warnings.push(format!(
                        "group 嵌套超过 {MAX_GROUP_DEPTH} 层，已跳过 `{}`",
                        row.name.as_deref().unwrap_or("")
                    ));
                    continue;
                }
                flat.extend(self.flatten(&row.children, group_depth + 1));
                continue;
            }
            flat.push(row.clone());
        }
        flat
    }

    fn build_entries(&mut self, flat: Vec<LoaderRow>) -> Vec<PluginEntry> {
        let mut entries = Vec::new();
        for row in flat {
            let Some(name) = row.name else {
                self.warnings.push(String::from("条目缺少 name，已跳过"));
                continue;
            };
            entries.push(PluginEntry {
                name,
                enabled: !row.disabled,
                config: row.config,
                inject: row.inject,
            });
        }
        entries
    }

    fn finish(&mut self) {
        if self.js_expressions > 0 {
            self.warnings.push(format!(
                "含 {} 处 `!!js` 表达式：Rust Loader 不求值（按原文字符串透传，`disabled` 按真值处理），求值语义仍在 Node/DSH 侧",
                self.js_expressions
            ));
        }
    }
}

fn find_row_mut<'a>(rows: &'a mut [LoaderRow], id: &str) -> Option<&'a mut LoaderRow> {
    for row in rows.iter_mut() {
        if row.id.as_deref() == Some(id) {
            return Some(row);
        }
        if let Some(found) = find_row_mut(&mut row.children, id) {
            return Some(found);
        }
    }
    None
}

fn rows_from_document(document: Yaml) -> Result<Vec<LoaderRow>, String> {
    match document {
        Yaml::Null => Ok(Vec::new()),
        Yaml::List(items) => interpret_rows(&items),
        Yaml::Map(pairs) => {
            // bundle 顶层映射形式：`include: path | [paths]`。
            let mut rows = Vec::new();
            for (key, value) in pairs {
                match key.as_str() {
                    "include" => rows.push(include_row(string_list(&value))),
                    other => {
                        return Err(format!(
                            "bundle 顶层映射只支持 `include` 字段，发现 `{other}`"
                        ))
                    }
                }
            }
            Ok(rows)
        }
        _ => Err(String::from("bundle 顶层必须是条目列表（- name: ...）")),
    }
}

fn include_row(paths: Vec<String>) -> LoaderRow {
    LoaderRow {
        id: None,
        name: None,
        group: false,
        disabled: false,
        inject: None,
        config: Value::Null,
        includes: paths,
        children: Vec::new(),
    }
}

fn interpret_rows(items: &[Yaml]) -> Result<Vec<LoaderRow>, String> {
    items.iter().map(interpret_row).collect()
}

fn interpret_row(item: &Yaml) -> Result<LoaderRow, String> {
    let Yaml::Map(pairs) = item else {
        return Err(String::from("bundle 条目必须是映射"));
    };
    let mut row = LoaderRow {
        id: None,
        name: None,
        group: false,
        disabled: false,
        inject: None,
        config: Value::Null,
        includes: Vec::new(),
        children: Vec::new(),
    };
    for (key, value) in pairs {
        match key.as_str() {
            "id" => row.id = Some(scalar_string(value)),
            "name" => row.name = Some(scalar_string(value)),
            "group" => {
                if let Yaml::List(items) = value {
                    row.group = true;
                    row.children = interpret_rows(items)?;
                } else {
                    row.group = truthy(value);
                }
            }
            "disabled" => row.disabled = truthy_or_defined(value),
            "inject" => row.inject = Some(string_list(value)),
            "config" => row.config = yaml_to_value(value),
            "include" => row.includes.extend(string_list(value)),
            "intercept" | "isolate" => {
                // 这些字段影响服务可见性/实例，当前宿主不实现；保留但不透传。
            }
            other => return Err(format!("bundle 条目不支持的字段 `{other}`")),
        }
    }

    // 真实 DSH 形状：`- name: cordis:include` + `config.path` / `config.paths`。
    if row.name.as_deref() == Some("cordis:include") {
        row.includes
            .extend(includes_from_include_config(&row.config)?);
    }
    // 真实 DSH 形状：group 行的 `config` 就是嵌套条目列表。
    if row.group && row.children.is_empty() {
        if let Value::Array(items) = &row.config {
            row.children = items
                .iter()
                .map(|item| interpret_row(&json_to_yaml(item)))
                .collect::<Result<Vec<_>, _>>()?;
        }
    }
    if !row.includes.is_empty() {
        if row.group {
            return Err(String::from("include 行不能同时声明 group"));
        }
        if let Some(name) = &row.name {
            if name != "cordis:include" {
                return Err(format!("include 行不能同时声明 name `{name}`"));
            }
        }
    }
    Ok(row)
}

fn includes_from_include_config(config: &Value) -> Result<Vec<String>, String> {
    match config {
        Value::String(path) => Ok(vec![path.clone()]),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| String::from("cordis:include 的 config 数组项必须是路径字符串"))
            })
            .collect(),
        Value::Object(map) => {
            let mut paths = Vec::new();
            if let Some(path) = map.get("path").and_then(Value::as_str) {
                paths.push(path.to_owned());
            }
            if let Some(items) = map.get("paths").and_then(Value::as_array) {
                for item in items {
                    paths.push(item.as_str().map(str::to_owned).ok_or_else(|| {
                        String::from("cordis:include 的 config.paths 项必须是路径字符串")
                    })?);
                }
            }
            if paths.is_empty() {
                return Err(String::from(
                    "cordis:include 需要 config.path 或 config.paths",
                ));
            }
            Ok(paths)
        }
        _ => Err(String::from(
            "cordis:include 的 config 必须是路径字符串、数组或含 path 的映射",
        )),
    }
}

fn parse_patch_with_js(yaml: &str) -> Result<(Vec<PatchOp>, usize), String> {
    let document = parse_document(yaml)?;
    let js_expressions = count_js_expressions(&document);
    Ok((parse_patch_document(document)?, js_expressions))
}

fn parse_patch_document(document: Yaml) -> Result<Vec<PatchOp>, String> {
    let items = match document {
        Yaml::Null => Vec::new(),
        Yaml::List(items) => items,
        other => vec![other],
    };
    let mut ops = Vec::new();
    for item in items {
        let Yaml::Map(pairs) = item else {
            return Err(String::from("patch 项必须是映射"));
        };
        let mut insert: Option<Vec<LoaderRow>> = None;
        let mut patch = RowPatch {
            id: String::new(),
            name: None,
            inject: None,
            config: None,
            disabled: None,
        };
        for (key, value) in pairs {
            match key.as_str() {
                "insert" => {
                    let Yaml::List(entries) = value else {
                        return Err(String::from("insert 必须是条目列表"));
                    };
                    insert = Some(interpret_rows(&entries)?);
                }
                "id" => patch.id = scalar_string(&value),
                "name" => patch.name = Some(scalar_string(&value)),
                "config" => patch.config = Some(yaml_to_value(&value)),
                "inject" => patch.inject = Some(string_list(&value)),
                "disabled" => patch.disabled = Some(truthy_or_defined(&value)),
                "group" => {
                    let _ = value;
                }
                other => {
                    return Err(format!("patch 不支持的字段 `{other}`"));
                }
            }
        }
        if let Some(rows) = insert {
            if patch.id.is_empty() {
                ops.push(PatchOp::Insert(rows));
            } else {
                ops.push(PatchOp::InsertInto { id: patch.id, rows });
            }
        } else if !patch.id.is_empty() {
            ops.push(PatchOp::Override(patch));
        } else {
            return Err(String::from("patch 既没有 insert 也没有 id"));
        }
    }
    Ok(ops)
}

fn truthy_or_defined(value: &Yaml) -> bool {
    match value {
        Yaml::Null => false,
        Yaml::Bool(value) => *value,
        Yaml::Str(text) | Yaml::Js(text) => !text.is_empty(),
        _ => true,
    }
}

fn truthy(value: &Yaml) -> bool {
    match value {
        Yaml::Null => false,
        Yaml::Bool(value) => *value,
        Yaml::Str(text) | Yaml::Js(text) => text == "true",
        _ => true,
    }
}

fn string_list(value: &Yaml) -> Vec<String> {
    match value {
        Yaml::List(items) => items.iter().map(scalar_string).collect(),
        other => vec![scalar_string(other)],
    }
}

fn scalar_string(value: &Yaml) -> String {
    match value {
        Yaml::Null => String::new(),
        Yaml::Bool(value) => value.to_string(),
        Yaml::Number(value) => value.to_string(),
        Yaml::Float(value) => value.to_string(),
        Yaml::Str(text) | Yaml::Js(text) => text.clone(),
        Yaml::List(_) | Yaml::Map(_) => String::new(),
    }
}

fn yaml_to_value(value: &Yaml) -> Value {
    match value {
        Yaml::Null => Value::Null,
        Yaml::Bool(value) => Value::Bool(*value),
        Yaml::Number(value) => json!(value),
        Yaml::Float(value) => json!(value),
        Yaml::Str(text) | Yaml::Js(text) => Value::String(text.clone()),
        Yaml::List(items) => Value::Array(items.iter().map(yaml_to_value).collect()),
        Yaml::Map(pairs) => {
            let mut object = serde_json::Map::new();
            for (key, value) in pairs {
                object.insert(key.clone(), yaml_to_value(value));
            }
            Value::Object(object)
        }
    }
}

fn json_to_yaml(value: &Value) -> Yaml {
    match value {
        Value::Null => Yaml::Null,
        Value::Bool(value) => Yaml::Bool(*value),
        Value::Number(number) => number
            .as_i64()
            .map(Yaml::Number)
            .or_else(|| number.as_f64().map(Yaml::Float))
            .unwrap_or(Yaml::Null),
        Value::String(text) => Yaml::Str(text.clone()),
        Value::Array(items) => Yaml::List(items.iter().map(json_to_yaml).collect()),
        Value::Object(map) => Yaml::Map(
            map.iter()
                .map(|(key, value)| (key.clone(), json_to_yaml(value)))
                .collect(),
        ),
    }
}

fn count_js_expressions(value: &Yaml) -> usize {
    match value {
        Yaml::Js(_) => 1,
        Yaml::List(items) => items.iter().map(count_js_expressions).sum(),
        Yaml::Map(pairs) => pairs
            .iter()
            .map(|(_, value)| count_js_expressions(value))
            .sum(),
        _ => 0,
    }
}

/// include 文件：`.json` 按 JSON 解析，其余按 YAML 子集解析。
fn parse_include_document(path: &Path, text: &str) -> Result<Yaml, String> {
    let is_json = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
    if !is_json {
        return parse_document(text);
    }
    let value: Value = serde_json::from_str(text).map_err(|err| format!("JSON 解析失败: {err}"))?;
    Ok(json_to_yaml(&value))
}

/// 校验并归一化 include 相对路径（与 `archive.rs` 的 tar 路径安全同一思路：
/// 拒绝绝对路径、`..` 逃逸、反斜杠、Windows 保留设备名）。
fn normalize_include_path(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err(String::from("路径为空"));
    }
    if raw.contains('\\') {
        return Err(format!("拒绝含反斜杠的路径: {raw}"));
    }
    let mut normalized = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => {
                if is_windows_reserved(part) {
                    return Err(format!("拒绝 Windows 保留设备名: {raw}"));
                }
                normalized.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("拒绝路径逃逸（..）: {raw}"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("拒绝绝对路径: {raw}"));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(format!("拒绝空路径: {raw}"));
    }
    Ok(normalized)
}

fn is_windows_reserved(part: &OsStr) -> bool {
    let Some(name) = part.to_str() else {
        return false;
    };
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit()
}

// ---------------------------------------------------------------------------
// 迷你 YAML 解析
// ---------------------------------------------------------------------------

struct Line {
    indent: usize,
    content: String,
}

fn parse_document(text: &str) -> Result<Yaml, String> {
    let mut raw = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = strip_comment(line);
        let trimmed = trimmed.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim() == "---" {
            continue;
        }
        let indent = trimmed.len() - trimmed.trim_start().len();
        raw.push((
            index + 1,
            Line {
                indent,
                content: trimmed.trim_start().to_owned(),
            },
        ));
    }
    if raw.is_empty() {
        return Ok(Yaml::Null);
    }
    let mut index = 0usize;
    let value = parse_block(&raw, &mut index, raw[0].1.indent)?;
    if index < raw.len() {
        return Err(format!("第 {} 行缩进无法解析", raw[index].0));
    }
    Ok(value)
}

fn strip_comment(line: &str) -> String {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        match ch {
            '\'' | '"' if quote.is_none() => {
                quote = Some(ch);
                out.push(ch);
            }
            '\'' | '"' if quote == Some(ch) => {
                quote = None;
                out.push(ch);
            }
            '#' if quote.is_none() => break,
            _ => out.push(ch),
        }
    }
    out
}

fn parse_block(raw: &[(usize, Line)], index: &mut usize, indent: usize) -> Result<Yaml, String> {
    if raw[*index].1.content.starts_with('-') {
        parse_list(raw, index, indent)
    } else {
        parse_map(raw, index, indent, None)
    }
}

fn parse_list(raw: &[(usize, Line)], index: &mut usize, indent: usize) -> Result<Yaml, String> {
    let mut items = Vec::new();
    while *index < raw.len() {
        let (_, line) = &raw[*index];
        if line.indent != indent || !line.content.starts_with('-') {
            break;
        }
        let rest = line.content[1..].trim_start();
        if rest.is_empty() {
            *index += 1;
            if *index < raw.len() && raw[*index].1.indent > indent {
                let nested_indent = raw[*index].1.indent;
                items.push(parse_block(raw, index, nested_indent)?);
            } else {
                items.push(Yaml::Null);
            }
            continue;
        }
        if let Some((key, value)) = split_pair(rest) {
            // 列表项是映射：首个键在 `-` 行，后续键缩进 +2。
            let initial = (key, value);
            let map = parse_map(raw, index, indent + 2, Some(initial))?;
            items.push(map);
        } else {
            items.push(parse_scalar(rest)?);
            *index += 1;
        }
    }
    Ok(Yaml::List(items))
}

fn parse_map(
    raw: &[(usize, Line)],
    index: &mut usize,
    indent: usize,
    initial: Option<(String, String)>,
) -> Result<Yaml, String> {
    let mut pairs = Vec::new();
    if let Some((key, value)) = initial {
        // 首行（`- key: value`）已经消费。
        *index += 1;
        let parsed = if value.is_empty() {
            if *index < raw.len() && raw[*index].1.indent > indent - 1 {
                let nested = raw[*index].1.indent;
                parse_block(raw, index, nested)?
            } else {
                Yaml::Null
            }
        } else {
            parse_scalar(&value)?
        };
        pairs.push((key, parsed));
    }
    while *index < raw.len() {
        let (line_number, line) = &raw[*index];
        if line.indent != indent || line.content.starts_with('-') {
            break;
        }
        let Some((key, value)) = split_pair(&line.content) else {
            return Err(format!("第 {line_number} 行不是 key: value"));
        };
        *index += 1;
        let parsed = if value.is_empty() {
            if *index < raw.len() && raw[*index].1.indent > indent {
                let nested = raw[*index].1.indent;
                parse_block(raw, index, nested)?
            } else {
                Yaml::Null
            }
        } else {
            parse_scalar(&value)?
        };
        pairs.push((key, parsed));
    }
    Ok(Yaml::Map(pairs))
}

fn split_pair(content: &str) -> Option<(String, String)> {
    let mut quote: Option<char> = None;
    for (offset, ch) in content.char_indices() {
        match ch {
            '\'' | '"' if quote.is_none() => quote = Some(ch),
            '\'' | '"' if quote == Some(ch) => quote = None,
            ':' if quote.is_none() => {
                let key = content[..offset].trim();
                if key.is_empty() {
                    return None;
                }
                let value = content[offset + 1..].trim();
                return Some((key.to_owned(), value.to_owned()));
            }
            _ => {}
        }
    }
    None
}

fn parse_scalar(text: &str) -> Result<Yaml, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Yaml::Null);
    }
    if let Some(rest) = text.strip_prefix("!!js ") {
        // 不求值：保留表达式原文（含引号），由上层决定如何处理。
        return Ok(Yaml::Js(rest.trim().to_owned()));
    }
    if text == "{}" {
        return Ok(Yaml::Map(Vec::new()));
    }
    if text.starts_with('[') && text.ends_with(']') {
        let inner = &text[1..text.len() - 1];
        let items = inner
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(parse_scalar)
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Yaml::List(items));
    }
    if (text.starts_with('"') && text.ends_with('"'))
        || (text.starts_with('\'') && text.ends_with('\''))
    {
        return Ok(Yaml::Str(text[1..text.len() - 1].to_owned()));
    }
    match text {
        "null" | "~" => Ok(Yaml::Null),
        "true" => Ok(Yaml::Bool(true)),
        "false" => Ok(Yaml::Bool(false)),
        _ => {
            if let Ok(number) = text.parse::<i64>() {
                return Ok(Yaml::Number(number));
            }
            // 只在形似浮点数时才尝试解析，避免把普通字符串（如 `v1.2`）误判成数字。
            let float_candidate = (text.contains('.') || text.contains('e') || text.contains('E'))
                && text.bytes().all(|byte| {
                    byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-')
                });
            let float = if float_candidate {
                text.parse::<f64>().ok()
            } else {
                None
            };
            if let Some(number) = float {
                return Ok(Yaml::Float(number));
            }
            Ok(Yaml::Str(text.to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path as StdPath, PathBuf as StdPathBuf};

    const BUNDLE: &str = r#"
# dshr bundle
- name: "@deepseek-ai/dsh-tool-workspace-dependencies"
  config:
    source: "C:/dsh/primary-runtime"
- id: fs-tools   # 可被 patch 定位
  name: "@deepseek-ai/dsh-tool-fs"
  inject: [tools, fs, systemPrompt]
  config:
    read: true
    write: false
- group: true
  name: cordis:group
  config: []
"#;

    const PATCH: &str = r#"
- id: fs-tools
  disabled: true
  config:
    read: true
    write: true
- insert:
    - name: "@deepseek-ai/dsh-tool-extra"
      config:
        limit: 3
"#;

    #[test]
    fn bundle_rows_parse_config_inject_and_group() {
        let rows = parse_bundle(BUNDLE).expect("bundle");
        assert_eq!(rows.len(), 3);
        assert!(rows[0].name.as_deref() == Some("@deepseek-ai/dsh-tool-workspace-dependencies"));
        assert_eq!(rows[0].config["source"], "C:/dsh/primary-runtime");
        assert_eq!(rows[1].id.as_deref(), Some("fs-tools"));
        assert_eq!(
            rows[1].inject.as_deref(),
            Some(&["tools".into(), "fs".into(), "systemPrompt".into()][..])
        );
        assert!(!rows[1].disabled);
        assert!(rows[2].group);
    }

    #[test]
    fn patch_overrides_config_wholesale_and_disables() {
        let mut rows = parse_bundle(BUNDLE).unwrap();
        let ops = parse_patch(PATCH).unwrap();
        let warnings = apply_patch(&mut rows, &ops);
        assert!(warnings.is_empty(), "{warnings:?}");
        let fs = rows
            .iter()
            .find(|row| row.id.as_deref() == Some("fs-tools"))
            .unwrap();
        assert!(fs.disabled);
        assert_eq!(fs.config["write"], true);
        // config 是整体替换，不是合并：原来没有 keys 就不该出现。
        assert!(fs.config.get("readImage").is_none());
        assert_eq!(rows.len(), 4, "insert 追加了一条");
    }

    #[test]
    fn entries_from_loader_skips_groups_and_maps_disabled() {
        let (entries, warnings) = entries_from_loader(BUNDLE, &[PATCH.to_owned()]).expect("loader");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(entries.len(), 3);
        let fs = entries
            .iter()
            .find(|entry| entry.name == "@deepseek-ai/dsh-tool-fs")
            .unwrap();
        assert!(!fs.enabled);
        assert!(entries.iter().all(|entry| entry.name != "cordis:group"));
    }

    #[test]
    fn missing_patch_target_is_a_warning_not_an_error() {
        let ops = parse_patch("- id: nope\n  disabled: true\n").unwrap();
        let mut rows = parse_bundle(BUNDLE).unwrap();
        let warnings = apply_patch(&mut rows, &ops);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn scalars_comments_and_quotes_are_handled() {
        let yaml = "- name: 'quoted: name' # comment\n  disabled: null\n";
        let rows = parse_bundle(yaml).unwrap();
        assert_eq!(rows[0].name.as_deref(), Some("quoted: name"));
        assert!(!rows[0].disabled);
    }

    // ---- M3-LOADER：include/group 展开 ----

    fn temp_dir(tag: &str) -> StdPathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dshr-loader-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_file(dir: &StdPath, name: &str, text: &str) -> StdPathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, text).expect("write temp file");
        path
    }

    fn names(entries: &[PluginEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    #[test]
    fn m3_loader_001_single_include_expands_in_place() {
        // M3-LOADER-001：单层 include 按声明位置展开，顺序确定。
        let dir = temp_dir("single");
        write_file(
            &dir,
            "extra.yml",
            "- name: '@x/extra'\n  config:\n    n: 1\n",
        );
        let bundle = "- name: '@x/first'\n- include: extra.yml\n- name: '@x/last'\n";
        let (entries, warnings) = entries_from_loader_with_base(bundle, &dir, &[]).expect("loader");
        assert_eq!(names(&entries), vec!["@x/first", "@x/extra", "@x/last"]);
        assert_eq!(entries[1].config["n"], 1);
        assert!(warnings.is_empty(), "{warnings:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn m3_loader_002_nested_include_uses_each_file_directory() {
        // M3-LOADER-002：多层 include，每层相对自己的文件目录解析。
        let dir = temp_dir("nested");
        write_file(&dir, "a.yml", "- include: sub/b.yml\n");
        write_file(&dir, "sub/b.yml", "- include: inner/c.yml\n");
        write_file(&dir, "sub/inner/c.yml", "- name: '@x/deep'\n");
        let (entries, warnings) =
            entries_from_loader_with_base("- include: a.yml\n", &dir, &[]).expect("loader");
        assert_eq!(names(&entries), vec!["@x/deep"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn m3_loader_003_include_array_expands_in_order() {
        // M3-LOADER-003：include 数组按列出的顺序展开。
        let dir = temp_dir("array");
        write_file(&dir, "x.yml", "- name: '@x/x1'\n- name: '@x/x2'\n");
        write_file(&dir, "y.yml", "- name: '@x/y1'\n");
        let bundle = "- name: '@x/first'\n- include: [x.yml, y.yml]\n- name: '@x/last'\n";
        let (entries, warnings) = entries_from_loader_with_base(bundle, &dir, &[]).expect("loader");
        assert_eq!(
            names(&entries),
            vec!["@x/first", "@x/x1", "@x/x2", "@x/y1", "@x/last"]
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn m3_loader_004_include_cycle_warns_and_skips() {
        // M3-LOADER-004：include 循环 → 告警并跳过，不无限递归、条目只出现一次。
        let dir = temp_dir("cycle");
        write_file(&dir, "a.yml", "- include: b.yml\n");
        write_file(&dir, "b.yml", "- include: a.yml\n- name: '@x/b1'\n");
        let (entries, warnings) =
            entries_from_loader_with_base("- include: a.yml\n", &dir, &[]).expect("loader");
        assert_eq!(names(&entries), vec!["@x/b1"]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("循环"), "{warnings:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn m3_loader_005_missing_include_is_a_warning() {
        // M3-LOADER-005：缺失 include 文件只告警，不影响其它条目。
        let dir = temp_dir("missing");
        let bundle = "- name: '@x/a'\n- include: nope.yml\n- name: '@x/b'\n";
        let (entries, warnings) = entries_from_loader_with_base(bundle, &dir, &[]).expect("loader");
        assert_eq!(names(&entries), vec!["@x/a", "@x/b"]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("不存在"), "{warnings:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn m3_loader_006_include_path_escape_is_rejected() {
        // M3-LOADER-006：`..` 逃逸与绝对路径都拒绝（即使目标文件真实存在）。
        let root = temp_dir("escape-root");
        let bundle_dir = root.join("bundle");
        std::fs::create_dir_all(&bundle_dir).expect("create bundle dir");
        write_file(&root, "outside.yml", "- name: '@x/evil'\n");

        let (entries, warnings) =
            entries_from_loader_with_base("- include: ../outside.yml\n", &bundle_dir, &[])
                .expect("loader");
        assert!(entries.is_empty(), "{entries:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("非法 include 路径") && warnings[0].contains("逃逸"),
            "{warnings:?}"
        );

        let outside = root.join("outside.yml").display().to_string();
        let absolute = format!("- include: \"{outside}\"\n");
        let (entries, warnings) =
            entries_from_loader_with_base(&absolute, &bundle_dir, &[]).expect("loader");
        assert!(entries.is_empty(), "{entries:?}");
        assert!(warnings[0].contains("非法 include 路径"), "{warnings:?}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn m3_loader_007_group_expands_in_order() {
        // M3-LOADER-007：group 在声明位置展开为顺序条目，group 自身不产生条目。
        let bundle = "\
- name: '@x/first'
- group: true
  config:
    - name: '@x/g1'
    - name: '@x/g2'
- name: '@x/last'
";
        let (entries, warnings) = entries_from_loader(bundle, &[]).expect("loader");
        assert_eq!(
            names(&entries),
            vec!["@x/first", "@x/g1", "@x/g2", "@x/last"]
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn m3_loader_008_nested_groups_expand_depth_first() {
        // M3-LOADER-008：嵌套 group 深度优先展开，order 确定。
        let bundle = "\
- group: true
  config:
    - name: '@x/g1'
    - group: true
      config:
        - name: '@x/n1'
        - name: '@x/n2'
    - name: '@x/g2'
- name: '@x/tail'
";
        let (entries, warnings) = entries_from_loader(bundle, &[]).expect("loader");
        assert_eq!(
            names(&entries),
            vec!["@x/g1", "@x/n1", "@x/n2", "@x/g2", "@x/tail"]
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn m3_loader_009_group_and_patch_interact() {
        // M3-LOADER-009：patch 可按 id 覆盖 group 内条目；`id` + insert 追加进 group。
        let bundle = "\
- id: grp
  group: true
  config:
    - id: child-a
      name: '@x/child-a'
      disabled: true
    - id: child-b
      name: '@x/child-b'
";
        let patch = "\
- id: child-a
  disabled: false
  config:
    k: v
- id: grp
  insert:
    - name: '@x/inserted-in-group'
- insert:
    - name: '@x/tail'
";
        let (entries, warnings) = entries_from_loader(bundle, &[patch.to_owned()]).expect("loader");
        assert_eq!(
            names(&entries),
            vec![
                "@x/child-a",
                "@x/child-b",
                "@x/inserted-in-group",
                "@x/tail"
            ]
        );
        assert!(entries[0].enabled, "patch 覆盖 group 内条目的 disabled");
        // config 整体替换，不是深合并。
        assert_eq!(entries[0].config, json!({ "k": "v" }));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn m3_loader_010_group_depth_limit_is_enforced() {
        // M3-LOADER-010：group 嵌套超过上限 → 告警并跳过更深部分。
        let within = nested_group_bundle(MAX_GROUP_DEPTH);
        let (entries, warnings) = entries_from_loader(&within, &[]).expect("loader");
        assert_eq!(names(&entries), vec!["@x/deep"], "上限内应展开");
        assert!(warnings.is_empty(), "{warnings:?}");

        let over = nested_group_bundle(MAX_GROUP_DEPTH + 1);
        let (entries, warnings) = entries_from_loader(&over, &[]).expect("loader");
        assert!(entries.is_empty(), "{entries:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("group 嵌套超过")
                && warnings[0].contains(&MAX_GROUP_DEPTH.to_string()),
            "{warnings:?}"
        );
    }

    #[test]
    fn m3_loader_011_js_expressions_are_not_evaluated() {
        // M3-LOADER-011：`!!js` 不求值，按原文字符串透传；`disabled: !!js` 按真值处理。
        let bundle = "\
- id: guarded
  name: '@x/guarded'
  disabled: !!js \"!ctx.get('profileContext')\"
  config:
    flag: !!js \"process.platform === 'win32'\"
";
        let (entries, warnings) = entries_from_loader(bundle, &[]).expect("loader");
        assert_eq!(names(&entries), vec!["@x/guarded"]);
        assert!(!entries[0].enabled, "disabled: !!js 按真值处理");
        let flag = entries[0].config["flag"]
            .as_str()
            .expect("!!js 透传为字符串");
        assert!(flag.contains("process.platform"), "{flag}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("!!js") && warnings[0].contains('2'),
            "{warnings:?}"
        );
    }

    #[test]
    fn m3_loader_012_text_entry_warns_about_include_without_base() {
        // M3-LOADER-012：无文件基准的文本入口不解析 include，但必须告警（不静默丢）。
        let (entries, warnings) = entries_from_loader("- include: a.yml\n", &[]).expect("loader");
        assert!(entries.is_empty(), "{entries:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("include"), "{warnings:?}");
        assert!(warnings[0].contains("基准"), "{warnings:?}");
    }

    #[test]
    fn m3_loader_013_json_include_is_supported() {
        // M3-LOADER-013：`.json` include 文件按 JSON 条目列表解析（真实 DSH 允许 YAML/JSON）。
        let dir = temp_dir("json");
        write_file(
            &dir,
            "extra.json",
            r#"[{"name":"@x/json-entry","config":{"limit":3}}]"#,
        );
        let (entries, warnings) =
            entries_from_loader_with_base("- include: extra.json\n", &dir, &[]).expect("loader");
        assert_eq!(names(&entries), vec!["@x/json-entry"]);
        assert_eq!(entries[0].config["limit"], 3);
        assert!(warnings.is_empty(), "{warnings:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn m3_loader_014_cordis_include_and_top_level_include_shapes() {
        // M3-LOADER-014：真实 DSH `cordis:include` 行与 bundle 顶层 include 映射。
        let dir = temp_dir("shapes");
        write_file(&dir, "one.yml", "- name: '@x/one'\n");
        write_file(&dir, "two.yml", "- name: '@x/two'\n");

        let cordis_row = "\
- name: cordis:include
  config:
    path: one.yml
- name: cordis:include
  config:
    paths: [two.yml]
";
        let (entries, warnings) =
            entries_from_loader_with_base(cordis_row, &dir, &[]).expect("loader");
        assert_eq!(names(&entries), vec!["@x/one", "@x/two"]);
        assert!(warnings.is_empty(), "{warnings:?}");

        let top_level = "include: [one.yml, two.yml]\n";
        let (entries, warnings) =
            entries_from_loader_with_base(top_level, &dir, &[]).expect("loader");
        assert_eq!(names(&entries), vec!["@x/one", "@x/two"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn m3_loader_015_include_depth_limit_is_enforced() {
        // M3-LOADER-015：include 链超过上限 → 告警并跳过更深文件；上限内正常展开。
        let dir = temp_dir("include-depth");
        let last = MAX_INCLUDE_DEPTH - 1;
        for index in 0..last {
            write_file(
                &dir,
                &format!("f{index}.yml"),
                &format!("- include: f{}.yml\n", index + 1),
            );
        }
        // 上限内：f0 → … → f15（f15 是名字条目，共 16 个文件）。
        write_file(&dir, &format!("f{last}.yml"), "- name: '@x/deep'\n");
        let (entries, warnings) =
            entries_from_loader_with_base("- include: f0.yml\n", &dir, &[]).expect("loader");
        assert_eq!(names(&entries), vec!["@x/deep"]);
        assert!(warnings.is_empty(), "{warnings:?}");

        // 超限：f15 再指向 f16，f16 需要 depth=16 → 告警并跳过。
        write_file(
            &dir,
            &format!("f{last}.yml"),
            &format!("- include: f{MAX_INCLUDE_DEPTH}.yml\n"),
        );
        write_file(
            &dir,
            &format!("f{MAX_INCLUDE_DEPTH}.yml"),
            "- name: '@x/too-deep'\n",
        );
        let (entries, warnings) =
            entries_from_loader_with_base("- include: f0.yml\n", &dir, &[]).expect("loader");
        assert!(entries.is_empty(), "{entries:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("include 嵌套超过")
                && warnings[0].contains(&MAX_INCLUDE_DEPTH.to_string()),
            "{warnings:?}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// 构造 `depth` 层 group 嵌套，最内层一个 `@x/deep` 条目。
    fn nested_group_bundle(depth: usize) -> String {
        let mut yaml = String::new();
        for level in 0..depth {
            let indent = " ".repeat(level * 4);
            yaml.push_str(&indent);
            yaml.push_str("- group: true\n");
            yaml.push_str(&format!("{indent}  config:\n"));
        }
        let indent = " ".repeat(depth * 4);
        yaml.push_str(&format!("{indent}- name: '@x/deep'\n"));
        yaml
    }
}
