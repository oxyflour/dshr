use crate::*;

/// 用真实 Cordis 运行时跑一轮对话（`DSHR_SIDECAR=cordis`）。
pub(crate) fn selftest_cordis() -> i32 {
    let path = PathBuf::from(".tmp/chat-cordis.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state =
        ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    state.submit("用工具试一下");

    let items = state.items();
    println!("[selftest-cordis] items={}", items.len());
    for item in &items {
        let row = describe(item);
        match row.sub_text() {
            Some(sub) => println!("  {}\n      ↳ {sub}", row.main_text()),
            None => println!("  {}", row.main_text()),
        }
    }

    let ok = items.len() == 3
        && matches!(&items[1], Item::Tool(t) if t.tool == "cordis_echo"
            && t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false });

    let replayed = build_from_log(
        &SessionLog::from_jsonl(
            SESSION_ID,
            &std::fs::read_to_string(&path).unwrap_or_default(),
        )
        .expect("reload cordis session"),
    );
    let same = replayed == items;
    println!("[selftest-cordis] replay_equal={same}");

    if ok && same {
        println!("[selftest-cordis] PASS");
        0
    } else {
        println!("[selftest-cordis] FAIL");
        1
    }
}

/// 验证普通桌面启动默认注册的官方 DSH 插件。
///
/// 装载的是**未修改的官方包**（`@deepseek-ai/dsh-tool-workspace-dependencies`
/// 与只读配置的 `@deepseek-ai/dsh-tool-fs`），通过 `tools` 服务的 `register`
/// 进入 Rust Registry，再按普通 sidecar 工具调用。
///
/// 依赖 `sidecar/dsh-official/node_modules`（`node tools/dsh-vendor.mjs` 从本机 DSH 抽取）。
/// 缺失时打印 SKIP 并返回 0：CI 环境不一定装了 DSH。
pub(crate) fn selftest_dsh() -> i32 {
    // 资源根按 exe 相对解析（打包后不依赖源码树）。
    let root = app_root();
    if !official_sidecar_ready(&root.join("sidecar/dsh-official")) {
        println!("[selftest-dsh] SKIP: 先运行 `node tools/dsh-vendor.mjs` 准备官方插件依赖");
        return 0;
    }

    let fixture = tempfile::tempdir_in(data_root()).expect("create DSH selftest workspace");
    let path = fixture.path().join("session.jsonl");
    let contents = "dshr desktop sidecar read fixture";
    std::fs::write(fixture.path().join("read-test.txt"), contents).expect("write DSH fixture");
    std::env::set_var("DSHR_WORKSPACE", fixture.path());
    std::env::remove_var("DSHR_SIDECAR");
    prepare_desktop_sidecar(&Profile::defaults(fixture.path()));
    // 本自检只验证"只读官方插件 + 回放"；写工具由 --selftest-dsh-write 覆盖。
    std::env::set_var("DSHR_FS_WRITE", "0");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state =
        ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    // 官方插件经 ctx.tools.register 映射进同一个 Registry，origin 必须是 sidecar。
    let registered_workspace = state
        .plugins
        .registry()
        .get("load_workspace_dependencies")
        .is_some_and(|tool| tool.origin.kind() == "sidecar");
    let registered_read = state
        .plugins
        .registry()
        .get("read")
        .is_some_and(|tool| tool.origin.kind() == "sidecar");

    state.submit("用工具试一下");

    let items = state.items();
    println!(
        "[selftest-dsh] items={} registered(workspace={registered_workspace}, read={registered_read})",
        items.len()
    );
    for item in &items {
        let row = describe(item);
        match row.sub_text() {
            Some(sub) => println!("  {}\n      ↳ {sub}", row.main_text()),
            None => println!("  {}", row.main_text()),
        }
    }

    let tool_ok = matches!(&items[1], Item::Tool(t)
        if t.tool == "load_workspace_dependencies"
            && t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false });
    // 官方 tool 的真实返回值里必须有 python 路径（证明 execute 真的跑了）。
    let value_ok = matches!(&items[1], Item::Tool(t)
        if t.card.as_ref().is_some_and(|card| card.detail.contains("python")));

    // 直接调官方 read 工具：证明带依赖（fs/systemPrompt）的插件也能执行。
    let read = state.execute_tool(&ToolCallRequest {
        call_id: "selftest-read-1".to_owned(),
        tool: "read".to_owned(),
        arguments: json!({ "file_path": "read-test.txt" }),
    });
    let read_ok = !read.is_error && read.content.to_string().contains(contents);
    println!("[selftest-dsh] read_tool_ok={read_ok}");

    let replayed = build_from_log(
        &SessionLog::from_jsonl(
            SESSION_ID,
            &std::fs::read_to_string(&path).unwrap_or_default(),
        )
        .expect("reload dsh session"),
    );
    let same = replayed == items;
    println!("[selftest-dsh] replay_equal={same}");

    if registered_workspace && registered_read && tool_ok && value_ok && read_ok && same {
        println!("[selftest-dsh] PASS");
        0
    } else {
        println!("[selftest-dsh] FAIL");
        1
    }
}

/// Gate 4 证据（最关键分界线）：官方 `dsh-tool-fs` 的 write/edit/apply_patch 在
/// "Rust 审批 + 沙箱"约束下真正可用，官方插件源码一行未改。
///
/// 校验：能力协商后四个工具都注册 → write/edit/apply_patch 真实落盘 → 越界被拒 →
/// 审批 deny 时工具报错且文件不变 → 写前快照存在。
pub(crate) fn selftest_dsh_write() -> i32 {
    // 资源根按 exe 相对解析（打包后不依赖源码树）。
    let root = app_root();
    if !official_sidecar_ready(&root.join("sidecar/dsh-official")) {
        println!("[selftest-dsh-write] SKIP: 先运行 `node tools/dsh-vendor.mjs`");
        return 2;
    }
    let ws = std::path::absolute(".tmp/dsh-write-ws")
        .unwrap_or_else(|_| PathBuf::from(".tmp/dsh-write-ws"));
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::create_dir_all(&ws);
    std::env::set_var("DSHR_WORKSPACE", &ws);
    std::env::set_var("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    // 用全新 profile：旧 profile 文件可能还留着 write:false 的历史默认值。
    let profile_dir = PathBuf::from(".tmp/profile-dsh-write");
    let _ = std::fs::remove_dir_all(&profile_dir);
    let profile = Profile::defaults(&profile_dir);
    std::env::set_var("DSHR_DSH_PLUGINS", profile.sidecar_json());

    let path = PathBuf::from(".tmp/chat-dsh-write.jsonl");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(path, SESSION_ID, &cancel, ApprovalBroker::new());

    let registered: Vec<String> = state
        .plugins
        .registry()
        .list()
        .iter()
        .map(|definition| definition.name.clone())
        .collect();
    let has = |name: &str| registered.iter().any(|candidate| candidate == name);
    if !(has("write") && has("edit") && has("apply_patch") && has("read")) {
        println!("[dsh-write] registered={registered:?}");
        println!("[selftest-dsh-write] FAIL: 写工具未注册（host_caps/能力协商问题）");
        return 1;
    }

    let call = |tool: &str, arguments: serde_json::Value| ToolCallRequest {
        call_id: format!("dw-{tool}-{}", now_ts()),
        tool: tool.to_owned(),
        arguments,
    };
    let read_file = |name: &str| std::fs::read_to_string(ws.join(name)).unwrap_or_default();

    let write = state.execute_tool(&call(
        "write",
        json!({ "file_path": "hello.txt", "content": "hello dsh write" }),
    ));
    let wrote = !write.is_error && read_file("hello.txt") == "hello dsh write";

    let edit = state.execute_tool(&call(
        "edit",
        json!({ "file_path": "hello.txt", "old_string": "hello", "new_string": "hi" }),
    ));
    let edited = !edit.is_error && read_file("hello.txt") == "hi dsh write";

    let patch = state.execute_tool(&call(
        "apply_patch",
        json!({ "input": "*** Begin Patch\n*** Add File: notes.txt\n+patch line\n*** End Patch\n" }),
    ));
    let patched = !patch.is_error && read_file("notes.txt").contains("patch line");

    // 越界写：Rust 沙箱必须拒绝，且不落盘。
    let escaped = state.execute_tool(&call(
        "write",
        json!({ "file_path": "../escape.txt", "content": "nope" }),
    ));
    let escaped_rejected = escaped.is_error && !ws.join("../escape.txt").exists();

    // 审批拒绝：工具报错、文件内容不变。
    std::env::set_var("DSHR_APPROVAL_MODE", "deny");
    let denied = state.execute_tool(&call(
        "write",
        json!({ "file_path": "hello.txt", "content": "should not land" }),
    ));
    let denied_kept = denied.is_error && read_file("hello.txt") == "hi dsh write";

    let snapshots = std::fs::read_dir(ws.join(".dshr").join("snapshots"))
        .map(|dir| dir.count())
        .unwrap_or(0);
    let ok = wrote && edited && patched && escaped_rejected && denied_kept && snapshots > 0;
    println!(
        "[dsh-write] registered={registered:?} write={wrote} edit={edited} patch={patched} \
         escape_rejected={escaped_rejected} deny_kept={denied_kept} snapshots={snapshots}"
    );
    std::env::remove_var("DSHR_APPROVAL_MODE");
    if ok {
        println!("[selftest-dsh-write] PASS");
        0
    } else {
        println!("[selftest-dsh-write] FAIL");
        1
    }
}

/// 审批自检：策略对全部工具 Ask，
/// (1) `DSHR_APPROVAL_MODE=deny`：工具不执行、落 approval 事件、工具结果为空错误；
/// (2) `DSHR_APPROVAL_MODE=allow`：审批通过、工具真正执行。
pub(crate) fn selftest_approval() -> i32 {
    let _ = std::fs::create_dir_all(".tmp");
    std::env::set_var("DSHR_TOOL_POLICY", "ask-all");

    let denied_path = PathBuf::from(".tmp/chat-approval-deny.jsonl");
    reset_session_file(&denied_path);
    std::env::set_var("DSHR_APPROVAL_MODE", "deny");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut denied_state =
        ChatState::open_diagnostics(denied_path, SESSION_ID, &cancel, ApprovalBroker::new());
    let denied_reply = denied_state.stream_turn("用工具试一下", &mut |_| {});

    let event_count = |state: &ChatState, kind: &str| {
        state
            .log
            .events()
            .iter()
            .filter(|event| event.event_type == kind)
            .count()
    };
    // P1-3：审批 Deny 属于“工具执行失败”，必须写 tool.result(is_error=true)，
    // 而不是 tool.aborted（aborted 留给真正的取消/崩溃语义）。
    let denied_tool_result = denied_state.log.events().iter().any(|event| {
        event.event_type == "tool.result"
            && event
                .payload
                .get("is_error")
                .and_then(|value| value.as_bool())
                == Some(true)
    });
    let denied_tool_aborted = event_count(&denied_state, "tool.aborted") > 0;
    let asked = event_count(&denied_state, "approval.asked");
    let answered = event_count(&denied_state, "approval.answered");
    let deny_outcome = denied_state.log.events().iter().any(|event| {
        event.event_type == "approval.answered"
            && event.payload.get("outcome").and_then(|v| v.as_str()) == Some("deny")
    });

    let allowed_path = PathBuf::from(".tmp/chat-approval-allow.jsonl");
    reset_session_file(&allowed_path);
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let mut allowed_state =
        ChatState::open_diagnostics(allowed_path, SESSION_ID, &cancel, ApprovalBroker::new());
    let allowed_reply = allowed_state.stream_turn("用工具试一下", &mut |_| {});
    let allowed_tool = allowed_state.log.events().iter().any(|event| {
        event.event_type == "tool.result"
            && event
                .payload
                .get("is_error")
                .and_then(|value| value.as_bool())
                == Some(false)
    });
    let allow_outcome = allowed_state.log.events().iter().any(|event| {
        event.event_type == "approval.answered"
            && event.payload.get("outcome").and_then(|v| v.as_str()) == Some("allow_once")
    });

    std::env::remove_var("DSHR_APPROVAL_MODE");
    std::env::remove_var("DSHR_TOOL_POLICY");

    println!(
        "[approval] asked={asked} answered={answered} denied_tool_result={denied_tool_result} \
         denied_tool_aborted={denied_tool_aborted} deny_outcome={deny_outcome} \
         allowed_tool={allowed_tool} allow_outcome={allow_outcome}"
    );
    let ok = denied_reply.is_some()
        && allowed_reply.is_some()
        && denied_tool_result
        && !denied_tool_aborted
        && asked == 1
        && answered == 1
        && deny_outcome
        && allowed_tool
        && allow_outcome;
    if ok {
        println!("[selftest-approval] PASS");
        0
    } else {
        println!("[selftest-approval] FAIL");
        1
    }
}

/// `--selftest-official`：打包后的官方 sidecar 自检（不依赖源码树 fixture）。
///
/// 从 `app_root()/sidecar/dsh-official` 启动 vendored sidecar，断言 v2 握手、
/// host services active、白名单 3 张表；用于验证 zip/tar 包里的 node_modules 与
/// `runtime/node` 布局。失败即 exit 1，不做 SKIP。
pub(crate) fn selftest_official_sidecar() -> i32 {
    let dsh_dir = app_root().join("sidecar/dsh-official");
    if !official_sidecar_ready(&dsh_dir) {
        eprintln!(
            "[selftest-official] FAIL 缺少官方 sidecar: {}",
            dsh_dir.display()
        );
        return 1;
    }
    let l2 = ChatSessionAccess::new(sessions_dir_from_env());
    let env_keys = [
        "DSHR_SIDECAR",
        "DSHR_DSH_PLUGINS",
        "DSHR_HOST_SERVICES",
        "DSHR_FS_WRITE",
    ];
    let saved: Vec<Option<std::ffi::OsString>> = env_keys.iter().map(std::env::var_os).collect();
    std::env::set_var("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_DSH_PLUGINS", "[]");
    std::env::set_var("DSHR_HOST_SERVICES", "1");
    std::env::set_var("DSHR_FS_WRITE", "0");
    let host = spawn_sidecar(None, Some(l2));
    for (key, value) in env_keys.iter().zip(saved) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    match host {
        None => {
            eprintln!("[selftest-official] FAIL 官方 sidecar 启动失败");
            1
        }
        Some(host) => {
            let protocol = host.negotiated_protocol();
            let active = host.host_services_active();
            let tables = host
                .host_services_payload()
                .and_then(|payload| payload.as_array().map(Vec::len))
                .unwrap_or(0);
            drop(host);
            if protocol == 2 && active && tables >= 4 {
                println!(
                    "[selftest-official] PASS protocol={protocol} active={active} host_services={tables}"
                );
                0
            } else {
                eprintln!(
                    "[selftest-official] FAIL protocol={protocol} active={active} host_services={tables}"
                );
                1
            }
        }
    }
}

/// `--selftest-l2`：v2 Host Service 的 CLI 层 smoke（不依赖真实 DSH 插件执行）。
///
/// Phase A（可选，需要 vendored 官方 sidecar）：真握手到 v2，断言
/// `negotiated_protocol == 2`、`host_services_active`、白名单 3 张表。
/// Phase B（始终）：用 compat-dsh-host 的 fixture sidecar 跑
/// `storage-kv/v1` open→putRecord→loadAll 的真实 `host.service.request` 往返。
pub(crate) fn selftest_l2() -> i32 {
    let mut pass = true;

    // Phase A：真实官方 sidecar（无 vendored 依赖时 SKIP，不算失败）。
    let dsh_dir = app_root().join("sidecar/dsh-official");
    if official_sidecar_ready(&dsh_dir) {
        let l2 = ChatSessionAccess::new(sessions_dir_from_env());
        let env_keys = [
            "DSHR_SIDECAR",
            "DSHR_DSH_PLUGINS",
            "DSHR_HOST_SERVICES",
            "DSHR_FS_WRITE",
        ];
        let saved: Vec<Option<std::ffi::OsString>> =
            env_keys.iter().map(std::env::var_os).collect();
        std::env::set_var("DSHR_SIDECAR", "dsh");
        std::env::set_var("DSHR_DSH_PLUGINS", "[]");
        std::env::set_var("DSHR_HOST_SERVICES", "1");
        std::env::set_var("DSHR_FS_WRITE", "0");
        let host = spawn_sidecar(None, Some(l2));
        for (key, value) in env_keys.iter().zip(saved) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        match host {
            None => println!("[selftest-l2] SKIP phase A: 官方 sidecar 启动失败（环境受限）"),
            Some(host) => {
                let protocol = host.negotiated_protocol();
                let active = host.host_services_active();
                let tables = host
                    .host_services_payload()
                    .and_then(|payload| payload.as_array().map(Vec::len))
                    .unwrap_or(0);
                drop(host);
                if protocol == 2 && active && tables >= 4 {
                    println!(
                        "[selftest-l2] phase A PASS protocol={protocol} active={active} host_services={tables}"
                    );
                } else {
                    eprintln!(
                        "[selftest-l2] phase A FAIL protocol={protocol} active={active} host_services={tables}"
                    );
                    pass = false;
                }
            }
        }
    } else {
        println!(
            "[selftest-l2] SKIP phase A: 未找到 vendored 官方 sidecar（先运行 node tools/dsh-vendor.mjs）"
        );
    }

    // Phase B：fixture sidecar 的真实 host.service.request 往返（storage-kv）。
    let fixture = std::path::absolute(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/compat-dsh-host/tests/fixtures/l2-sidecar.mjs"),
    )
    .unwrap_or_else(|_| PathBuf::from("crates/compat-dsh-host/tests/fixtures/l2-sidecar.mjs"));
    if !fixture.exists() {
        eprintln!(
            "[selftest-l2] FAIL phase B: fixture 不存在: {}",
            fixture.display()
        );
        return 1;
    }
    let root = data_root()
        .join("selftests")
        .join(format!("l2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(&root) {
        // review P2-4：不再用用户 `%TEMP%`，失败要打印真实路径与原因。
        eprintln!(
            "[selftest-l2] FAIL phase B: 创建临时目录 {} 失败: {error}",
            root.display()
        );
        return 1;
    }
    let access = ChatSessionAccess::new(root.join("sessions"));
    let router = match build_l2_router_at(
        access,
        root.join("storage"),
        root.join("workspace"),
        "l2.fixture",
        ProcessServices::default(),
        false,
    ) {
        Ok(router) => router,
        Err(err) => {
            eprintln!("[selftest-l2] FAIL phase B: {err}");
            return 1;
        }
    };
    let options = HostOptions::default()
        .with_env("L2_SCENARIO", "storage")
        .with_registration_window(Duration::from_secs(3))
        .with_host_services(router);
    match SidecarHost::spawn_with(&fixture, &fixture, options) {
        Err(err) => {
            eprintln!("[selftest-l2] FAIL phase B: spawn fixture: {err}");
            pass = false;
        }
        Ok(mut host) => {
            let protocol = host.negotiated_protocol();
            let active = host.host_services_active();
            match host.invoke("l2_probe", json!({}), Duration::from_secs(15)) {
                Ok(outcome) => {
                    let steps = &outcome.terminal.payload["value"]["steps"];
                    let open_ok = steps[0]["ok"].as_bool().unwrap_or(false);
                    let put_ok = steps[1]["ok"].as_bool().unwrap_or(false);
                    let load_n = steps[2]["result"]["tables"]["items"]["a"]["n"].as_i64();
                    let unit_id = steps[0]["result"]["unit_id"]
                        .as_str()
                        .unwrap_or("<none>")
                        .to_owned();
                    if protocol == 2 && active && open_ok && put_ok && load_n == Some(1) {
                        println!(
                            "[selftest-l2] phase B PASS protocol={protocol} active={active} unit_id={unit_id} roundtrip=ok"
                        );
                    } else {
                        eprintln!(
                            "[selftest-l2] phase B FAIL protocol={protocol} active={active} payload={}",
                            outcome.terminal.payload
                        );
                        pass = false;
                    }
                }
                Err(err) => {
                    eprintln!("[selftest-l2] FAIL phase B: invoke: {err}");
                    pass = false;
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    if pass {
        println!("[selftest-l2] PASS");
        0
    } else {
        println!("[selftest-l2] FAIL");
        1
    }
}

/// `--selftest-parallel-tools`：M3 并行工具的 CLI 层证据（串行 vs 并行墙钟 + 顺序）。
///
/// 用 probe sidecar 的 `probe_slow`（每个 ≈2s）跑 3 个：串行 ≈6s、并行 ≈2s；
/// 另用 `probe_echo` 验证 `execute_batch` 的结果按原顺序返回。取消/超时/迟到语义
/// 由 `compat-dsh-host` 的 `M3-PARALLEL-101..107` 覆盖。
pub(crate) fn selftest_parallel_tools() -> i32 {
    let path = PathBuf::from(".tmp/chat-parallel-tools.jsonl");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics_with_options(
        path,
        "s-parallel",
        &cancel,
        ApprovalBroker::new(),
        false,
        true,
    );
    if state.host.is_none() {
        println!(
            "[selftest-parallel-tools] SKIP: sidecar unavailable（找不到 node/probe sidecar）"
        );
        return 0;
    }
    let log_cell = std::rc::Rc::new(std::cell::RefCell::new(std::mem::replace(
        &mut state.log,
        SessionLog::new("s-parallel"),
    )));
    let mut executor = ChatExecutor {
        plugins: &mut state.plugins,
        host: &mut state.host,
        approvals: &state.approvals,
        creator: None,
        step: ToolStep::default(),
        program_hidden: HashSet::new(),
        headless: state.headless,
        diagnostics: state.diagnostics,
        log: log_cell.clone(),
        sessions_dir: &state.sessions_dir,
        parent_id: "s-parallel",
        workspace: &state.workspace,
        cancel: &cancel,
        settings: &state.settings,
        permission: &state.permission,
        preset_inputs: &state.preset_inputs,
        published_presets: &mut state.published_presets,
        allow_spawn: false,
        l2_access: state.l2_access.clone(),
    };

    executor.prepare_step();

    // 1) 顺序证据：3 个 probe_echo（fast），并行批返回顺序必须等于调用顺序。
    let order_calls: Vec<ToolCallRequest> = (0..3)
        .map(|index| ToolCallRequest {
            call_id: format!("parallel-order-{index}"),
            tool: "probe_echo".to_owned(),
            arguments: json!({ "i": index }),
        })
        .collect();
    let order_results = executor.execute_batch(&order_calls, 3);
    let order_ok = order_results.iter().enumerate().all(|(index, outcome)| {
        !outcome.is_error && outcome.value["echo"]["i"].as_u64() == Some(index as u64)
    });

    // 2) 墙钟证据：3 个 probe_slow（每个 ≈2s）先串行、再并行。
    let slow_calls: Vec<ToolCallRequest> = (0..3)
        .map(|index| ToolCallRequest {
            call_id: format!("parallel-slow-{index}"),
            tool: "probe_slow".to_owned(),
            arguments: json!({}),
        })
        .collect();
    let serial_started = std::time::Instant::now();
    let serial_results: Vec<ToolOutcome> = slow_calls
        .iter()
        .map(|call| executor.execute(call))
        .collect();
    let serial_elapsed = serial_started.elapsed();
    let parallel_started = std::time::Instant::now();
    let parallel_results = executor.execute_batch(&slow_calls, 3);
    let parallel_elapsed = parallel_started.elapsed();

    drop(executor);
    state.log = std::rc::Rc::try_unwrap(log_cell)
        .map(|cell| cell.into_inner())
        .unwrap_or_else(|cell| cell.borrow().clone());
    state.save();

    let all_ok = serial_results
        .iter()
        .chain(parallel_results.iter())
        .all(|outcome| !outcome.is_error && outcome.value["done"] == true);
    let ratio = if serial_elapsed.is_zero() {
        f64::INFINITY
    } else {
        parallel_elapsed.as_secs_f64() / serial_elapsed.as_secs_f64()
    };
    println!(
        "[selftest-parallel-tools] order_ok={order_ok} all_ok={all_ok} serial={serial_elapsed:?} parallel={parallel_elapsed:?} ratio={ratio:.2}"
    );
    if order_ok && all_ok && ratio < 0.6 {
        println!("[selftest-parallel-tools] PASS");
        0
    } else {
        println!("[selftest-parallel-tools] FAIL（并行加速比应 < 0.6）");
        1
    }
}
