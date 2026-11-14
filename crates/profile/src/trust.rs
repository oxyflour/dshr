//! 最小插件信任链（M3，见 `docs/spec/plugin-trust.md` 与 ADR-0009）。
//!
//! 三件事：
//! 1. **完整性**：按 `dshr-trust/v1` 的规范化算法计算包摘要（SHA-256），
//!    与包内 `dshr-trust.json` 声明的摘要比对；
//! 2. **来源（可选）**：ed25519 签名（ring），公钥来自本机信任密钥库
//!    `<profile>/trust-keys.json`；
//! 3. **知情**：首装/未验证签名需要调用方确认，接受与拒绝都写
//!    `<profile>/install-audit.jsonl`；安装后写 `<dest>/.dshr-install.json`。
//!
//! 不做 CA/证书链/吊销/时间戳（M4）。信任文件是**包级兄弟文件**而不是 manifest 字段，
//! 因为 manifest 无法包含自身摘要（自引用）。

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ring::digest::SHA256;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::install::{install_dir, prepare_package, read_manifest, read_package_version};
use crate::{InstallReport, Profile};

/// 包级信任文件名（不参与摘要；UTF-8 JSON）。
pub const TRUST_FILE: &str = "dshr-trust.json";
/// 信任协议版本（只接受 1）。
pub const TRUST_SCHEMA_VERSION: u32 = 1;
/// 已安装元数据文件名（写在安装目标目录内）。
pub const INSTALL_META_FILE: &str = ".dshr-install.json";
/// 审计日志文件名（写在 profile 目录）。
pub const AUDIT_FILE: &str = "install-audit.jsonl";
/// 信任密钥库文件名（写在 profile 目录）。
pub const KEY_STORE_FILE: &str = "trust-keys.json";

const HEX_DIGEST_LEN: usize = 64;
const HEX_SIGNATURE_LEN: usize = 128;
const HEX_PUBLIC_KEY_LEN: usize = 64;

#[derive(Debug, Error)]
pub enum TrustError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("package error: {0}")]
    Package(String),
    #[error("trust file is malformed: {0}")]
    TrustManifest(String),
    #[error("package digest mismatch: expected {expected}, actual {actual}")]
    IntegrityMismatch { expected: String, actual: String },
    #[error("unknown signing key `{key_id}`")]
    UnknownKey { key_id: String },
    #[error("invalid ed25519 signature (key `{key_id}`)")]
    Signature { key_id: String },
    #[error("package version {candidate} is older than installed {installed}")]
    Downgrade {
        installed: String,
        candidate: String,
    },
    #[error("install was not confirmed by the caller")]
    NotConfirmed,
}

impl TrustError {
    /// 稳定的错误码（写审计日志与 UI 用；与 spec §7 一致）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(_) | Self::Package(_) => "E_TRUST_IO",
            Self::TrustManifest(_) => "E_TRUST_MANIFEST",
            Self::IntegrityMismatch { .. } => "E_INTEGRITY_MISMATCH",
            Self::UnknownKey { .. } => "E_TRUST_UNKNOWN_KEY",
            Self::Signature { .. } => "E_TRUST_SIGNATURE",
            Self::Downgrade { .. } => "E_TRUST_DOWNGRADE",
            Self::NotConfirmed => "E_TRUST_NOT_CONFIRMED",
        }
    }
}

/// 包内 `dshr-trust.json`。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustFile {
    pub schema_version: u32,
    pub plugin_id: String,
    pub version: String,
    pub package: PackageIntegrity,
    #[serde(default)]
    pub signature: Option<PackageSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIntegrity {
    pub digest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSignature {
    pub scheme: String,
    pub key_id: String,
    /// 小写 hex（64 字节 = 128 hex chars）。
    pub value_hex: String,
}

/// 信任密钥库中的一把公钥。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustKey {
    pub key_id: String,
    /// ed25519 公钥，64 位小写 hex。
    pub public_key_hex: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustKeyStoreFile {
    pub schema_version: u32,
    pub keys: Vec<TrustKey>,
}

/// 内存中的信任密钥库。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustStore {
    keys: Vec<TrustKey>,
}

impl TrustStore {
    pub fn new(keys: Vec<TrustKey>) -> Self {
        Self { keys }
    }

    /// 从 profile 目录读 `trust-keys.json`；文件不存在时返回空库（不报错）。
    pub fn load(profile_dir: &Path) -> Result<Self, TrustError> {
        let path = profile_dir.join(KEY_STORE_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)?;
        let text = text.trim_start_matches('\u{feff}');
        let file: TrustKeyStoreFile = serde_json::from_str(text)
            .map_err(|err| TrustError::TrustManifest(format!("{KEY_STORE_FILE}: {err}")))?;
        if file.schema_version != TRUST_SCHEMA_VERSION {
            return Err(TrustError::TrustManifest(format!(
                "{KEY_STORE_FILE}: unsupported schema_version {}",
                file.schema_version
            )));
        }
        let mut seen = Vec::new();
        for key in &file.keys {
            if key.key_id.trim().is_empty() {
                return Err(TrustError::TrustManifest(
                    "trust key has empty key_id".to_owned(),
                ));
            }
            if seen.contains(&key.key_id) {
                return Err(TrustError::TrustManifest(format!(
                    "duplicate trust key id `{}`",
                    key.key_id
                )));
            }
            decode_hex_exact(&key.public_key_hex, HEX_PUBLIC_KEY_LEN, "public_key_hex")
                .map_err(TrustError::TrustManifest)?;
            seen.push(key.key_id.clone());
        }
        Ok(Self { keys: file.keys })
    }

    pub fn get(&self, key_id: &str) -> Option<&TrustKey> {
        self.keys.iter().find(|key| key.key_id == key_id)
    }

    pub fn keys(&self) -> &[TrustKey] {
        &self.keys
    }
}

/// 签名验证结果（写进安装预览/元数据/审计）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureStatus {
    /// 包里没有 `dshr-trust.json`（未验证）。
    Missing,
    /// 有 trust 文件但没有签名（未验证，但摘要可用于完整性）。
    Unsigned,
    /// 签名验证通过。
    Verified,
}

impl SignatureStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Unsigned => "unsigned",
            Self::Verified => "verified",
        }
    }
}

/// 一次包校验的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustReport {
    pub name: String,
    pub version: String,
    pub digest_sha256: String,
    pub trust_file: Option<TrustFile>,
    pub signature_status: SignatureStatus,
    pub key_id: Option<String>,
}

/// 安装前展示给用户/调用方的信息（确认回调的入参）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPreview {
    pub name: String,
    pub version: String,
    pub digest_sha256: String,
    pub signature_status: SignatureStatus,
    pub key_id: Option<String>,
    pub first_install: bool,
    pub installed_version: Option<String>,
    pub downgrade: bool,
    pub permissions_hint: String,
}

/// 信任策略（由调用方决定：CLI 或 GUI）。
#[derive(Debug, Clone)]
pub struct TrustPolicy {
    /// 允许显式降级（`--allow-downgrade`）。
    pub allow_downgrade: bool,
    /// 未验证签名的包是否必须确认（默认 true）。
    pub require_confirm_for_unsigned: bool,
    /// 信任密钥库。
    pub store: TrustStore,
    /// 审计/元数据用的时间源（测试可注入；None 用系统时间）。
    pub now_ms: Option<u64>,
}

impl Default for TrustPolicy {
    fn default() -> Self {
        Self {
            allow_downgrade: false,
            require_confirm_for_unsigned: true,
            store: TrustStore::default(),
            now_ms: None,
        }
    }
}

/// 安装结果：沿用 M2 的 `InstallReport`，附带信任报告与实际是否确认。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedInstallReport {
    pub install: InstallReport,
    pub trust: TrustReport,
    pub user_confirmed: bool,
    pub downgrade: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledMeta {
    pub schema_version: u32,
    pub name: String,
    pub version: String,
    pub digest_sha256: String,
    pub signature: InstalledSignature,
    pub installed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledSignature {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
}

/// 审计记录（一行一条 JSON）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub ts: u64,
    pub action: String,
    pub name: String,
    pub version: String,
    pub digest_sha256: String,
    pub signature_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub user_confirmed: bool,
    pub first_install: bool,
    pub downgrade: bool,
}

/// 计算包摘要（`dshr-trust/v1` §3）。`dshr-trust.json` 自身不参与。
pub fn package_digest(root: &Path) -> Result<String, TrustError> {
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    collect_files(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let mut ctx = ring::digest::Context::new(&SHA256);
    for (relative, path) in entries {
        let bytes = fs::read(&path)?;
        ctx.update(relative.as_bytes());
        ctx.update(&[0u8]);
        ctx.update(bytes.len().to_string().as_bytes());
        ctx.update(&[0u8]);
        ctx.update(&bytes);
    }
    Ok(to_hex(ctx.finish().as_ref()))
}

fn collect_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), TrustError> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            // 信任链不跟随链接；安装路径同样拒绝/跳过链接。
            continue;
        }
        if metadata.is_dir() {
            if is_skipped_dir(&name) {
                continue;
            }
            dirs.push(path);
        } else if metadata.is_file() {
            if name == TRUST_FILE {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| {
                    TrustError::Package(format!("path escapes package: {}", path.display()))
                })?
                .to_string_lossy()
                .replace('\\', "/");
            out.push((relative, path));
        }
    }
    for dir in dirs {
        collect_files(root, &dir, out)?;
    }
    Ok(())
}

fn is_skipped_dir(name: &str) -> bool {
    name == ".git" || name == "node_modules" || name.starts_with('.')
}

/// 读取并按信任链校验一个包（不安装）。
pub fn verify_package(
    root: &Path,
    actual_name: &str,
    actual_version: Option<&str>,
    store: &TrustStore,
) -> Result<TrustReport, TrustError> {
    let digest_sha256 = package_digest(root)?;
    let trust_path = root.join(TRUST_FILE);
    if !trust_path.exists() {
        return Ok(TrustReport {
            name: actual_name.to_owned(),
            version: actual_version.unwrap_or_default().to_owned(),
            digest_sha256,
            trust_file: None,
            signature_status: SignatureStatus::Missing,
            key_id: None,
        });
    }
    let text = fs::read_to_string(&trust_path)?;
    let text = text.trim_start_matches('\u{feff}');
    let trust_file: TrustFile = serde_json::from_str(text)
        .map_err(|err| TrustError::TrustManifest(format!("{TRUST_FILE}: invalid JSON: {err}")))?;
    validate_trust_file(&trust_file, actual_name, actual_version, &digest_sha256)?;

    let mut signature_status = SignatureStatus::Unsigned;
    let mut key_id = None;
    if let Some(signature) = &trust_file.signature {
        if !signature.scheme.eq_ignore_ascii_case("ed25519") {
            return Err(TrustError::TrustManifest(format!(
                "unsupported signature scheme `{}`",
                signature.scheme
            )));
        }
        let key = store
            .get(&signature.key_id)
            .ok_or_else(|| TrustError::UnknownKey {
                key_id: signature.key_id.clone(),
            })?;
        let public_key =
            decode_hex_exact(&key.public_key_hex, HEX_PUBLIC_KEY_LEN, "public_key_hex")
                .map_err(TrustError::TrustManifest)?;
        let signature_bytes = decode_hex_exact(
            &signature.value_hex,
            HEX_SIGNATURE_LEN,
            "signature.value_hex",
        )
        .map_err(TrustError::TrustManifest)?;
        let message = signature_message(&trust_file.plugin_id, &trust_file.version, &digest_sha256);
        UnparsedPublicKey::new(&ED25519, public_key)
            .verify(message.as_bytes(), &signature_bytes)
            .map_err(|_| TrustError::Signature {
                key_id: signature.key_id.clone(),
            })?;
        signature_status = SignatureStatus::Verified;
        key_id = Some(signature.key_id.clone());
    }
    Ok(TrustReport {
        name: actual_name.to_owned(),
        version: trust_file.version.clone(),
        digest_sha256,
        trust_file: Some(trust_file),
        signature_status,
        key_id,
    })
}

fn validate_trust_file(
    trust: &TrustFile,
    actual_name: &str,
    actual_version: Option<&str>,
    digest_sha256: &str,
) -> Result<(), TrustError> {
    if trust.schema_version != TRUST_SCHEMA_VERSION {
        return Err(TrustError::TrustManifest(format!(
            "unsupported schema_version {}",
            trust.schema_version
        )));
    }
    if trust.plugin_id != actual_name {
        return Err(TrustError::TrustManifest(format!(
            "plugin_id `{}` does not match package `{actual_name}`",
            trust.plugin_id
        )));
    }
    if trust.version.trim().is_empty() {
        return Err(TrustError::TrustManifest("empty version".to_owned()));
    }
    if let Some(actual) = actual_version {
        if !actual.is_empty() && actual != trust.version {
            return Err(TrustError::TrustManifest(format!(
                "version `{}` does not match package version `{actual}`",
                trust.version
            )));
        }
    }
    decode_hex_exact(
        &trust.package.digest_sha256,
        HEX_DIGEST_LEN,
        "package.digest_sha256",
    )
    .map_err(TrustError::TrustManifest)?;
    if trust.package.digest_sha256 != digest_sha256 {
        return Err(TrustError::IntegrityMismatch {
            expected: trust.package.digest_sha256.clone(),
            actual: digest_sha256.to_owned(),
        });
    }
    if trust.signature.is_some() && trust.package.digest_sha256.is_empty() {
        return Err(TrustError::TrustManifest(
            "signature requires package.digest_sha256".to_owned(),
        ));
    }
    Ok(())
}

fn signature_message(plugin_id: &str, version: &str, digest_sha256: &str) -> String {
    format!("dshr-trust/v1\n{plugin_id}\n{version}\n{digest_sha256}")
}

/// 读取已安装元数据；不存在或损坏时返回 `None`（损坏不影响安装，后续会被覆盖）。
pub fn read_installed_meta(destination: &Path) -> Option<InstalledMeta> {
    let path = destination.join(INSTALL_META_FILE);
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

fn write_installed_meta(destination: &Path, meta: &InstalledMeta) -> Result<(), TrustError> {
    let path = destination.join(INSTALL_META_FILE);
    let text = serde_json::to_string_pretty(meta)
        .map_err(|err| TrustError::Package(format!("serialize install meta: {err}")))?;
    fs::write(path, format!("{text}\n"))?;
    Ok(())
}

/// 追加审计记录；审计失败不阻塞安装（记录到 stderr 由调用方决定），但这里返回错误让调用方可见。
pub fn append_audit(profile_dir: &Path, record: &AuditRecord) -> Result<(), TrustError> {
    fs::create_dir_all(profile_dir)?;
    let path = profile_dir.join(AUDIT_FILE);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(record)
        .map_err(|err| TrustError::Package(format!("serialize audit: {err}")))?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

/// `Profile::path()` 指向 `plugins.json`（文件），审计/密钥库需要的是 profile 目录。
fn profile_dir_of(profile: &Profile) -> &Path {
    profile.path().parent().unwrap_or_else(|| Path::new("."))
}

/// 可信安装：校验 → 降级判定 → 确认 → 复制 → 元数据 → 审计。
///
/// `confirm` 是调用方注入的确认回调（CLI/GUI 各自实现）；返回 false 即拒绝。
/// 任何拒绝也会写审计（`action: "reject"`）。
pub fn install_package_trusted<F>(
    sidecar_dir: &Path,
    package: &Path,
    profile: &mut Profile,
    policy: &TrustPolicy,
    mut confirm: F,
) -> Result<TrustedInstallReport, TrustError>
where
    F: FnMut(&InstallPreview) -> bool,
{
    if !profile.can_edit() {
        return Err(TrustError::Package(
            "Loader 模式的插件列表只读，不能安装".to_owned(),
        ));
    }
    let source = prepare_package(package).map_err(TrustError::Package)?;
    install_package_trusted_from_dir(sidecar_dir, &source.root, profile, policy, &mut confirm)
}

fn install_package_trusted_from_dir<F>(
    sidecar_dir: &Path,
    package_dir: &Path,
    profile: &mut Profile,
    policy: &TrustPolicy,
    confirm: &mut F,
) -> Result<TrustedInstallReport, TrustError>
where
    F: FnMut(&InstallPreview) -> bool,
{
    let name = read_manifest(package_dir).map_err(TrustError::Package)?;
    let actual_version = read_package_version(package_dir).map_err(TrustError::Package)?;
    let trust = verify_package(package_dir, &name, actual_version.as_deref(), &policy.store)?;

    let destination = sidecar_dir.join("node_modules").join(&name);
    let installed = read_installed_meta(&destination);
    let installed_version = installed.as_ref().map(|meta| meta.version.clone());
    let first_install = installed.is_none();
    let downgrade = match (installed_version.as_deref(), actual_version.as_deref()) {
        (Some(installed), Some(candidate)) => version_less(candidate, installed),
        _ => false,
    };
    let preview = InstallPreview {
        name: name.clone(),
        version: trust.version.clone(),
        digest_sha256: trust.digest_sha256.clone(),
        signature_status: trust.signature_status,
        key_id: trust.key_id.clone(),
        first_install,
        installed_version: installed_version.clone(),
        downgrade,
        permissions_hint: permissions_hint(package_dir),
    };

    if downgrade && !policy.allow_downgrade {
        let error = TrustError::Downgrade {
            installed: installed_version.unwrap_or_default(),
            candidate: trust.version.clone(),
        };
        audit_reject(profile, policy, &preview, &error)?;
        return Err(error);
    }

    // 已验证签名 = 来源可信，无需打扰用户；未验证（无 trust 文件/无签名）在首装
    // 或策略要求时必须确认。
    let needs_confirm = trust.signature_status != SignatureStatus::Verified
        && (first_install || policy.require_confirm_for_unsigned);
    let user_confirmed = if needs_confirm {
        confirm(&preview)
    } else {
        false
    };
    if needs_confirm && !user_confirmed {
        let error = TrustError::NotConfirmed;
        audit_reject(profile, policy, &preview, &error)?;
        return Err(error);
    }

    let install = install_dir(sidecar_dir, package_dir, profile).map_err(TrustError::Package)?;
    let meta = InstalledMeta {
        schema_version: TRUST_SCHEMA_VERSION,
        name: name.clone(),
        version: trust.version.clone(),
        digest_sha256: trust.digest_sha256.clone(),
        signature: InstalledSignature {
            status: trust.signature_status.as_str().to_owned(),
            key_id: trust.key_id.clone(),
        },
        installed_at_ms: now_ms(policy),
    };
    write_installed_meta(&install.destination, &meta)?;
    append_audit(
        profile_dir_of(profile),
        &AuditRecord {
            ts: now_ms(policy),
            action: "install".to_owned(),
            name,
            version: trust.version.clone(),
            digest_sha256: trust.digest_sha256.clone(),
            signature_status: trust.signature_status.as_str().to_owned(),
            key_id: trust.key_id.clone(),
            decision: "accepted".to_owned(),
            reason: None,
            user_confirmed,
            first_install,
            downgrade,
        },
    )?;
    Ok(TrustedInstallReport {
        install,
        trust,
        user_confirmed,
        downgrade,
    })
}

fn audit_reject(
    profile: &Profile,
    policy: &TrustPolicy,
    preview: &InstallPreview,
    error: &TrustError,
) -> Result<(), TrustError> {
    append_audit(
        profile_dir_of(profile),
        &AuditRecord {
            ts: now_ms(policy),
            action: "reject".to_owned(),
            name: preview.name.clone(),
            version: preview.version.clone(),
            digest_sha256: preview.digest_sha256.clone(),
            signature_status: preview.signature_status.as_str().to_owned(),
            key_id: preview.key_id.clone(),
            decision: "rejected".to_owned(),
            reason: Some(format!("{}: {error}", error.code())),
            user_confirmed: false,
            first_install: preview.first_install,
            downgrade: preview.downgrade,
        },
    )
}

fn permissions_hint(package_dir: &Path) -> String {
    // 先看 CPP 的 plugin.toml，再看 DSH package.json 的 dsh 字段；都读不到就返回通用提示。
    if let Ok(text) = fs::read_to_string(package_dir.join("plugin.toml")) {
        if let Some(line) = text
            .lines()
            .find(|line| line.trim_start().starts_with("permissions"))
        {
            return line.trim().to_owned();
        }
    }
    if let Ok(text) = fs::read_to_string(package_dir.join("package.json")) {
        if let Ok(value) =
            serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}'))
        {
            if let Some(dsh) = value.get("dsh") {
                return format!("dsh: {dsh}");
            }
        }
    }
    "可在你的用户权限下执行代码".to_owned()
}

fn now_ms(policy: &TrustPolicy) -> u64 {
    policy.now_ms.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    })
}

/// 极简 SemVer 比较（只比较 x.y.z 与预发布后缀；build metadata 忽略）。
/// 返回 `true` 表示 `candidate < installed`。
pub fn version_less(candidate: &str, installed: &str) -> bool {
    match (parse_version(candidate), parse_version(installed)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SimpleVersion {
    core: (u64, u64, u64),
    prerelease: Option<String>,
}

impl Ord for SimpleVersion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.core
            .cmp(&other.core)
            .then_with(|| match (&self.prerelease, &other.prerelease) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl PartialOrd for SimpleVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn parse_version(raw: &str) -> Option<SimpleVersion> {
    let core_end = raw.find(['-', '+']).unwrap_or(raw.len());
    let (core, rest) = raw.split_at(core_end);
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let prerelease = rest
        .strip_prefix('-')
        .map(|pre| pre.split('+').next().unwrap_or(pre).to_owned());
    Some(SimpleVersion {
        core: (major, minor, patch),
        prerelease,
    })
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex_exact(raw: &str, expected_len: usize, field: &str) -> Result<Vec<u8>, String> {
    if raw.len() != expected_len {
        return Err(format!(
            "{field}: expected {expected_len} hex chars, got {}",
            raw.len()
        ));
    }
    let mut out = Vec::with_capacity(expected_len / 2);
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let high = hex_nibble(bytes[index])?;
        let low = hex_nibble(bytes[index + 1])?;
        out.push((high << 4) | low);
        index += 2;
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("invalid hex character".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::test_support::{build_tar, gzip};
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dshr-trust-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn package(root: &Path, name: &str, version: &str) -> PathBuf {
        let dir = root.join("pkg");
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(
            dir.join("package.json"),
            serde_json::json!({ "name": name, "version": version }).to_string(),
        )
        .unwrap();
        fs::write(dir.join("lib/index.js"), "export const ok = true\n").unwrap();
        dir
    }

    fn write_trust(dir: &Path, trust: &TrustFile) {
        fs::write(
            dir.join(TRUST_FILE),
            serde_json::to_string_pretty(trust).unwrap(),
        )
        .unwrap();
    }

    fn unsigned_trust(dir: &Path, name: &str, version: &str) -> TrustFile {
        TrustFile {
            schema_version: 1,
            plugin_id: name.to_owned(),
            version: version.to_owned(),
            package: PackageIntegrity {
                digest_sha256: package_digest(dir).unwrap(),
            },
            signature: None,
        }
    }

    fn signing_key() -> Ed25519KeyPair {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
    }

    fn store_with(keypair: &Ed25519KeyPair, key_id: &str) -> TrustStore {
        TrustStore::new(vec![TrustKey {
            key_id: key_id.to_owned(),
            public_key_hex: to_hex(keypair.public_key().as_ref()),
            note: None,
        }])
    }

    fn signed_trust(dir: &Path, name: &str, version: &str, keypair: &Ed25519KeyPair, key_id: &str) {
        let digest = package_digest(dir).unwrap();
        let message = signature_message(name, version, &digest);
        let signature = keypair.sign(message.as_bytes());
        write_trust(
            dir,
            &TrustFile {
                schema_version: 1,
                plugin_id: name.to_owned(),
                version: version.to_owned(),
                package: PackageIntegrity {
                    digest_sha256: digest,
                },
                signature: Some(PackageSignature {
                    scheme: "ed25519".to_owned(),
                    key_id: key_id.to_owned(),
                    value_hex: to_hex(signature.as_ref()),
                }),
            },
        );
    }

    fn policy(store: TrustStore) -> TrustPolicy {
        TrustPolicy {
            allow_downgrade: false,
            require_confirm_for_unsigned: true,
            store,
            now_ms: Some(1_763_000_000_000),
        }
    }

    fn sidecar(root: &Path) -> PathBuf {
        let dir = root.join("sidecar");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // CPP-TRUST-001：摘要与文件创建顺序无关（按相对路径排序）。
    #[test]
    fn digest_is_order_independent() {
        let root = temp_root("digest-order");
        let a = root.join("a");
        fs::create_dir_all(a.join("sub")).unwrap();
        fs::write(a.join("x.txt"), "x").unwrap();
        fs::write(a.join("sub/y.txt"), "y").unwrap();
        let b = root.join("b");
        fs::create_dir_all(b.join("sub")).unwrap();
        fs::write(b.join("sub/y.txt"), "y").unwrap();
        fs::write(b.join("x.txt"), "x").unwrap();
        assert_eq!(package_digest(&a).unwrap(), package_digest(&b).unwrap());
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-002：内容被改 → 摘要变化。
    #[test]
    fn digest_detects_tampering() {
        let root = temp_root("digest-tamper");
        let dir = package(&root, "cad.validate", "0.1.0");
        let before = package_digest(&dir).unwrap();
        fs::write(dir.join("lib/index.js"), "export const ok = false\n").unwrap();
        assert_ne!(before, package_digest(&dir).unwrap());
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-003：dshr-trust.json 自身不参与摘要。
    #[test]
    fn digest_ignores_trust_file() {
        let root = temp_root("digest-trust-file");
        let dir = package(&root, "cad.validate", "0.1.0");
        let before = package_digest(&dir).unwrap();
        write_trust(&dir, &unsigned_trust(&dir, "cad.validate", "0.1.0"));
        assert_eq!(before, package_digest(&dir).unwrap());
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-004：无信任文件 → Missing，但摘要可用。
    #[test]
    fn missing_trust_file_reports_unsigned() {
        let root = temp_root("missing");
        let dir = package(&root, "cad.validate", "0.1.0");
        let report =
            verify_package(&dir, "cad.validate", Some("0.1.0"), &TrustStore::default()).unwrap();
        assert_eq!(report.signature_status, SignatureStatus::Missing);
        assert_eq!(report.digest_sha256.len(), 64);
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-005：摘要匹配且无签名 → Unsigned。
    #[test]
    fn integrity_match_without_signature_is_unsigned() {
        let root = temp_root("unsigned");
        let dir = package(&root, "cad.validate", "0.1.0");
        write_trust(&dir, &unsigned_trust(&dir, "cad.validate", "0.1.0"));
        let report =
            verify_package(&dir, "cad.validate", Some("0.1.0"), &TrustStore::default()).unwrap();
        assert_eq!(report.signature_status, SignatureStatus::Unsigned);
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-006：内容被改但声明摘要未变 → IntegrityMismatch。
    #[test]
    fn tampered_package_is_rejected() {
        let root = temp_root("tampered");
        let dir = package(&root, "cad.validate", "0.1.0");
        write_trust(&dir, &unsigned_trust(&dir, "cad.validate", "0.1.0"));
        fs::write(dir.join("lib/index.js"), "malicious()\n").unwrap();
        let error = verify_package(&dir, "cad.validate", Some("0.1.0"), &TrustStore::default())
            .unwrap_err();
        assert!(matches!(error, TrustError::IntegrityMismatch { .. }));
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-007：trust 文件与清单版本不一致 → TrustManifest。
    #[test]
    fn trust_version_must_match_manifest() {
        let root = temp_root("version");
        let dir = package(&root, "cad.validate", "0.1.0");
        write_trust(&dir, &unsigned_trust(&dir, "cad.validate", "9.9.9"));
        let error = verify_package(&dir, "cad.validate", Some("0.1.0"), &TrustStore::default())
            .unwrap_err();
        assert!(matches!(error, TrustError::TrustManifest(_)));
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-008：有效 ed25519 签名 → Verified。
    #[test]
    fn valid_signature_is_verified() {
        let root = temp_root("signature-ok");
        let dir = package(&root, "cad.validate", "0.1.0");
        let keypair = signing_key();
        signed_trust(&dir, "cad.validate", "0.1.0", &keypair, "cad-vendor");
        let report = verify_package(
            &dir,
            "cad.validate",
            Some("0.1.0"),
            &store_with(&keypair, "cad-vendor"),
        )
        .unwrap();
        assert_eq!(report.signature_status, SignatureStatus::Verified);
        assert_eq!(report.key_id.as_deref(), Some("cad-vendor"));
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-009：坏签名 → Signature。
    #[test]
    fn invalid_signature_is_rejected() {
        let root = temp_root("signature-bad");
        let dir = package(&root, "cad.validate", "0.1.0");
        let keypair = signing_key();
        signed_trust(&dir, "cad.validate", "0.1.0", &keypair, "cad-vendor");
        let mut trust: TrustFile =
            serde_json::from_str(&fs::read_to_string(dir.join(TRUST_FILE)).unwrap()).unwrap();
        trust.signature.as_mut().unwrap().value_hex = "00".repeat(64);
        write_trust(&dir, &trust);
        let error = verify_package(
            &dir,
            "cad.validate",
            Some("0.1.0"),
            &store_with(&keypair, "cad-vendor"),
        )
        .unwrap_err();
        assert!(matches!(error, TrustError::Signature { .. }));
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-010：未知密钥 → UnknownKey。
    #[test]
    fn unknown_key_is_rejected() {
        let root = temp_root("unknown-key");
        let dir = package(&root, "cad.validate", "0.1.0");
        let keypair = signing_key();
        signed_trust(&dir, "cad.validate", "0.1.0", &keypair, "cad-vendor");
        let error = verify_package(&dir, "cad.validate", Some("0.1.0"), &TrustStore::default())
            .unwrap_err();
        assert!(matches!(error, TrustError::UnknownKey { .. }));
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-011：首装未确认 → NotConfirmed，不安装，写 reject 审计。
    #[test]
    fn first_install_requires_confirmation() {
        let root = temp_root("confirm-no");
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let dir = package(&root, "cad.validate", "0.1.0");
        let error = install_package_trusted(
            &sidecar(&root),
            &dir,
            &mut profile,
            &policy(TrustStore::default()),
            |preview| {
                assert!(preview.first_install);
                assert_eq!(preview.signature_status, SignatureStatus::Missing);
                false
            },
        )
        .unwrap_err();
        assert!(matches!(error, TrustError::NotConfirmed));
        assert!(!sidecar(&root).join("node_modules/cad.validate").exists());
        let audit = fs::read_to_string(profile_dir_of(&profile).join(AUDIT_FILE)).unwrap();
        let record: AuditRecord = serde_json::from_str(audit.lines().last().unwrap()).unwrap();
        assert_eq!(record.action, "reject");
        assert!(record
            .reason
            .as_deref()
            .unwrap()
            .contains("E_TRUST_NOT_CONFIRMED"));
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-012：首装确认 → 安装成功，写元数据与 accept 审计。
    #[test]
    fn confirmed_install_writes_metadata_and_audit() {
        let root = temp_root("confirm-yes");
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let dir = package(&root, "cad.validate", "0.1.0");
        write_trust(&dir, &unsigned_trust(&dir, "cad.validate", "0.1.0"));
        let report = install_package_trusted(
            &sidecar(&root),
            &dir,
            &mut profile,
            &policy(TrustStore::default()),
            |_| true,
        )
        .unwrap();
        assert!(report.user_confirmed);
        assert!(!report.downgrade);
        let meta = read_installed_meta(&report.install.destination).expect("meta");
        assert_eq!(meta.version, "0.1.0");
        assert_eq!(meta.signature.status, "unsigned");
        assert_eq!(meta.digest_sha256, report.trust.digest_sha256);
        let audit = fs::read_to_string(profile_dir_of(&profile).join(AUDIT_FILE)).unwrap();
        let record: AuditRecord = serde_json::from_str(audit.lines().last().unwrap()).unwrap();
        assert_eq!(record.action, "install");
        assert_eq!(record.decision, "accepted");
        assert!(record.user_confirmed);
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-013：有效签名安装无需确认（仍审计 user_confirmed=false）。
    #[test]
    fn verified_signature_installs_without_confirmation() {
        let root = temp_root("signed-install");
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let dir = package(&root, "cad.validate", "0.1.0");
        let keypair = signing_key();
        signed_trust(&dir, "cad.validate", "0.1.0", &keypair, "cad-vendor");
        let report = install_package_trusted(
            &sidecar(&root),
            &dir,
            &mut profile,
            &policy(store_with(&keypair, "cad-vendor")),
            |preview| panic!("verified package must not prompt: {preview:?}"),
        )
        .unwrap();
        assert!(!report.user_confirmed);
        assert_eq!(report.trust.signature_status, SignatureStatus::Verified);
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-014：降级被拒（已装 2.0.0，候选 1.0.0）。
    #[test]
    fn downgrade_is_rejected() {
        let root = temp_root("downgrade");
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let dir = package(&root, "cad.validate", "2.0.0");
        let sidecar_dir = sidecar(&root);
        install_package_trusted(
            &sidecar_dir,
            &dir,
            &mut profile,
            &policy(TrustStore::default()),
            |_| true,
        )
        .unwrap();
        fs::write(
            dir.join("package.json"),
            serde_json::json!({ "name": "cad.validate", "version": "1.0.0" }).to_string(),
        )
        .unwrap();
        let error = install_package_trusted(
            &sidecar_dir,
            &dir,
            &mut profile,
            &policy(TrustStore::default()),
            |_| panic!("downgrade must be rejected before confirmation"),
        )
        .unwrap_err();
        assert!(matches!(error, TrustError::Downgrade { .. }));
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-015：显式允许降级 → 安装并审计 downgrade=true。
    #[test]
    fn downgrade_can_be_forced() {
        let root = temp_root("downgrade-force");
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let sidecar_dir = sidecar(&root);
        let dir = package(&root, "cad.validate", "2.0.0");
        install_package_trusted(
            &sidecar_dir,
            &dir,
            &mut profile,
            &policy(TrustStore::default()),
            |_| true,
        )
        .unwrap();
        fs::write(
            dir.join("package.json"),
            serde_json::json!({ "name": "cad.validate", "version": "1.0.0" }).to_string(),
        )
        .unwrap();
        let mut forced = policy(TrustStore::default());
        forced.allow_downgrade = true;
        let report =
            install_package_trusted(&sidecar_dir, &dir, &mut profile, &forced, |_| true).unwrap();
        assert!(report.downgrade);
        let audit = fs::read_to_string(profile_dir_of(&profile).join(AUDIT_FILE)).unwrap();
        let record: AuditRecord = serde_json::from_str(audit.lines().last().unwrap()).unwrap();
        assert!(record.downgrade);
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-016：tgz 包走同一条信任链。
    #[test]
    fn tgz_package_uses_same_trust_path() {
        let root = temp_root("tgz");
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let pkg = package(&root, "cad.validate", "0.1.0");
        let digest = package_digest(&pkg).unwrap();
        let trust = serde_json::json!({
            "schema_version": 1,
            "plugin_id": "cad.validate",
            "version": "0.1.0",
            "package": { "digest_sha256": digest }
        });
        let trust_text = serde_json::to_string(&trust).unwrap();
        let package_json = fs::read_to_string(pkg.join("package.json")).unwrap();
        let index_js = fs::read_to_string(pkg.join("lib/index.js")).unwrap();
        let tar = build_tar(&[
            ("package.json", b'0', package_json.as_bytes()),
            ("lib/", b'5', &[]),
            ("lib/index.js", b'0', index_js.as_bytes()),
            (TRUST_FILE, b'0', trust_text.as_bytes()),
        ]);
        let tgz = root.join("pkg.tgz");
        fs::write(&tgz, gzip(&tar)).unwrap();
        let report = install_package_trusted(
            &sidecar(&root),
            &tgz,
            &mut profile,
            &policy(TrustStore::default()),
            |_| true,
        )
        .unwrap();
        assert_eq!(report.install.name, "cad.validate");
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-017：密钥库加载：重复/非法公钥拒绝。
    #[test]
    fn key_store_validation() {
        let root = temp_root("keystore");
        let good = serde_json::json!({
            "schema_version": 1,
            "keys": [{ "key_id": "k1", "public_key_hex": "11".repeat(32) }]
        });
        fs::write(
            root.join(KEY_STORE_FILE),
            serde_json::to_string(&good).unwrap(),
        )
        .unwrap();
        let store = TrustStore::load(&root).unwrap();
        assert!(store.get("k1").is_some());

        let duplicate = serde_json::json!({
            "schema_version": 1,
            "keys": [
                { "key_id": "k1", "public_key_hex": "11".repeat(32) },
                { "key_id": "k1", "public_key_hex": "22".repeat(32) }
            ]
        });
        fs::write(
            root.join(KEY_STORE_FILE),
            serde_json::to_string(&duplicate).unwrap(),
        )
        .unwrap();
        assert!(TrustStore::load(&root).is_err());

        let bad = serde_json::json!({
            "schema_version": 1,
            "keys": [{ "key_id": "k1", "public_key_hex": "zz" }]
        });
        fs::write(
            root.join(KEY_STORE_FILE),
            serde_json::to_string(&bad).unwrap(),
        )
        .unwrap();
        assert!(TrustStore::load(&root).is_err());
        fs::remove_dir_all(root).ok();
    }

    // CPP-TRUST-018：SemVer 降级比较（预发布 < 正式）。
    #[test]
    fn version_comparison() {
        assert!(version_less("1.0.0", "1.0.1"));
        assert!(version_less("2.0.0", "10.0.0"));
        assert!(version_less("1.0.0-rc.1", "1.0.0"));
        assert!(!version_less("1.0.0", "1.0.0"));
        assert!(!version_less("1.0.1", "1.0.0"));
        assert!(!version_less("not-semver", "1.0.0"));
    }
}
