use crate::*;

/// 损坏会话日志自检：加载失败必须**隔离原文件**并新建空会话，绝不能把原文件覆盖成
/// 1 行 `session.created`（review P1-1）。
pub(crate) fn selftest_corrupt_session() -> i32 {
    let root = PathBuf::from(".tmp/chat-corrupt-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);

    // 先造一份合法日志，再追加半截记录模拟中断写入。
    let valid = {
        let mut log = SessionLog::new("s-corrupt");
        let _ = log.append_created(now_ts(), &SessionCreated::root(String::from(".")));
        log.to_jsonl()
    };
    let corrupt_bytes = format!("{valid}{{\"session_id\":\"s-corrupt\",\"seq\":2");
    let path = root.join("s-corrupt.jsonl");
    std::fs::write(&path, &corrupt_bytes).expect("write corrupt fixture");

    let cancel = Arc::new(AtomicBool::new(false));
    let state = ChatState::open_diagnostics_with_options(
        path.clone(),
        "s-corrupt",
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );

    // 原文件被隔离，字节完整保留。
    let quarantined: Vec<PathBuf> = std::fs::read_dir(&root)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|candidate| {
                    candidate
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name.contains(".corrupt-"))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    let quarantined_bytes = quarantined
        .first()
        .and_then(|candidate| std::fs::read_to_string(candidate).ok())
        .unwrap_or_default();

    // 新会话文件存在且只含一条 session.created（未被损坏内容污染）。
    let fresh_text = std::fs::read_to_string(&path).unwrap_or_default();
    let fresh_ok = SessionLog::from_jsonl("s-corrupt", &fresh_text)
        .map(|log| log.events().len() == 1 && log.events()[0].event_type == "session.created")
        .unwrap_or(false);

    let reported = state
        .session_errors
        .iter()
        .any(|message| message.contains("会话日志损坏") && message.contains("隔离"));
    let preserved = quarantined_bytes == corrupt_bytes;

    println!(
        "[corrupt-session] quarantined={} preserved={preserved} fresh_ok={fresh_ok} reported={reported}",
        quarantined.len()
    );
    let ok = quarantined.len() == 1 && preserved && fresh_ok && reported;
    if ok {
        println!("[selftest-corrupt-session] PASS");
        0
    } else {
        println!("[selftest-corrupt-session] FAIL");
        1
    }
}

/// 故意在注册后失败的插件：宿主必须原子回滚，且仍能装载后续健康插件。
struct RecoveryFailingPlugin;

impl Plugin for RecoveryFailingPlugin {
    fn id(&self) -> &'static str {
        "recovery.failing"
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "recovery_fail_tool",
                "should be rolled back",
                ToolOrigin::Native {
                    plugin_id: "recovery.failing".to_owned(),
                },
            )
            .with_schema(json!({ "type": "object" })),
        )?;
        Err(PluginError::Invalid("injected apply failure".to_owned()))
    }

    fn call(
        &mut self,
        _tool: &str,
        _arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        unreachable!("failing plugin is rolled back")
    }
}

/// 失败插件之后装载的健康插件。
struct RecoveryHealthyPlugin;

impl Plugin for RecoveryHealthyPlugin {
    fn id(&self) -> &'static str {
        "recovery.healthy"
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "recovery_healthy_echo",
                "healthy plugin tool",
                ToolOrigin::Native {
                    plugin_id: "recovery.healthy".to_owned(),
                },
            )
            .with_schema(json!({ "type": "object" })),
        )?;
        Ok(())
    }

    fn call(
        &mut self,
        _tool: &str,
        _arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        Ok(json!({ "ok": true }))
    }
}

/// `--selftest-recovery`：G2 数据/插件失败恢复（无窗口）。
///
/// 1. 损坏会话：隔离原字节、生成干净 `session.created`、错误可见（复用
///    `--selftest-corrupt-session`）；
/// 2. 备份恢复：删掉主文件后 `.bak` 原子恢复，重放成功并可继续追加；
/// 3. 插件失败：`apply` 注册后失败被回滚，随后健康插件可装载/调用。
pub(crate) fn selftest_recovery() -> i32 {
    let corrupt_ok = selftest_corrupt_session() == 0;

    let root = PathBuf::from(".tmp/m4-recovery");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let path = root.join("s-recovery.jsonl");
    let mut log = SessionLog::new("s-recovery");
    let _ = log.append_created(now_ts(), &SessionCreated::root(String::from(".")));
    let first_len = log.events().len();
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());
    let _ = log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "before crash" }),
    ));
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());

    // 模拟 Windows 替换窗口：主文件缺失、`.bak` 是上一次成功版本。
    std::fs::remove_file(&path).expect("remove main log");
    let recovered = recover_backup(&path).unwrap_or(false);
    let replayed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-recovery", &text).ok());
    let backup_ok = recovered
        && replayed
            .as_ref()
            .map(|log| log.events().len() == first_len)
            .unwrap_or(false);
    let continue_ok = replayed.is_some_and(|mut log| {
        let _ = log.append(EventSpec::durable(
            "message.user",
            now_ts(),
            json!({ "text": "after recovery" }),
        ));
        save_atomic(&path, log.to_jsonl().as_bytes()).is_ok()
            && std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| SessionLog::from_jsonl("s-recovery", &text).ok())
                .map(|log| log.events().len() == first_len + 1)
                .unwrap_or(false)
    });

    let mut host = PluginHost::new(AllowAll);
    let rollback_ok =
        host.load(Box::new(RecoveryFailingPlugin)).is_err() && host.registry().is_empty();
    let healthy_ok = host.load(Box::new(RecoveryHealthyPlugin)).is_ok();
    let continue_plugin_ok = host.call("recovery_healthy_echo", &json!({})).is_ok();

    let ok =
        corrupt_ok && backup_ok && continue_ok && rollback_ok && healthy_ok && continue_plugin_ok;
    println!(
        "[selftest-recovery] corrupt={corrupt_ok} backup={backup_ok} continue={continue_ok} plugin_rollback={rollback_ok} healthy={healthy_ok} healthy_call={continue_plugin_ok}"
    );
    if ok {
        println!("[selftest-recovery] PASS");
        0
    } else {
        println!("[selftest-recovery] FAIL");
        1
    }
}

/// `--selftest-crash-child <path>`：模拟进程在保存后崩溃（主文件缺失、`.bak` 完整），
/// 由父进程 `--selftest-crash-recovery` 验证恢复与继续追加。
pub(crate) fn selftest_crash_child(path: &str) -> i32 {
    let path = PathBuf::from(path);
    let _ = std::fs::remove_file(&path);
    let mut log = SessionLog::new("s-crash");
    let _ = log.append_created(now_ts(), &SessionCreated::root(String::from(".")));
    let _ = log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "before crash" }),
    ));
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());
    // 第二次保存产生 `.bak`（上一版含 before crash），再删主文件模拟替换窗口崩溃。
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());
    let _ = std::fs::remove_file(&path);
    std::process::abort();
}

/// `--selftest-crash-recovery`：真实子进程崩溃 → `.bak` 恢复 → 继续追加。
pub(crate) fn selftest_crash_recovery() -> i32 {
    let root = PathBuf::from(".tmp/m4-crash-recovery");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let path = root.join("s-crash.jsonl");
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            println!("[selftest-crash-recovery] FAIL: current_exe: {error}");
            return 1;
        }
    };
    let status = std::process::Command::new(exe)
        .arg("--selftest-crash-child")
        .arg(&path)
        .status();
    let crashed = status.map(|status| !status.success()).unwrap_or(false);
    let backup_exists = path.with_extension("jsonl.bak").exists();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics_with_options(
        path.clone(),
        "s-crash",
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );
    let before_ok = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-crash", &text).ok())
        .map(|log| {
            log.events().iter().any(|event| {
                event.event_type == "message.user"
                    && event.payload["text"].as_str() == Some("before crash")
            })
        })
        .unwrap_or(false);
    let _ = state.log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "after crash" }),
    ));
    state.save();
    let after_ok = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-crash", &text).ok())
        .map(|log| {
            log.events().iter().any(|event| {
                event.event_type == "message.user"
                    && event.payload["text"].as_str() == Some("after crash")
            })
        })
        .unwrap_or(false);
    let ok = crashed && backup_exists && before_ok && after_ok;
    let notice_ok = state
        .session_errors
        .iter()
        .any(|message| message.starts_with("已恢复："));
    println!(
        "[selftest-crash-recovery] crashed={crashed} backup={backup_exists} before={before_ok} after={after_ok} notice={notice_ok}"
    );
    if ok && notice_ok {
        println!("[selftest-crash-recovery] PASS");
        0
    } else {
        println!("[selftest-crash-recovery] FAIL");
        1
    }
}

/// `--selftest-window-recovery`：G2 真窗口端到端。
///
/// 1. 用 `--selftest-crash-child` 制造真实崩溃现场：主文件缺失、`.bak` 完整；
/// 2. 启动 GUI 子进程（`DSHR_RECOVERY_FLOW=1`）：会话索引必须发现孤儿 `.bak`，
///    启动恢复后显示横幅，并在真实窗口里输入/发送 “after recovery”；
/// 3. 父进程校验子进程 exit 0，且最终日志同时含 “before crash” 与 “after recovery”。
pub(crate) fn selftest_window_recovery() -> i32 {
    let root = std::path::absolute(PathBuf::from(".tmp/m4-window-recovery"))
        .unwrap_or_else(|_| PathBuf::from(".tmp/m4-window-recovery"));
    let sessions = root.join("sessions");
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(&sessions) {
        eprintln!(
            "[selftest-window-recovery] FAIL: 创建临时目录 {} 失败: {error}",
            sessions.display()
        );
        return 1;
    }
    let path = sessions.join("s-crash.jsonl");
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            println!("[selftest-window-recovery] FAIL: current_exe: {error}");
            return 1;
        }
    };

    // 真实子进程崩溃：退出码非 0，留下 `<path>.bak` 并删掉主文件。
    let crashed = std::process::Command::new(&exe)
        .arg("--selftest-crash-child")
        .arg(&path)
        .status()
        .map(|status| !status.success())
        .unwrap_or(false);
    let backup = path.with_extension("jsonl.bak");
    let backup_exists = backup.exists();

    // GUI 子进程：真实窗口 + 恢复后的继续输入/发送。stdout/stderr 继承，CI 日志可见。
    let child_status = std::process::Command::new(&exe)
        .arg("--selftest-window-recovery-child")
        .env("DSHR_HOME", &root)
        .env("DSHR_SESSIONS_DIR", &sessions)
        .env("DSHR_PROFILE_DIR", root.join("profile"))
        .env("DSHR_RECOVERY_FLOW", "1")
        .env("DSHR_FORCE_MOCK", "1")
        .status();
    let child_ok = child_status.map(|status| status.success()).unwrap_or(false);

    let log = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-crash", &text).ok());
    let before_ok = log.as_ref().is_some_and(|log| {
        log.events().iter().any(|event| {
            event.event_type == "message.user"
                && event.payload["text"].as_str() == Some("before crash")
        })
    });
    let after_ok = log.as_ref().is_some_and(|log| {
        log.events().iter().any(|event| {
            event.event_type == "message.user"
                && event.payload["text"].as_str() == Some("after recovery")
        })
    });

    println!(
        "[selftest-window-recovery] crashed={crashed} backup={backup_exists} child={child_ok} before={before_ok} after={after_ok}"
    );
    if crashed && backup_exists && child_ok && before_ok && after_ok {
        println!("[selftest-window-recovery] PASS");
        0
    } else {
        println!("[selftest-window-recovery] FAIL");
        1
    }
}

/// `--selftest-sidecar-recovery`：G2 的 sidecar 崩溃场景（headless）。
///
/// 先确认 sidecar 可调用，再 kill 子进程；断言会话仍可追加/保存、原生工具仍注册，
/// 且对已死 sidecar 的调用返回错误而不是挂起。
pub(crate) fn selftest_sidecar_recovery() -> i32 {
    let root = PathBuf::from(".tmp/m4-sidecar-recovery");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let path = root.join("s-sidecar.jsonl");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics_with_options(
        path.clone(),
        "s-sidecar",
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );

    let Some(mut host) = spawn_sidecar(None, None) else {
        println!("[selftest-sidecar-recovery] FAIL: sidecar 不可用（需要 node + sidecar/probe-*）");
        return 1;
    };
    let live = host
        .invoke(
            "probe_echo",
            json!({ "text": "live" }),
            Duration::from_secs(10),
        )
        .is_ok();
    let _ = state.log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "before kill" }),
    ));
    state.save();
    state.host = Some(host);

    let killed = state
        .host
        .as_mut()
        .map(|host| host.kill().is_ok())
        .unwrap_or(false);
    let dead_call = state
        .host
        .as_mut()
        .map(|host| {
            host.invoke(
                "probe_echo",
                json!({ "text": "after" }),
                Duration::from_secs(2),
            )
            .is_err()
        })
        .unwrap_or(false);
    let native_ok = state.plugins.registry().get("probe_native").is_some();
    let _ = state.log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "after kill" }),
    ));
    state.save();
    let replayed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-sidecar", &text).ok())
        .map(|log| log.events().len())
        .unwrap_or(0);
    let ok = live && killed && dead_call && native_ok && replayed >= 3;
    println!(
        "[selftest-sidecar-recovery] live={live} killed={killed} dead_call={dead_call} native={native_ok} replayed={replayed}"
    );
    if ok {
        println!("[selftest-sidecar-recovery] PASS");
        0
    } else {
        println!("[selftest-sidecar-recovery] FAIL");
        1
    }
}
