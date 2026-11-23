use super::*;
use crate::install::{install_dir, prepare_package, read_manifest, read_package_version};

/// 可信安装：校验 → 降级判定 → 确认 → 复制 → 元数据 → 审计。
///
/// `confirm` 是调用方注入的确认回调（CLI/GUI 各自实现）；返回 false 即拒绝。
/// 任何拒绝也会写审计（`action: "reject"`）。
pub fn install_package_trusted<F>(
    sidecar_dir: &Path,
    package: &Path,
    profile: &mut Profile,
    policy: &TrustPolicy,
    confirm: F,
) -> Result<TrustedInstallReport, TrustError>
where
    F: FnMut(&InstallPreview) -> bool,
{
    install_package_trusted_with_legacy(sidecar_dir, None, package, profile, policy, confirm)
}

pub fn install_package_trusted_with_legacy<F>(
    sidecar_dir: &Path,
    legacy_dir: Option<&Path>,
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
    install_package_trusted_from_dir(
        sidecar_dir,
        legacy_dir,
        &source.root,
        profile,
        policy,
        &mut confirm,
    )
}

fn install_package_trusted_from_dir<F>(
    sidecar_dir: &Path,
    legacy_dir: Option<&Path>,
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
    let at_ms = now_ms(policy);
    let trust = verify_package_at(
        package_dir,
        &name,
        actual_version.as_deref(),
        &policy.store,
        Some(at_ms),
    )?;

    let destination = sidecar_dir.join("node_modules").join(&name);
    let installed = read_installed_meta(&destination).or_else(|| {
        if destination.exists() {
            return None;
        }
        let legacy = legacy_dir?.join("node_modules").join(&name);
        read_installed_meta(&legacy)
    });
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
        installed_at_ms: at_ms,
    };
    write_installed_meta(&install.destination, &meta)?;
    append_audit(
        profile_dir_of(profile),
        &AuditRecord {
            ts: at_ms,
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
