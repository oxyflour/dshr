use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use dshr_session_store::try_lock_session;
use ring::digest;
use serde::Deserialize;

const MAX_ARCHIVE: u64 = 1024 * 1024 * 1024;
const MAX_EXPANDED: u64 = 2 * MAX_ARCHIVE;
const WORKSPACE_DEPS: &str = "@deepseek-ai/dsh-tool-workspace-dependencies";
pub(super) const WORKSPACE_DEPS_TOOL: &str = "load_workspace_dependencies";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Component {
    Python,
    CreatorBuild,
}

impl Component {
    pub(super) const ALL: [Self; 2] = [Self::Python, Self::CreatorBuild];

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::CreatorBuild => "creator-build",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.id() == value)
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Python => "Python 工作区运行时",
            Self::CreatorBuild => "创造模式构建组件",
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Self::Python => "提供 Python 解释器和标准库。",
            Self::CreatorBuild => "离线编译 Rust 插件，包含 Cargo、链接器和 SDK 依赖。",
        }
    }

    fn complete(self, root: &Path) -> bool {
        match self {
            Self::Python => python_payload_ready(&root.join("primary-runtime")),
            Self::CreatorBuild => {
                root.join("toolchain/bin/cargo.exe").is_file()
                    && root.join("toolchain/bin/rustc.exe").is_file()
                    && root
                        .join("toolchain/lib/rustlib/x86_64-pc-windows-gnu/lib")
                        .is_dir()
                    && root.join("vendor").is_dir()
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema_version: u32,
    platform: String,
    components: BTreeMap<String, Asset>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Asset {
    pub(super) version: String,
    pub(super) bytes: u64,
    archive: String,
    sha256: String,
    url: Option<String>,
}

impl Asset {
    fn validate(&self) -> Result<(), String> {
        let simple = |value: &str| {
            !value.is_empty()
                && value.len() <= 96
                && value
                    .bytes()
                    .next()
                    .is_some_and(|byte| byte.is_ascii_alphanumeric())
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
                && !value.ends_with('.')
                && valid_path_part(value)
        };
        if !simple(&self.version)
            || !simple(&self.archive)
            || !self.archive.ends_with(".zip")
            || self.bytes == 0
            || self.bytes > MAX_ARCHIVE
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("运行组件目录包含无效的版本、文件名、大小或 SHA-256".to_owned());
        }
        if let Some(raw) = &self.url {
            let url = url::Url::parse(raw).map_err(|_| "无效的运行组件下载地址".to_owned())?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("运行组件地址必须是 HTTPS".to_owned());
            }
        }
        Ok(())
    }

    fn destination(&self, data: &Path, component: Component) -> PathBuf {
        data.join("runtime-components")
            .join(component.id())
            .join(&self.version)
    }
}

fn io_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn read_asset(app: &Path, component: Component) -> Result<Asset, String> {
    let path = app.join("runtime/components.json");
    let content = fs::read_to_string(&path)
        .map_err(|error| format!("本发行包未提供运行组件目录。{error}"))?;
    let mut catalog: Catalog = serde_json::from_str(content.trim_start_matches('\u{feff}'))
        .map_err(|error| format!("运行组件目录无效。{error}"))?;
    if catalog.schema_version != 1
        || catalog.platform != "windows-x64"
        || !cfg!(all(windows, target_arch = "x86_64"))
    {
        return Err("当前平台或运行组件目录版本不受支持".to_owned());
    }
    let asset = catalog
        .components
        .remove(component.id())
        .ok_or_else(|| format!("本发行包未提供{}", component.title()))?;
    asset.validate()?;
    Ok(asset)
}

pub(super) fn asset(component: Component) -> Result<Asset, String> {
    read_asset(&crate::app_root(), component)
}

fn planned(component: Component) -> Option<PathBuf> {
    asset(component)
        .ok()
        .map(|entry| entry.destination(&crate::data_root(), component))
}

pub(super) fn installed(component: Component) -> Option<PathBuf> {
    planned(component).filter(|root| component.complete(root))
}

pub(super) fn install(component: Component) -> Result<PathBuf, String> {
    let app = crate::app_root();
    let asset = read_asset(&app, component)?;
    install_with(&app, &crate::data_root(), component, &asset)
}

fn install_with(
    app: &Path,
    data: &Path,
    component: Component,
    asset: &Asset,
) -> Result<PathBuf, String> {
    let destination = asset.destination(data, component);
    let parent = destination.parent().expect("component version parent");
    fs::create_dir_all(parent).map_err(io_error)?;
    let _lock =
        try_lock_session(&destination).map_err(|error| format!("无法取得组件安装锁。{error}"))?;
    if component.complete(&destination) {
        return Ok(destination);
    }
    if destination.exists() {
        return Err(format!("组件目录不完整。{}", destination.display()));
    }
    let work = tempfile::tempdir_in(parent).map_err(io_error)?;
    let archive_path = work.path().join("payload.zip");
    let offline = app.join("runtime-packages").join(&asset.archive);
    let output = File::create(&archive_path).map_err(io_error)?;
    if offline.is_file() {
        verified_copy(File::open(offline).map_err(io_error)?, output, asset)?;
    } else if let Some(url) = &asset.url {
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(Duration::from_secs(600)))
            .build()
            .new_agent();
        let body = agent
            .get(url)
            .call()
            .map_err(|error| format!("组件下载失败。{error}"))?
            .into_body();
        verified_copy(body.into_reader(), output, asset)?;
    } else {
        return Err(format!("找不到离线组件 {}", asset.archive));
    }

    let staged = work.path().join("component");
    fs::create_dir(&staged).map_err(io_error)?;
    unpack(File::open(&archive_path).map_err(io_error)?, &staged)?;
    if !component.complete(&staged) {
        return Err("运行组件包缺少必需文件".to_owned());
    }
    fs::rename(&staged, &destination).map_err(io_error)?;
    Ok(destination)
}

fn verified_copy(input: impl Read, mut output: impl Write, asset: &Asset) -> Result<(), String> {
    let mut input = input.take(asset.bytes + 1);
    let mut state = digest::Context::new(&digest::SHA256);
    let mut buffer = [0; 64 * 1024];
    let mut total = 0u64;
    loop {
        let size = input.read(&mut buffer).map_err(io_error)?;
        if size == 0 {
            break;
        }
        total += size as u64;
        if total > asset.bytes {
            return Err("运行组件字节数与发行目录不符".to_owned());
        }
        state.update(&buffer[..size]);
        output.write_all(&buffer[..size]).map_err(io_error)?;
    }
    let actual: String = state
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if total != asset.bytes || !actual.eq_ignore_ascii_case(&asset.sha256) {
        return Err("运行组件大小或 SHA-256 与发行目录不符".to_owned());
    }
    Ok(())
}

fn unpack(file: File, root: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(file).map_err(io_error)?;
    if archive.len() > 50_000 {
        return Err("运行组件条目过多".to_owned());
    }
    let mut remaining = MAX_EXPANDED;
    for index in 0..archive.len() {
        let mut item = archive.by_index(index).map_err(io_error)?;
        let name = item.name().trim_end_matches('/');
        if name.is_empty()
            || name.split('/').any(|part| !valid_path_part(part))
            || item.is_symlink()
            || !(item.is_file() || item.is_dir())
        {
            return Err("运行组件包含非法路径或特殊文件".to_owned());
        }
        let path = root.join(name);
        if item.is_dir() {
            fs::create_dir_all(&path).map_err(io_error)?;
            continue;
        }
        if item.size() > remaining {
            return Err("运行组件解压数据超限".to_owned());
        }
        fs::create_dir_all(path.parent().expect("asset file parent")).map_err(io_error)?;
        let mut output = File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io_error)?;
        let copied =
            io::copy(&mut item.by_ref().take(remaining + 1), &mut output).map_err(io_error)?;
        if copied != item.size() {
            return Err("运行组件文件大小无效".to_owned());
        }
        remaining -= copied;
    }
    Ok(())
}

fn valid_path_part(part: &str) -> bool {
    let stem = part
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    !part.is_empty()
        && !part.ends_with(['.', ' '])
        && !part.chars().any(|ch| {
            ch.is_control() || matches!(ch, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
        && !matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        )
        && !matches!(
            stem.strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT")),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
        )
}

fn python_payload_ready(path: &Path) -> bool {
    path.join("runtime.json").is_file()
        && path
            .join(if cfg!(windows) {
                "dependencies/python/python.exe"
            } else {
                "dependencies/python/bin/python3"
            })
            .is_file()
        && if cfg!(windows) {
            path.join("dependencies/python/Lib/site-packages").is_dir()
        } else {
            true
        }
}

fn workspace_dependencies_source(entry: &serde_json::Value) -> Option<PathBuf> {
    let default = dshr_profile::default_primary_runtime();
    let source = entry["config"]["source"].as_str().unwrap_or(&default);
    if (!source.is_empty() && source != default)
        || crate::non_empty_env_path("DSHR_DSH_SOURCE").is_some()
        || python_payload_ready(Path::new(source))
    {
        return (!source.is_empty()).then(|| PathBuf::from(source));
    }
    planned(Component::Python)
        .map(|root| root.join("primary-runtime"))
        .or_else(|| (!source.is_empty()).then(|| PathBuf::from(source)))
}

pub(super) fn sidecar_profile(raw: &str) -> Option<String> {
    let mut entries = serde_json::from_str::<Vec<serde_json::Value>>(raw).ok()?;
    for entry in entries
        .iter_mut()
        .filter(|row| row["name"] == WORKSPACE_DEPS)
    {
        if let Some(source) = workspace_dependencies_source(entry) {
            if entry["config"].is_object() || entry["config"].is_null() {
                entry["config"]["source"] = serde_json::json!(source);
            }
        }
    }
    serde_json::to_string(&entries).ok()
}

pub(super) fn web_credential_refs(profile: &str) -> BTreeSet<String> {
    let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(profile) else {
        return BTreeSet::new();
    };
    let mut names = BTreeSet::new();
    for entry in entries.iter().filter(|entry| entry["enabled"] != false) {
        let fields: &[&str] = match entry["name"].as_str() {
            Some("@deepseek-ai/dsh-web-deepseek-api") => &["searchApiKeyEnv", "fetchApiKeyEnv"],
            Some("@deepseek-ai/dsh-web-search-deepseek") => &["apiKeyEnv"],
            _ => continue,
        };
        for field in fields {
            let name = entry["config"][field]
                .as_str()
                .filter(|value| !value.is_empty())
                .unwrap_or("DEEPSEEK_API_KEY");
            let mut bytes = name.bytes();
            if bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                names.insert(name.to_owned());
            }
        }
    }
    names
}

pub(super) fn workspace_dependencies_ready() -> bool {
    let Some(entries) = std::env::var("DSHR_DSH_PLUGINS")
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
    else {
        return true;
    };
    let Some(entry) = entries.iter().find(|row| row["name"] == WORKSPACE_DEPS) else {
        return true;
    };
    workspace_dependencies_source(entry)
        .as_deref()
        .is_some_and(python_payload_ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    fn archive(app: &Path, extra: Option<&str>) -> Asset {
        let path = app.join("runtime-packages/test.zip");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for name in [
            "primary-runtime/runtime.json",
            "primary-runtime/dependencies/python/python.exe",
            "primary-runtime/dependencies/python/bin/python3",
            "primary-runtime/dependencies/python/Lib/site-packages/placeholder",
        ]
        .into_iter()
        .chain(extra)
        {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(b"test").unwrap();
        }
        zip.finish().unwrap();
        let bytes = fs::read(path).unwrap();
        let sha256 = digest::digest(&digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Asset {
            version: "test".into(),
            bytes: bytes.len() as u64,
            archive: "test.zip".into(),
            sha256,
            url: None,
        }
    }

    // M4-RUNTIME-001
    #[test]
    fn installs_verified_assets_once_and_rejects_bad_archives() {
        let app = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let component = Component::Python;
        let mut asset = archive(app.path(), None);
        let installed = install_with(app.path(), data.path(), component, &asset).unwrap();
        assert!(component.complete(&installed));
        assert_eq!(
            install_with(app.path(), data.path(), component, &asset).unwrap(),
            installed
        );

        asset.version = "invalid-hash".into();
        asset.sha256 = "0".repeat(64);
        assert!(install_with(app.path(), data.path(), component, &asset)
            .unwrap_err()
            .contains("SHA-256"));
        assert!(!asset.destination(data.path(), component).exists());

        let mut asset = archive(app.path(), Some("../outside"));
        asset.version = "invalid-path".into();
        assert!(install_with(app.path(), data.path(), component, &asset)
            .unwrap_err()
            .contains("非法路径"));
        assert!(!asset.destination(data.path(), component).exists());
        assert!(component.complete(&installed));

        asset.version = "missing-payload".into();
        assert!(install_with(app.path(), data.path(), Component::CreatorBuild, &asset).is_err());
        assert!(!asset
            .destination(data.path(), Component::CreatorBuild)
            .exists());
    }
}
