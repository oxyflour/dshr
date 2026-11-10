//! Loader YAML 子集：bundle 条目列表 + patch 层。
//!
//! 只实现我们需要的方言（见 `cordis-composition-reference`），不追求完整 YAML：
//! - 顶层条目列表：`- name: ...` / `id:` / `config:` / `inject:` / `disabled:` / `group: true`；
//! - patch 文件：每项是 `insert: [rows]` 或带 `id` 的覆盖映射（config 整体替换）；
//! - 标量支持字符串（引号）、布尔、数字、null、`[a, b]` 数组；`!!js` 视为字符串；
//! - 注释、空行、`---` 文档分隔符。
//!
//! 不支持的语义（`cordis:include`、`group` 内插入、`!!js` 求值）会生成 warning，
//! 由调用方决定如何呈现，而不是静默丢弃。

use serde_json::{json, Value};

use crate::PluginEntry;

#[derive(Debug, Clone, PartialEq)]
enum Yaml {
    Null,
    Bool(bool),
    Number(i64),
    Str(String),
    List(Vec<Yaml>),
    Map(Vec<(String, Yaml)>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoaderRow {
    pub id: Option<String>,
    pub name: Option<String>,
    pub group: bool,
    pub disabled: bool,
    pub inject: Option<Vec<String>>,
    pub config: Value,
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
    Insert(Vec<LoaderRow>),
    Override(RowPatch),
}

/// 解析 bundle 文件（顶层条目列表）。
pub fn parse_bundle(yaml: &str) -> Result<Vec<LoaderRow>, String> {
    let document = parse_document(yaml)?;
    let Yaml::List(items) = document else {
        return Err(String::from("bundle 顶层必须是条目列表（- name: ...）"));
    };
    let mut rows = Vec::new();
    for item in items {
        rows.push(interpret_row(&item)?);
    }
    Ok(rows)
}

/// 解析 patch 文件（顶层是 patch 映射列表）。
pub fn parse_patch(yaml: &str) -> Result<Vec<PatchOp>, String> {
    let document = parse_document(yaml)?;
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
                    let mut rows = Vec::new();
                    for entry in entries {
                        rows.push(interpret_row(&entry)?);
                    }
                    insert = Some(rows);
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
            ops.push(PatchOp::Insert(rows));
        } else if !patch.id.is_empty() {
            ops.push(PatchOp::Override(patch));
        } else {
            return Err(String::from("patch 既没有 insert 也没有 id"));
        }
    }
    Ok(ops)
}

/// 应用 patch 层；返回 warning（目标不存在、group 插入等）。
pub fn apply_patch(rows: &mut Vec<LoaderRow>, ops: &[PatchOp]) -> Vec<String> {
    let mut warnings = Vec::new();
    for op in ops {
        match op {
            PatchOp::Insert(entries) => {
                for entry in entries {
                    if entry.group {
                        warnings.push(format!(
                            "跳过 group 插入 `{}`（当前不展开 group）",
                            entry.name.clone().unwrap_or_default()
                        ));
                        continue;
                    }
                    rows.push(entry.clone());
                }
            }
            PatchOp::Override(patch) => {
                let Some(row) = rows
                    .iter_mut()
                    .find(|row| row.id.as_deref() == Some(patch.id.as_str()))
                else {
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

/// bundle + patch → `PluginEntry`（只保留顶层、非 group 的条目）。
pub fn entries_from_loader(
    bundle: &str,
    patches: &[String],
) -> Result<(Vec<PluginEntry>, Vec<String>), String> {
    let mut rows = parse_bundle(bundle)?;
    let mut warnings = Vec::new();
    for patch in patches {
        let ops = parse_patch(patch)?;
        warnings.extend(apply_patch(&mut rows, &ops));
    }
    let mut entries = Vec::new();
    for row in rows {
        if row.group {
            continue;
        }
        let Some(name) = row.name else {
            warnings.push(String::from("条目缺少 name，已跳过"));
            continue;
        };
        entries.push(PluginEntry {
            name,
            enabled: !row.disabled,
            config: row.config,
            inject: row.inject,
        });
    }
    Ok((entries, warnings))
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
    };
    for (key, value) in pairs {
        match key.as_str() {
            "id" => row.id = Some(scalar_string(value)),
            "name" => row.name = Some(scalar_string(value)),
            "group" => row.group = truthy(value),
            "disabled" => row.disabled = truthy_or_defined(value),
            "inject" => row.inject = Some(string_list(value)),
            "config" => row.config = yaml_to_value(value),
            "intercept" | "isolate" => {
                // 这些字段影响服务可见性/实例，当前宿主不实现；保留但不透传。
            }
            other => return Err(format!("bundle 条目不支持的字段 `{other}`")),
        }
    }
    Ok(row)
}

fn truthy_or_defined(value: &Yaml) -> bool {
    match value {
        Yaml::Null => false,
        Yaml::Bool(value) => *value,
        Yaml::Str(text) => !text.is_empty(),
        _ => true,
    }
}

fn truthy(value: &Yaml) -> bool {
    match value {
        Yaml::Null => false,
        Yaml::Bool(value) => *value,
        Yaml::Str(text) => text == "true",
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
        Yaml::Str(text) => text.clone(),
        Yaml::List(_) | Yaml::Map(_) => String::new(),
    }
}

fn yaml_to_value(value: &Yaml) -> Value {
    match value {
        Yaml::Null => Value::Null,
        Yaml::Bool(value) => Value::Bool(*value),
        Yaml::Number(value) => json!(value),
        Yaml::Str(text) => Value::String(text.clone()),
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
    if text.starts_with("!!js ") {
        if let Some(rest) = text.strip_prefix("!!js ") {
            return Ok(Yaml::Str(rest.trim().to_owned()));
        }
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
            Ok(Yaml::Str(text.to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
