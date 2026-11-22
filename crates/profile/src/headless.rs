//! DSH's manifest shape with native headless bundles and static row overrides.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::loader::{apply_patch, parse_bundle, parse_static_overrides, PatchOp};
use crate::PluginEntry;

const BUNDLES: [&str; 2] = ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-headless"];
const TOOL_FS: &str = "tool-fs";
const PATCH_FILE: &str = "cordis.patch.yml";
const FS_FIELDS: &[&str] = &["read", "readImage", "write", "edit", "applyPatch"];
const BASE_ENTRIES: &str = "
- id: tool-fs
  name: '@deepseek-ai/dsh-tool-fs'
  inject: [tools, fs, systemPrompt]
  config:
    read: true
    readImage: false
";

#[derive(Deserialize)]
struct Manifest {
    dsh: DshManifest,
}

#[derive(Deserialize)]
struct DshManifest {
    profile: Declaration,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    bundles: Vec<String>,
}

pub fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        && !name.ends_with('.')
        && !name.eq_ignore_ascii_case("node_modules")
        && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
}

/// Return the declared plugins, or the legacy profile source for an inline headless launch.
pub fn load(
    root: &Path,
    name: &str,
    extra_patches: &[PathBuf],
) -> Result<Option<Vec<PluginEntry>>, String> {
    if !valid_name(name) {
        return Err("无效 profile 名称".into());
    }
    let profile = root.join("profiles").join(name);
    let manifest = profile.join("package.json");
    let has_manifest = match optional_file(root, &manifest)? {
        Some(text) => {
            let manifest_data: Manifest = serde_json::from_str(text.trim_start_matches('\u{feff}'))
                .map_err(|error| format!("{}: {error}", manifest.display()))?;
            let bundles = manifest_data.dsh.profile.bundles;
            if bundles != BUNDLES {
                return Err(format!(
                    "{}: 不支持的 bundle 列表 {bundles:?}，预期 {BUNDLES:?}",
                    manifest.display()
                ));
            }
            true
        }
        None if name == "headless" => false,
        None => return Err(format!("{}: 找不到 profile 清单", manifest.display())),
    };
    let local = profile.join(PATCH_FILE);
    let home = root.join(PATCH_FILE);
    let local_text = optional_file(root, &local)?;
    let home_text = optional_file(root, &home)?;
    if !has_manifest && local_text.is_none() && home_text.is_none() && extra_patches.is_empty() {
        return Ok(None);
    }

    let mut rows = parse_bundle(BASE_ENTRIES)?;
    for (path, text) in [(&local, local_text), (&home, home_text)] {
        if let Some(text) = text {
            patch(&mut rows, path, &text)?;
        }
    }
    for path in extra_patches {
        let text =
            fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
        patch(&mut rows, path, &text)?;
    }
    let entries = rows.into_iter().filter_map(|row| {
        Some(PluginEntry {
            name: row.name?,
            enabled: !row.disabled,
            config: row.config,
            inject: row.inject,
        })
    });
    Ok(Some(entries.collect()))
}

fn optional_file(root: &Path, path: &Path) -> Result<Option<String>, String> {
    let resolved = match path.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let root = root
        .canonicalize()
        .map_err(|error| format!("{}: {error}", root.display()))?;
    if !resolved.starts_with(&root) {
        return Err(format!("{}: 路径超出 DSHR 数据根", path.display()));
    }
    fs::read_to_string(resolved)
        .map(Some)
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn patch(rows: &mut Vec<crate::loader::LoaderRow>, path: &Path, text: &str) -> Result<(), String> {
    let in_file = |message| format!("{}: {message}", path.display());
    let ops = parse_static_overrides(text.trim_start_matches('\u{feff}')).map_err(in_file)?;
    for op in &ops {
        let PatchOp::Override(row) = op else {
            return Err(in_file("仅支持静态条目覆盖".into()));
        };
        if row.id != TOOL_FS {
            return Err(in_file(format!("未知条目 `{}`", row.id)));
        }
        if let Some(fields) = row.config.as_ref().and_then(serde_json::Value::as_object) {
            for (name, value) in fields {
                if !FS_FIELDS.contains(&name.as_str()) || !value.is_boolean() {
                    return Err(in_file(format!(
                        "条目 `{}` 的 config 字段 `{name}` 不受支持或类型错误",
                        row.id
                    )));
                }
            }
        }
    }
    let warnings = apply_patch(rows, &ops);
    if let Some(warning) = warnings.into_iter().next() {
        return Err(in_file(warning));
    }
    Ok(())
}
