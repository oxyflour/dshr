use crate::*;

/// 沙箱 seam 自检：原型写工具走"策略 Ask → 自动批准 → 沙箱写入/快照/路径拒绝"。
pub(crate) fn selftest_sandbox() -> i32 {
    let root =
        std::path::absolute(".tmp/sandbox-ws").unwrap_or_else(|_| PathBuf::from(".tmp/sandbox-ws"));
    let _ = std::fs::remove_dir_all(&root);
    std::env::set_var("DSHR_WORKSPACE", &root);
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let path = PathBuf::from(".tmp/chat-sandbox.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(path, SESSION_ID, &cancel, ApprovalBroker::new());

    let write = |call_id: &str, relative: &str, content: &str| ToolCallRequest {
        call_id: call_id.to_owned(),
        tool: "probe_write".to_owned(),
        arguments: json!({ "path": relative, "content": content }),
    };
    let first = state.execute_tool(&write("c-w1", "demo.txt", "hello sandbox"));
    let second = state.execute_tool(&write("c-w2", "demo.txt", "v2"));
    let escaped = state.execute_tool(&write("c-w3", "../escape.txt", "nope"));

    let file = root.join("demo.txt");
    let content = std::fs::read_to_string(&file).unwrap_or_default();
    let snapshots = state
        .approvals
        .take_records()
        .into_iter()
        .filter(|record| record.tool == "probe_write")
        .count();
    let snapshot_files = std::fs::read_dir(root.join(".dshr").join("snapshots"))
        .map(|entries| entries.count())
        .unwrap_or(0);

    std::env::remove_var("DSHR_APPROVAL_MODE");
    std::env::remove_var("DSHR_WORKSPACE");

    println!(
        "[sandbox] write_ok={} overwrite_ok={} denied={} content={content:?} approval_records={snapshots} snapshot_files={snapshot_files} first_err={:?} second_err={:?}",
        !first.is_error,
        !second.is_error,
        escaped.is_error,
        first.content,
        second.content
    );
    let ok = !first.is_error
        && !second.is_error
        && escaped.is_error
        && content == "v2"
        && snapshots >= 2
        && snapshot_files >= 1;
    if ok {
        println!("[selftest-sandbox] PASS");
        0
    } else {
        println!("[selftest-sandbox] FAIL");
        1
    }
}

/// 插件 profile 自检：默认值、启停持久化、sidecar JSON、以及（有 vendored 依赖时）
/// 禁用 fs 后 read 工具消失、重新启用后回来。
pub(crate) fn selftest_profile() -> i32 {
    let _ = std::fs::create_dir_all(".tmp");
    let dir = PathBuf::from(".tmp/profile-selftest");
    let _ = std::fs::remove_dir_all(&dir);
    let mut profile = Profile::load(&dir).expect("load profile");
    let defaults_ok = profile.entries().len() == 2;
    let all_json = profile.sidecar_json();
    let disable_ok = profile.set_enabled("@deepseek-ai/dsh-tool-fs", false);
    if let Err(err) = profile.save() {
        println!("[selftest-profile] 保存失败: {err}");
        return 1;
    }
    let disabled_json = profile.sidecar_json();
    let reloaded = Profile::load(&dir).expect("reload profile");
    let persisted = reloaded
        .entries()
        .iter()
        .any(|entry| entry.name == "@deepseek-ai/dsh-tool-fs" && !entry.enabled);
    let json_ok = all_json.contains("dsh-tool-fs") && !disabled_json.contains("dsh-tool-fs");

    let vendored = PathBuf::from("sidecar/dsh-official/node_modules").exists();
    let mut sidecar_ok = true;
    if vendored {
        std::env::set_var("DSHR_SIDECAR", "dsh");
        std::env::set_var("DSHR_DSH_PLUGINS", disabled_json.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        let without_fs = ChatState::open_diagnostics(
            PathBuf::from(".tmp/chat-profile-without-fs.jsonl"),
            SESSION_ID,
            &cancel,
            ApprovalBroker::new(),
        );
        let read_absent = without_fs.plugins.registry().get("read").is_none();
        let workspace_present = without_fs
            .plugins
            .registry()
            .get("load_workspace_dependencies")
            .is_some();

        let mut reopened = Profile::load(&dir).expect("reload");
        reopened.set_enabled("@deepseek-ai/dsh-tool-fs", true);
        let _ = reopened.save();
        std::env::set_var("DSHR_DSH_PLUGINS", reopened.sidecar_json());
        let with_fs = ChatState::open_diagnostics(
            PathBuf::from(".tmp/chat-profile-with-fs.jsonl"),
            SESSION_ID,
            &cancel,
            ApprovalBroker::new(),
        );
        let read_present = with_fs.plugins.registry().get("read").is_some();
        sidecar_ok = read_absent && workspace_present && read_present;
        println!(
            "[profile] vendored=true read_absent={read_absent} workspace_present={workspace_present} read_present={read_present}"
        );
    } else {
        println!("[profile] vendored=false（跳过 sidecar 端到端）");
    }

    let ok = defaults_ok && disable_ok && persisted && json_ok && sidecar_ok;
    println!(
        "[profile] defaults_ok={defaults_ok} disable_ok={disable_ok} persisted={persisted} json_ok={json_ok}"
    );
    if ok {
        println!("[selftest-profile] PASS");
        0
    } else {
        println!("[selftest-profile] FAIL");
        1
    }
}

/// Loader YAML 自检：bundle 条目 + patch 覆盖/插入 → 只读 profile。
pub(crate) fn selftest_loader() -> i32 {
    const BUNDLE: &str = r#"
- name: "@deepseek-ai/dsh-tool-workspace-dependencies"
  config:
    source: "C:/dsh/primary-runtime"
- id: fs-tools
  name: "@deepseek-ai/dsh-tool-fs"
  inject: [tools, fs, systemPrompt]
  config:
    read: true
    write: false
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

    let dir = PathBuf::from(".tmp/loader-selftest");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let bundle = dir.join("cordis.yml");
    let patch = dir.join("cordis.patch.yml");
    if let Err(err) = std::fs::write(&bundle, BUNDLE) {
        println!("[selftest-loader] 写 bundle 失败: {err}");
        return 1;
    }
    if let Err(err) = std::fs::write(&patch, PATCH) {
        println!("[selftest-loader] 写 patch 失败: {err}");
        return 1;
    }

    let profile = match Profile::from_loader(&dir, &bundle, &[patch]) {
        Ok(profile) => profile,
        Err(err) => {
            println!("[selftest-loader] 加载失败: {err}");
            return 1;
        }
    };
    let entries = profile.entries();
    let fs = entries
        .iter()
        .find(|entry| entry.name == "@deepseek-ai/dsh-tool-fs");
    let extra = entries
        .iter()
        .find(|entry| entry.name == "@deepseek-ai/dsh-tool-extra");
    let json = profile.sidecar_json();

    let ok = profile.source() == ProfileSource::Loader
        && !profile.can_edit()
        && profile.save().is_err()
        && fs.is_some_and(|entry| !entry.enabled && entry.config["write"] == true)
        && extra.is_some()
        && !json.contains("dsh-tool-fs")
        && json.contains("dsh-tool-extra");
    println!(
        "[loader] entries={} fs_disabled={:?} extra={} json_has_fs={} read_only={}",
        entries.len(),
        fs.map(|entry| !entry.enabled),
        extra.is_some(),
        json.contains("dsh-tool-fs"),
        !profile.can_edit()
    );
    if ok {
        println!("[selftest-loader] PASS");
        0
    } else {
        println!("[selftest-loader] FAIL");
        1
    }
}
