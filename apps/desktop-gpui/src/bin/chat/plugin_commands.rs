use crate::*;

pub(crate) struct InstallCliOptions {
    pub(crate) package: PathBuf,
    /// 非交互确认（CI / 脚本）；未验证签名的包必须显式给出。
    pub(crate) yes_install: bool,
    pub(crate) allow_downgrade: bool,
}

pub(crate) fn install_plugin_cli(package: PathBuf) -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let sidecar_dir = sidecar_dir_for_app();
    let profile_dir = profile_dir_for_app();
    install_plugin_with_options(
        &InstallCliOptions {
            package,
            yes_install: args.iter().any(|arg| arg == "--yes-install"),
            allow_downgrade: args.iter().any(|arg| arg == "--allow-downgrade"),
        },
        &sidecar_dir,
        &profile_dir,
    )
}

/// 可信安装 CLI（M3 G2）：摘要/签名校验 → 首装确认（`--yes-install`）→ 审计。
pub(crate) fn install_plugin_with_options(
    options: &InstallCliOptions,
    sidecar_dir: &std::path::Path,
    profile_dir: &std::path::Path,
) -> i32 {
    if let Err(err) = std::fs::create_dir_all(sidecar_dir) {
        eprintln!("[install] 创建 sidecar 目录失败: {err}");
        return 1;
    }
    let mut profile = match Profile::load(profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[install] 加载 profile 失败: {err}");
            return 1;
        }
    };
    let store = match TrustStore::load(profile_dir) {
        Ok(store) => store,
        Err(err) => {
            eprintln!("[install] 信任密钥库无效（{profile_dir:?}/trust-keys.json）: {err}");
            return 1;
        }
    };
    let policy = TrustPolicy {
        allow_downgrade: options.allow_downgrade,
        require_confirm_for_unsigned: true,
        store,
        now_ms: None,
    };
    let package = options.package.clone();
    let yes_install = options.yes_install;
    let result = install_package_trusted(sidecar_dir, &package, &mut profile, &policy, |preview| {
        // 非交互 CLI：必须显式 --yes-install；打印与 spec §5 一致的预览项。
        println!(
            "[install] 待安装: {} {}（{}）",
            preview.name,
            preview.version,
            preview.signature_status.as_str()
        );
        println!(
            "[install] 来源: {}；摘要: {}",
            package.display(),
            preview.digest_sha256
        );
        if let Some(key_id) = &preview.key_id {
            println!("[install] 签名 key: {key_id}");
        }
        if let Some(installed) = &preview.installed_version {
            println!("[install] 已安装版本: {installed}");
        }
        println!("[install] 会获得的权限: {}", preview.permissions_hint);
        println!("[install] 警告: 该插件可在你的用户权限下执行任意代码");
        if !yes_install {
            eprintln!("[install] 未验证签名的包需要显式确认：加 --yes-install 重跑（本次未安装）");
        }
        yes_install
    });
    match result {
        Ok(report) => {
            println!(
                "[install] {} -> {}（profile_added={}，signature={}）",
                report.install.name,
                report.install.destination.display(),
                report.install.added_to_profile,
                report.trust.signature_status.as_str()
            );
            println!(
                "[install] profile={}；重启 DSHR 后加载",
                profile.path().display()
            );
            println!(
                "[install] 审计记录: {}",
                profile_dir.join("install-audit.jsonl").display()
            );
            0
        }
        Err(err) => {
            if matches!(err, dshr_profile::TrustError::NotConfirmed) {
                eprintln!("[install] 已取消：未确认安装");
                return 2;
            }
            eprintln!("[install] 安装失败（{}）: {err}", err.code());
            1
        }
    }
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// `dshr --uninstall-plugin <name>`：目录 + profile + 审计（M4 W1）。
pub(crate) fn uninstall_plugin_cli(name: String) -> i32 {
    let sidecar_dir = sidecar_dir_for_app();
    let profile_dir = profile_dir_for_app();
    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[uninstall] 加载 profile 失败: {err}");
            return 1;
        }
    };
    match uninstall_package(&sidecar_dir, &name, &mut profile) {
        Ok(report) => {
            println!(
                "[uninstall] {} version={} dir_removed={} profile_removed={}",
                report.name,
                report.version.as_deref().unwrap_or("-"),
                report.removed_dir,
                report.removed_profile_entry
            );
            if let Some(error) = report.audit_error {
                eprintln!("[uninstall] 警告：审计写入失败（卸载已生效）: {error}");
            }
            println!(
                "[uninstall] 审计: {}",
                profile_dir.join("install-audit.jsonl").display()
            );
            0
        }
        Err(err) => {
            eprintln!("[uninstall] 卸载失败: {err}");
            1
        }
    }
}

fn trust_key_status(key: &TrustKey, at_ms: u64) -> &'static str {
    if key.revoked {
        "revoked"
    } else if key.not_before_ms.is_some_and(|before| at_ms < before) {
        "not-yet-valid"
    } else if key.not_after_ms.is_some_and(|after| at_ms > after) {
        "expired"
    } else {
        "active"
    }
}

/// `dshr --trust-key <list|add|remove|revoke|rotate> ...`：本机信任密钥库管理。
pub(crate) fn trust_key_cli(args: &[String]) -> i32 {
    let profile_dir = profile_dir_for_app();
    if let Err(err) = std::fs::create_dir_all(&profile_dir) {
        eprintln!("[trust-key] 创建 profile 目录失败: {err}");
        return 1;
    }
    let action = args.first().map(String::as_str).unwrap_or("list");
    let at_ms = now_millis();
    match action {
        "list" => {
            let store = match TrustStore::load(&profile_dir) {
                Ok(store) => store,
                Err(err) => {
                    eprintln!("[trust-key] 密钥库无效（{}）: {err}", err.code());
                    return 1;
                }
            };
            if store.keys().is_empty() {
                println!("[trust-key] 密钥库为空（{}）", profile_dir.display());
            }
            for key in store.keys() {
                println!(
                    "[trust-key] key_id={} status={} not_before={} not_after={} note={}",
                    key.key_id,
                    trust_key_status(key, at_ms),
                    key.not_before_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| String::from("-")),
                    key.not_after_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| String::from("-")),
                    key.note.as_deref().unwrap_or("-")
                );
            }
            0
        }
        "add" => {
            let (Some(key_id), Some(public_key_hex)) = (args.get(1), args.get(2)) else {
                eprintln!("用法: dshr --trust-key add <key_id> <public_key_hex> [--note <text>]");
                return 2;
            };
            let note = args
                .iter()
                .position(|arg| arg == "--note")
                .and_then(|index| args.get(index + 1))
                .cloned();
            let mut key = TrustKey::new(key_id, public_key_hex);
            key.note = note;
            match add_trust_key(&profile_dir, key, at_ms) {
                Ok(report) => {
                    println!(
                        "[trust-key] add {}（keys={}）",
                        report.key_id, report.key_count
                    );
                    0
                }
                Err(err) => {
                    eprintln!("[trust-key] 添加失败（{}）: {err}", err.code());
                    1
                }
            }
        }
        "remove" | "revoke" => {
            let Some(key_id) = args.get(1) else {
                eprintln!("用法: dshr --trust-key {action} <key_id>");
                return 2;
            };
            let result = if action == "remove" {
                remove_trust_key(&profile_dir, key_id, at_ms)
            } else {
                revoke_trust_key(&profile_dir, key_id, at_ms)
            };
            match result {
                Ok(report) => {
                    println!(
                        "[trust-key] {} {}（keys={}）",
                        report.action, report.key_id, report.key_count
                    );
                    0
                }
                Err(err) => {
                    eprintln!("[trust-key] {action} 失败（{}）: {err}", err.code());
                    1
                }
            }
        }
        "rotate" => {
            let (Some(old_key_id), Some(new_key_id), Some(public_key_hex)) =
                (args.get(1), args.get(2), args.get(3))
            else {
                eprintln!(
                    "用法: dshr --trust-key rotate <old_key_id> <new_key_id> <public_key_hex>"
                );
                return 2;
            };
            let new_key = TrustKey::new(new_key_id, public_key_hex);
            match rotate_trust_key(&profile_dir, old_key_id, new_key, at_ms) {
                Ok(report) => {
                    println!(
                        "[trust-key] rotate {} -> {}（keys={}）",
                        report.old_key_id.as_deref().unwrap_or("-"),
                        report.key_id,
                        report.key_count
                    );
                    0
                }
                Err(err) => {
                    eprintln!("[trust-key] 轮换失败（{}）: {err}", err.code());
                    1
                }
            }
        }
        other => {
            eprintln!(
                "未知 --trust-key 动作 `{other}`；可用: list / add / remove / revoke / rotate"
            );
            2
        }
    }
}

pub(crate) fn ed25519_keypair() -> Option<ring::signature::Ed25519KeyPair> {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).ok()?;
    ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).ok()
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 用给定 ed25519 钥重签包（自检专用；作者侧签名由 `tools/plugin-pack.mjs` 完成）。
pub(crate) fn sign_package_with_key(
    package_dir: &std::path::Path,
    name: &str,
    version: &str,
    keypair: &ring::signature::Ed25519KeyPair,
    key_id: &str,
) -> Result<String, String> {
    let digest = package_digest(package_dir).map_err(|err| err.to_string())?;
    let message = format!("dshr-trust/v1\n{name}\n{version}\n{digest}");
    let signature = keypair.sign(message.as_bytes());
    let trust = json!({
        "schema_version": 1,
        "plugin_id": name,
        "version": version,
        "package": { "digest_sha256": digest },
        "signature": {
            "scheme": "ed25519",
            "key_id": key_id,
            "value_hex": hex_lower(signature.as_ref())
        }
    });
    std::fs::write(
        package_dir.join("dshr-trust.json"),
        trust.to_string() + "\n",
    )
    .map_err(|err| format!("写 dshr-trust.json 失败: {err}"))?;
    Ok(digest)
}
