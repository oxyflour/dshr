#![cfg(test)]

use super::*;

#[test]
#[ignore = "requires rc.5 native executor and timeout policy packages"]
fn stage6_official_native_and_timeout_apply_to_dispatched_tools() {
    let fixture = tempfile::tempdir().unwrap();
    stage5_runtime(fixture.path(), &[]);
    let probe = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/compat-dsh-host/tests/fixtures/execution-probe.mjs");
    let probe = format!(
        "file:///{}",
        probe
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/")
    );
    std::env::set_var(
        "DSHR_DSH_PLUGINS",
        json!([
            {"name":"@deepseek-ai/dsh-tool-executor-native"},
            {"name":"@deepseek-ai/dsh-tool-call-timeout-policy"},
            {"name":probe},
        ])
        .to_string(),
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(
        fixture.path().join("s-native.jsonl"),
        "s-native",
        &cancel,
        ApprovalBroker::new(),
    );
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        model_tools(&[
            (
                "echo",
                "execution_probe_echo",
                json!({"text":"native-proof"}),
            ),
            ("deadline", "execution_probe_delay", json!({})),
        ]),
        ModelReply::text_only("verified"),
    ]))));
    state
        .run_turn("Execute the tool probes", &[], &mut |_| {})
        .unwrap();
    let echo = logged_tool(&state.log, "echo");
    assert_eq!(
        echo["value"],
        "native-proof",
        "{echo:?}; {:?}",
        state.load_errors()
    );
    let timeout = logged_tool(&state.log, "deadline");
    assert_eq!(timeout["is_error"], true);
    assert!(
        timeout["content"].to_string().contains("after 80ms"),
        "{timeout:?}"
    );
    assert!(state.load_errors().is_empty(), "{:?}", state.load_errors());
}

#[test]
#[ignore = "requires rc.5 Cordis inspection"]
fn stage6_official_inspection_lists_only_the_active_host_registration_snapshot() {
    let fixture = tempfile::tempdir().unwrap();
    stage5_runtime(fixture.path(), &["cordis", "fs"]);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(
        fixture.path().join("s-inspect.jsonl"),
        "s-inspect",
        &cancel,
        ApprovalBroker::new(),
    );
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        model_tools(&[
            ("list", "cordis_inspect_list", json!({})),
            (
                "tools",
                "cordis_inspect_query",
                json!({"platform":"host","provider":"Tool","method":"listTools","input":{}}),
            ),
            (
                "denied",
                "cordis_inspect_query",
                json!({"platform":"host","provider":"Tool","method":"listTools","input":{"session_id":"other"}}),
            ),
        ]),
        ModelReply::text_only("verified"),
    ]))));
    state
        .run_turn("Inspect session tools", &[], &mut |_| {})
        .unwrap();
    let listed = logged_tool(&state.log, "list");
    assert!(
        listed["value"]["providers"]
            .to_string()
            .contains("listTools"),
        "{listed:?}; {:?}",
        state.load_errors()
    );
    let scope = logged_tool(&state.log, "tools");
    let tools = scope["value"]["data"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{scope:?}"));
    assert!(tools.iter().any(|tool| tool["name"] == "read"));
    assert!(tools.iter().all(|tool| tool["name"] != "probe_native"));
    assert_eq!(logged_tool(&state.log, "denied")["is_error"], true);
}

#[test]
#[ignore = "requires rc.5 PTC executor and the local sandbox"]
fn stage6_official_ptc_reuses_rust_registration_and_write_approval() {
    let fixture = tempfile::tempdir().unwrap();
    let workspace = stage5_runtime(fixture.path(), &["executor-ptc", "fs"]);
    std::fs::write(workspace.join("source.txt"), "scoped-source-proof").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(
        fixture.path().join("s-ptc.jsonl"),
        "s-ptc",
        &cancel,
        ApprovalBroker::new(),
    );
    std::env::set_var("DSHR_APPROVAL_MODE", "deny");
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        model_tools(&[
            ("direct", "read", json!({"file_path":"source.txt"})),
            (
                "program-read",
                "run_code",
                json!({"description":"Read test file","code":"return await tools.read({file_path:'source.txt'});"}),
            ),
            (
                "denied",
                "run_code",
                json!({"description":"Try denied write","code":"return await tools.write({file_path:'denied.txt',content:'must not write'});"}),
            ),
        ]),
        ModelReply::text_only("verified read"),
    ]))));
    state
        .run_turn("Try the scoped program", &[], &mut |_| {})
        .unwrap();
    let direct = logged_tool(&state.log, "direct");
    assert!(
        direct["content"].to_string().contains("requires run_code"),
        "{direct:?}; {:?}",
        state.load_errors()
    );
    let read = logged_tool(&state.log, "program-read");
    assert!(
        read["content"].to_string().contains("scoped-source-proof"),
        "{read:?}"
    );
    assert!(!workspace.join("denied.txt").exists());
    assert!(logged_tool(&state.log, "denied")["content"]
        .to_string()
        .contains("denied"));

    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        model_tools(&[(
            "allowed",
            "run_code",
            json!({"description":"Write a test file","code":"return await tools.write({file_path:'approved.txt',content:'approved proof'});"}),
        )]),
        ModelReply::text_only("verified write"),
    ]))));
    state
        .run_turn("Approve program write", &[], &mut |_| {})
        .unwrap();
    let allowed = logged_tool(&state.log, "allowed");
    assert_eq!(allowed["is_error"], false, "{allowed:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("approved.txt")).unwrap(),
        "approved proof"
    );
    let admissions: Vec<_> = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "tool.program.admitted")
        .collect();
    assert_eq!(admissions.len(), 2);
    assert!(admissions
        .iter()
        .all(|event| event.payload["registration_id"]
            .as_str()
            .unwrap()
            .starts_with("reg-")));
}

#[test]
#[ignore = "requires rc.5 executor packages in a scoped preset"]
fn stage6_official_preset_revokes_the_program_transport_on_release() {
    let fixture = tempfile::tempdir().unwrap();
    stage5_runtime(fixture.path(), &["executor-native"]);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(
        fixture.path().join("s-revision.jsonl"),
        "s-revision",
        &cancel,
        ApprovalBroker::new(),
    );
    let host = state.host.as_mut().expect("official host");
    let catalog = json!({"program": [
        {"name":"@deepseek-ai/dsh-tool-executor-ptc"},
        {"name":"@deepseek-ai/dsh-tool-fs"},
    ]});
    let roster = host.sync_presets(&catalog).unwrap();
    assert!(roster["program"]["revision"].is_string(), "{roster:?}");
    host.bind_preset("s-revision", "program", None).unwrap();
    let tools = host.preset_tool_definitions("s-revision");
    let program = tools
        .iter()
        .find(|tool| tool.name == "run_code")
        .expect("scoped program transport");
    assert!(host.preset_tool_definitions("s-other").is_empty());
    assert!(!host
        .tool_definitions()
        .iter()
        .any(|tool| tool.name == "run_code"));
    let ids: serde_json::Map<_, _> = tools
        .iter()
        .filter_map(|tool| match &tool.origin {
            ToolOrigin::Sidecar {
                registration_id, ..
            } => Some((tool.name.clone(), json!(registration_id))),
            _ => None,
        })
        .collect();
    let published = host
        .prepare_agent_step(
            "s-revision",
            json!({
                "step":1, "registrations":ids, "model":{"provider":"mock","model":"fixture"},
            }),
        )
        .unwrap();
    assert!(
        published["deferred_tools"]
            .as_array()
            .is_some_and(|tools| tools.contains(&json!("read"))),
        "{published:?}"
    );

    host.sync_presets(&json!({})).unwrap();
    assert!(host
        .preset_tool_definitions("s-revision")
        .iter()
        .any(|tool| tool.name == "run_code"));
    host.release_preset("s-revision").unwrap();
    assert!(host.preset_tool_definitions("s-revision").is_empty());
    assert!(host
        .registrations()
        .iter()
        .all(|registration| registration.name != "run_code"));
    let stale = dshr_compat_dsh_host::BatchInvokeCall::new(
        "stale-program",
        "run_code",
        json!({"description":"Expired transport","code":"return 42;"}),
        Duration::from_secs(2),
    )
    .with_session("s-revision")
    .with_binding(program.origin.clone());
    let result = host.invoke_batch(&[stale], 1, None);
    assert!(
        result[0]
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("E_TOOL_UNAVAILABLE"),
        "{result:?}"
    );
}

#[test]
#[ignore = "requires rc.5 terminal packages and the platform PTY backend"]
fn stage4_official_terminals_keep_state_and_replay_closed_sessions() {
    let fixture = tempfile::tempdir().unwrap();
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sidecar = PathBuf::from(std::env::var_os("DSHR_OFFICIAL_RC5_DIR").unwrap());
    std::env::set_var("DSHR_APP_ROOT", sidecar.parent().unwrap().parent().unwrap());
    std::env::set_var("DSHR_HOME", fixture.path().join("home"));
    std::env::set_var("DSHR_WORKSPACE", &workspace);
    std::env::set_var("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_PROVIDER", "mock");
    std::env::set_var("DSHR_PERMISSION_MODE", "workspace-write");
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let (engine, assign, echo, persistent_assign, persistent_echo) = if cfg!(windows) {
        (
            "pwsh",
            "$terminalMarker='state-ok'",
            "Write-Output $terminalMarker",
            "$persistentMarker='persist-ok'",
            "Write-Output $persistentMarker",
        )
    } else {
        (
            "bash",
            "terminalMarker=state-ok",
            "printf '%s\\n' \"$terminalMarker\"",
            "persistentMarker=persist-ok",
            "printf '%s\\n' \"$persistentMarker\"",
        )
    };
    std::env::set_var("DSHR_DSH_PLUGINS", json!([
        { "name": "@deepseek-ai/dsh-tool-terminal", "config": { "enableRunInBackground": false } },
        { "name": format!("@deepseek-ai/dsh-tool-{engine}-persistent"), "config": { "timeoutMs": 8_000 } },
    ]).to_string());
    let abort = Arc::new(AtomicBool::new(false));
    let session = "s-terminal";
    let log = fixture.path().join("terminals.jsonl");
    let open = |path: PathBuf, session: &str| {
        ChatState::open_diagnostics(path, session, &abort, ApprovalBroker::new())
    };
    let run = |state: &mut ChatState, id: &str, tool: &str, arguments| {
        state.execute_tool(&ToolCallRequest {
            call_id: id.to_owned(),
            tool: tool.to_owned(),
            arguments,
        })
    };
    let mut state = open(log.clone(), session);
    let outside = run(
        &mut state,
        "outside",
        "terminal_open",
        json!({
            "type": "shell", "cwd": fixture.path(),
        }),
    );
    assert!(outside.is_error, "{outside:?}");
    let opened = run(
        &mut state,
        "open",
        "terminal_open",
        json!({ "type": "shell", "name": "work" }),
    );
    assert!(!opened.is_error, "{opened:?}; {:?}", state.load_errors());
    let terminal = opened.value["sessionId"]
        .as_str()
        .expect("host terminal identity");
    assert!(terminal.starts_with("term-"));
    let send = |text| json!({ "sessionId": terminal, "text": text });
    let first = run(&mut state, "send-a", "terminal_send", send(assign));
    assert!(!first.is_error, "{first:?}");
    let sent = run(&mut state, "send-b", "terminal_send", send(echo));
    assert!(
        !sent.is_error && sent.value["viewport"].to_string().contains("state-ok"),
        "{sent:?}"
    );
    let scroll = run(
        &mut state,
        "read",
        "terminal_read",
        json!({ "sessionId": terminal }),
    );
    assert!(
        !scroll.is_error && scroll.value.to_string().contains("state-ok"),
        "{scroll:?}"
    );

    let start = run(
        &mut state,
        "persistent-a",
        engine,
        json!({ "command": persistent_assign }),
    );
    assert!(!start.is_error, "{start:?}");
    let persistence = run(
        &mut state,
        "persistent-b",
        engine,
        json!({ "command": persistent_echo }),
    );
    assert!(
        persistence
            .value
            .as_str()
            .is_some_and(|text| text.contains("persist-ok")),
        "{persistence:?}"
    );
    state.save_checked().unwrap();
    state.switch_session(fixture.path().join("other.jsonl"), "s-other");
    let other = run(
        &mut state,
        "outsider",
        "terminal_read",
        json!({ "sessionId": terminal }),
    );
    assert!(other.is_error, "{other:?}");
    assert_eq!(
        run(&mut state, "other-list", "terminal_list", json!({})).value,
        json!([])
    );
    state.switch_session(log.clone(), session);
    let closing = run(
        &mut state,
        "close",
        "terminal_close",
        json!({ "sessionId": terminal }),
    );
    assert!(
        !closing.is_error && closing.value["outcome"] == "closed",
        "{closing:?}"
    );
    assert!(run(&mut state, "send-after-close", "terminal_send", send(echo)).is_error);
    let marker = workspace.join("started.txt");
    let cancel = abort.clone();
    let watcher = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        cancel.store(true, Ordering::Release);
        marker.exists()
    });
    let slow = if cfg!(windows) {
        "Set-Content -LiteralPath started.txt -Value started; Start-Sleep -Milliseconds 1200; Set-Content -LiteralPath late.txt -Value late"
    } else {
        "printf started > started.txt; sleep 1.2; printf late > late.txt"
    };
    let cancelled = run(
        &mut state,
        "cancel-send",
        engine,
        json!({ "command": slow }),
    );
    assert!(
        watcher.join().unwrap(),
        "persistent terminal did not start the command"
    );
    assert!(cancelled.is_error, "{cancelled:?}");
    std::thread::sleep(Duration::from_millis(1300));
    assert!(!workspace.join("late.txt").exists());
    abort.store(false, Ordering::Release);
    state.save_checked().unwrap();
    drop(state);

    let mut reopened = open(log, session);
    let list = run(&mut reopened, "replay", "terminal_list", json!({}));
    let sessions = list.value.as_array().unwrap_or_else(|| panic!("{list:?}"));
    assert_eq!(sessions.len(), 2, "{list:?}");
    assert!(
        sessions
            .iter()
            .all(|session| session["status"]["kind"] == "exited"),
        "{list:?}"
    );
    std::env::remove_var("DSHR_APPROVAL_MODE");
}

#[test]
#[ignore = "requires rc.5 jobs and platform shell packages"]
fn stage4_official_jobs_claim_stop_and_survive_restart() {
    let fixture = tempfile::tempdir().unwrap();
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sidecar = PathBuf::from(std::env::var_os("DSHR_OFFICIAL_RC5_DIR").unwrap());
    std::env::set_var("DSHR_APP_ROOT", sidecar.parent().unwrap().parent().unwrap());
    std::env::set_var("DSHR_HOME", fixture.path().join("home"));
    std::env::set_var("DSHR_WORKSPACE", &workspace);
    std::env::set_var("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_PROVIDER", "mock");
    std::env::set_var("DSHR_PERMISSION_MODE", "workspace-write");
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let (engine, command, slow) = if cfg!(windows) {
        (
            "pwsh",
            "Write-Output 'background-proof'",
            "Start-Sleep -Seconds 12; Set-Content -LiteralPath finished.txt -Value bad",
        )
    } else {
        (
            "bash",
            "printf 'background-proof\\n'",
            "sleep 12; printf bad > finished.txt",
        )
    };
    std::env::set_var("DSHR_DSH_PLUGINS", json!([
        { "name": "@deepseek-ai/dsh-tool-jobs", "config": { "completionDelivery": "quiet" } },
        { "name": format!("@deepseek-ai/dsh-tool-{engine}"), "config": { "enableRunInBackground": true, "promoteOnTimeout": false } },
    ]).to_string());
    let session = "s-job";
    let log = fixture.path().join("jobs.jsonl");
    let abort = Arc::new(AtomicBool::new(false));
    let open = |path: PathBuf, session: &str| {
        ChatState::open_diagnostics(path, session, &abort, ApprovalBroker::new())
    };
    let run = |state: &mut ChatState, id: &str, tool: &str, arguments| {
        state.execute_tool(&ToolCallRequest {
            call_id: id.to_owned(),
            tool: tool.to_owned(),
            arguments,
        })
    };
    let shell = |command: &str| json!({ "command": command, "description": "Background process check", "run_in_background": true });
    let mut state = open(log.clone(), session);
    let first = run(&mut state, "start-a", engine, shell(command));
    assert!(!first.is_error, "{first:?}");
    let first_id = first.value["jobId"]
        .as_str()
        .expect("official background id");
    assert_eq!(first.value["kind"], "background");

    let claimed = run(
        &mut state,
        "claim-a",
        "job_output",
        json!({
            "job_id": first_id, "wait": true, "timeout_ms": 4_000,
        }),
    );
    assert!(!claimed.is_error, "{claimed:?}");
    assert_eq!(claimed.value["job"]["status"], "completed");
    assert!(claimed.value["text"]
        .as_str()
        .unwrap()
        .contains("background-proof"));
    let repeat = run(
        &mut state,
        "claim-again",
        "job_output",
        json!({ "job_id": first_id }),
    );
    assert_eq!(repeat.value["text"], "", "{repeat:?}");

    let denied_background = {
        std::env::set_var("DSHR_APPROVAL_MODE", "deny");
        let result = run(&mut state, "deny", engine, shell(slow));
        std::env::set_var("DSHR_APPROVAL_MODE", "allow");
        result
    };
    assert!(denied_background.is_error);
    let second = run(&mut state, "start-b", engine, shell(slow));
    assert!(!second.is_error, "{second:?}");
    let second_id = second.value["jobId"].as_str().unwrap();
    let killed = run(
        &mut state,
        "stop-b",
        "job_kill",
        json!({ "job_id": second_id, "reason": "test cleanup" }),
    );
    assert_eq!(
        killed.value["outcome"], "cancellation-requested",
        "{killed:?}"
    );
    let settled = run(
        &mut state,
        "claim-b",
        "job_output",
        json!({
            "job_id": second_id, "wait": true, "timeout_ms": 4_000,
        }),
    );
    assert_eq!(settled.value["job"]["status"], "killed", "{settled:?}");
    assert!(!workspace.join("finished.txt").exists());
    let third = run(&mut state, "start-c", engine, shell(command));
    let third_id = third.value["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("{third:?}"));
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    loop {
        let list = run(&mut state, "list-before-restart", "job_list", json!({}));
        if list.value.as_array().is_some_and(|jobs| {
            jobs.iter()
                .any(|job| job["id"] == third_id && job["status"] == "completed")
        }) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{list:?}");
        std::thread::sleep(Duration::from_millis(30));
    }
    drop(state);

    let mut reopened = open(log, session);
    let list = run(&mut reopened, "list", "job_list", json!({}));
    let listed = list.value.as_array().unwrap_or_else(|| panic!("{list:?}"));
    assert_eq!(listed.len(), 3, "{list:?}");
    assert!(listed
        .iter()
        .any(|job| job["id"] == first_id && job["status"] == "completed"));
    assert!(listed
        .iter()
        .any(|job| job["id"] == second_id && job["status"] == "killed"));
    assert_eq!(
        run(
            &mut reopened,
            "claimed",
            "job_output",
            json!({ "job_id": first_id })
        )
        .value["text"],
        ""
    );
    struct NoticeModel {
        id: String,
        notice: bool,
    }
    impl ModelProvider for NoticeModel {
        fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
            let prefix = format!("background job {}", self.id);
            let found = request.messages.iter().any(
                |message| matches!(message, ChatMessage::System(text) if text.contains(&prefix)),
            );
            assert_eq!(found, self.notice, "{:?}", request.messages);
            self.notice = false;
            Ok(ModelReply::text_only("done"))
        }
    }
    reopened.agent = Agent::new(Box::new(NoticeModel {
        id: third_id.to_owned(),
        notice: true,
    }));
    reopened
        .run_turn("Inspect the background tasks", &[], &mut |_| {})
        .unwrap();
    reopened.run_turn("Continue", &[], &mut |_| {}).unwrap();
    let retained = run(
        &mut reopened,
        "claim-after-restart",
        "job_output",
        json!({ "job_id": third_id }),
    );
    assert!(
        retained.value["text"]
            .as_str()
            .is_some_and(|text| text.contains("background-proof")),
        "{retained:?}"
    );
    reopened.switch_session(fixture.path().join("other.jsonl"), "s-other");
    let forbidden = run(
        &mut reopened,
        "outsider",
        "job_output",
        json!({ "job_id": first_id }),
    );
    assert!(forbidden.is_error, "{forbidden:?}");
    assert_eq!(
        run(&mut reopened, "other-list", "job_list", json!({})).value,
        json!([])
    );
    std::env::remove_var("DSHR_APPROVAL_MODE");
}
