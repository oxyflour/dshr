use crate::*;

/// 自检：可信安装端到端（首装确认、摘要校验、篡改拒绝、元数据/审计）。
pub(crate) fn selftest_trust_install() -> i32 {
    let root = PathBuf::from(".tmp/trust-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if std::fs::create_dir_all(package.join("lib")).is_err() {
        eprintln!("[selftest-trust] 创建目录失败");
        return 1;
    }
    let package_json = json!({ "name": "selftest.trust", "version": "0.1.0" });
    let _ = std::fs::write(package.join("package.json"), package_json.to_string());
    let _ = std::fs::write(package.join("lib/index.js"), "export const ok = true\n");
    let Ok(digest) = package_digest(&package) else {
        eprintln!("[selftest-trust] 计算摘要失败");
        return 1;
    };
    let trust = json!({
        "schema_version": 1,
        "plugin_id": "selftest.trust",
        "version": "0.1.0",
        "package": { "digest_sha256": digest }
    });
    let _ = std::fs::write(package.join("dshr-trust.json"), trust.to_string());

    // 1) 未确认 → 退出码 2 且未安装。
    let unconfirmed = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: false,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    );
    let destination = sidecar.join("node_modules").join("selftest.trust");
    let rejected_unconfirmed = unconfirmed == 2 && !destination.exists();

    // 2) 确认 → 安装成功，元数据/摘要一致。
    let confirmed = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: true,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    );
    let meta = read_installed_meta(&destination);
    let installed_ok = confirmed == 0
        && meta
            .as_ref()
            .map(|meta| meta.digest_sha256 == digest)
            .unwrap_or(false);

    // 3) 篡改内容后重装 → 摘要不匹配被拒。
    let _ = std::fs::write(package.join("lib/index.js"), "malicious()\n");
    let tampered = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: true,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    );
    let tamper_rejected = tampered == 1;

    // 4) 审计：至少包含 accepted 与 reject 两种记录。
    let audit =
        std::fs::read_to_string(profile_dir.join("install-audit.jsonl")).unwrap_or_default();
    let accepted = audit.contains("\"decision\":\"accepted\"");
    let rejected = audit.contains("\"decision\":\"rejected\"");
    let signature_status = meta.map(|meta| meta.signature.status).unwrap_or_default();
    let ok = rejected_unconfirmed && installed_ok && tamper_rejected && accepted && rejected;
    println!(
        "[selftest-trust] rejected_unconfirmed={rejected_unconfirmed} installed_ok={installed_ok} tamper_rejected={tamper_rejected} audit_accepted={accepted} audit_rejected={rejected} signature_status={signature_status}"
    );
    if ok {
        println!("[selftest-trust] PASS");
        0
    } else {
        println!("[selftest-trust] FAIL");
        1
    }
}

/// 自检：安装 → 卸载（目录/profile/审计）→ 密钥轮换（旧钥拒、新钥装）。
pub(crate) fn selftest_plugin_lifecycle() -> i32 {
    use ring::signature::KeyPair as _;

    let root = PathBuf::from(".tmp/m4-plugin-lifecycle");
    let _ = std::fs::remove_dir_all(&root);
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if std::fs::create_dir_all(package.join("lib")).is_err() {
        eprintln!("[selftest-lifecycle] 创建目录失败");
        return 1;
    }
    let name = "m4.lifecycle";
    if std::fs::write(
        package.join("package.json"),
        json!({ "name": name, "version": "0.1.0" }).to_string(),
    )
    .is_err()
        || std::fs::write(package.join("lib/index.js"), "export const ok = true\n").is_err()
    {
        eprintln!("[selftest-lifecycle] 写包失败");
        return 1;
    }
    let Some(old_key) = ed25519_keypair() else {
        eprintln!("[selftest-lifecycle] 生成旧钥失败");
        return 1;
    };
    let Some(new_key) = ed25519_keypair() else {
        eprintln!("[selftest-lifecycle] 生成新钥失败");
        return 1;
    };

    // A. 首装（未签名 → --yes-install）→ 卸载。
    let digest_ok = package_digest(&package)
        .map(|digest| {
            let trust = json!({
                "schema_version": 1,
                "plugin_id": name,
                "version": "0.1.0",
                "package": { "digest_sha256": digest }
            });
            std::fs::write(package.join("dshr-trust.json"), trust.to_string()).is_ok()
        })
        .unwrap_or(false);
    let installed = digest_ok
        && install_plugin_with_options(
            &InstallCliOptions {
                package: package.clone(),
                yes_install: true,
                allow_downgrade: false,
            },
            &sidecar,
            &profile_dir,
        ) == 0;
    let destination = sidecar.join("node_modules").join(name);
    let installed_ok = installed && destination.exists();

    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[selftest-lifecycle] 加载 profile 失败: {err}");
            return 1;
        }
    };
    let uninstalled = uninstall_package(&sidecar, name, &mut profile);
    let audit_text =
        std::fs::read_to_string(profile_dir.join("install-audit.jsonl")).unwrap_or_default();
    let uninstall_ok = uninstalled
        .as_ref()
        .map(|report| report.removed_dir && report.removed_profile_entry)
        .unwrap_or(false)
        && !destination.exists()
        && !profile.entries().iter().any(|entry| entry.name == name)
        && audit_text.contains("\"action\":\"uninstall\"")
        && audit_text.contains("\"decision\":\"accepted\"");

    // B. 旧钥签名安装 → 轮换 → 旧钥被拒 → 新钥重签安装。
    let key_added = add_trust_key(
        &profile_dir,
        TrustKey::new("old", hex_lower(old_key.public_key().as_ref())),
        now_millis(),
    )
    .is_ok();
    let signed_old = sign_package_with_key(&package, name, "0.1.0", &old_key, "old").is_ok();
    let old_install = signed_old
        && install_plugin_with_options(
            &InstallCliOptions {
                package: package.clone(),
                yes_install: false,
                allow_downgrade: false,
            },
            &sidecar,
            &profile_dir,
        ) == 0;
    let rotated = rotate_trust_key(
        &profile_dir,
        "old",
        TrustKey::new("new", hex_lower(new_key.public_key().as_ref())),
        now_millis(),
    )
    .is_ok();
    let store_ok = TrustStore::load(&profile_dir)
        .map(|store| {
            store.get("old").is_some_and(|key| key.revoked)
                && store.get("new").is_some_and(|key| !key.revoked)
        })
        .unwrap_or(false);
    let old_rejected = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: false,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    ) == 1;
    let signed_new = sign_package_with_key(&package, name, "0.1.0", &new_key, "new").is_ok();
    let new_install = signed_new
        && install_plugin_with_options(
            &InstallCliOptions {
                package: package.clone(),
                yes_install: false,
                allow_downgrade: false,
            },
            &sidecar,
            &profile_dir,
        ) == 0;
    let meta_ok = read_installed_meta(&destination)
        .map(|meta| {
            meta.signature.status == "verified" && meta.signature.key_id.as_deref() == Some("new")
        })
        .unwrap_or(false);
    let key_audit =
        std::fs::read_to_string(profile_dir.join("trust-key-audit.jsonl")).unwrap_or_default();
    let key_audit_ok = key_audit.contains("\"action\":\"add\"")
        && key_audit.contains("\"action\":\"rotate\"")
        && key_audit.contains("\"old_key_id\":\"old\"");

    let ok = installed_ok
        && uninstall_ok
        && key_added
        && old_install
        && rotated
        && store_ok
        && old_rejected
        && new_install
        && meta_ok
        && key_audit_ok;
    println!(
        "[selftest-lifecycle] installed={installed_ok} uninstalled={uninstall_ok} key_added={key_added} old_install={old_install} rotated={rotated} store_ok={store_ok} old_rejected={old_rejected} new_install={new_install} meta_ok={meta_ok} key_audit={key_audit_ok}"
    );
    if ok {
        println!("[selftest-lifecycle] PASS");
        0
    } else {
        println!("[selftest-lifecycle] FAIL");
        1
    }
}

/// 安装流程自检：本地包复制 + profile 追加 + 重启后可读。
pub(crate) fn selftest_install() -> i32 {
    let root = PathBuf::from(".tmp/install-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(root.join("sidecar"));
    let package = root.join("pkg");
    let _ = std::fs::create_dir_all(package.join("lib"));
    let _ = std::fs::write(
        package.join("package.json"),
        json!({ "name": "@deepseek-ai/dsh-tool-selftest", "version": "1.0.0" }).to_string(),
    );
    let _ = std::fs::write(package.join("lib/index.js"), "export const ok = true\n");

    let mut profile = match Profile::load(root.join("profile")) {
        Ok(profile) => profile,
        Err(err) => {
            println!("[selftest-install] 加载 profile 失败: {err}");
            return 1;
        }
    };
    let report = match install_package(&root.join("sidecar"), &package, &mut profile) {
        Ok(report) => report,
        Err(err) => {
            println!("[selftest-install] 安装失败: {err}");
            return 1;
        }
    };
    let copied = report.destination.join("lib/index.js").exists();
    let reloaded = Profile::load(root.join("profile")).ok();
    let in_profile = reloaded.is_some_and(|profile| {
        profile
            .entries()
            .iter()
            .any(|entry| entry.name == "@deepseek-ai/dsh-tool-selftest" && entry.enabled)
    });

    println!(
        "[install] name={} copied={copied} profile_added={} in_profile={in_profile}",
        report.name, report.added_to_profile
    );
    let ok = copied && report.added_to_profile && in_profile;
    if ok {
        println!("[selftest-install] PASS");
        0
    } else {
        println!("[selftest-install] FAIL");
        1
    }
}
