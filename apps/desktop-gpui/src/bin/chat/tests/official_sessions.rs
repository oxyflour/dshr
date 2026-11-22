#![cfg(test)]

use super::*;

#[test]
#[ignore = "requires the rc.5 official goal package"]
fn stage5_official_goals_persist_revisions_and_bound_automatic_turns() {
    let fixture = tempfile::tempdir().unwrap();
    stage5_runtime(fixture.path(), &["goal"]);
    let cancel = Arc::new(AtomicBool::new(false));
    let path = fixture.path().join("s-goal.jsonl");
    let open =
        || ChatState::open_diagnostics(path.clone(), "s-goal", &cancel, ApprovalBroker::new());
    let mut state = open();
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        model_tools(&[(
            "make",
            "create_goal",
            json!({"objective":"Verify persistence","max_goal_rounds":1}),
        )]),
        ModelReply::text_only("starting"),
        model_tools(&[
            ("automatic", "get_goal", json!({})),
            (
                "forged",
                "create_goal",
                json!({"objective":"Change authority","max_goal_rounds":1}),
            ),
        ]),
        ModelReply::text_only("round complete"),
    ]))));
    let result = state
        .run_turn("Keep working on this goal", &[], &mut |_| {})
        .unwrap();
    assert_eq!(result.reply, "round complete", "{:?}", state.load_errors());
    let created = logged_tool(&state.log, "make");
    let id = created["value"]["goal"]["id"]
        .as_str()
        .expect("goal ID")
        .to_owned();
    assert_eq!(created["value"]["activation"], "armed");
    assert_eq!(
        logged_tool(&state.log, "automatic")["value"]["goal"]["roundsStarted"],
        1
    );
    assert!(logged_tool(&state.log, "forged")["content"]
        .to_string()
        .contains("direct user turn"));
    assert_eq!(
        state
            .log
            .events()
            .iter()
            .filter(|event| event.event_type == "turn.started")
            .count(),
        2
    );
    drop(state);

    let mut reopened = open();
    reopened.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        model_tools(&[
            ("observe", "get_goal", json!({})),
            (
                "stale",
                "update_goal",
                json!({"goal_id":id,"revision":2,"action":"complete"}),
            ),
        ]),
        model_tools(&[(
            "finish",
            "update_goal",
            json!({"goal_id":id,"revision":1,"action":"complete"}),
        )]),
        ModelReply::text_only("verified"),
    ]))));
    assert_eq!(
        reopened
            .run_turn("Confirm and finish", &[], &mut |_| {})
            .unwrap()
            .reply,
        "verified"
    );
    let observed = logged_tool(&reopened.log, "observe");
    assert_eq!(observed["value"]["goal"]["roundsStarted"], 1);
    assert_eq!(observed["value"]["activation"], "disarmed");
    assert!(logged_tool(&reopened.log, "stale")["content"]
        .to_string()
        .contains("stale"));
    assert_eq!(
        logged_tool(&reopened.log, "finish")["value"]["goal"]["phase"],
        "complete"
    );
    assert_eq!(
        logged_tool(&reopened.log, "finish")["value"]["goal"]["revision"],
        2
    );
}

#[test]
#[ignore = "requires the rc.5 official session query package"]
fn stage5_official_query_enforces_the_session_tree_and_reads_prior_events() {
    let fixture = tempfile::tempdir().unwrap();
    stage5_runtime(fixture.path(), &["session-query"]);
    let cancel = Arc::new(AtomicBool::new(false));
    let index = SessionIndex::new(fixture.path());
    let mut state = ChatState::open_diagnostics(
        index.path_for("s-query"),
        "s-query",
        &cancel,
        ApprovalBroker::new(),
    );
    state
        .run_turn("root-visible-needle", &[], &mut |_| {})
        .unwrap();
    let workspace = state.log.created().unwrap().workspace_id.clone();
    let put = |id: &str, created: SessionCreated, text: &str| {
        let mut log = SessionLog::new(id);
        log.append_created(now_ts(), &created).unwrap();
        log.append(EventSpec::durable(
            "message.assistant.settled",
            now_ts(),
            json!({"message_id":"m-1","text":text}),
        ))
        .unwrap();
        save_atomic(&index.path_for(id), log.to_jsonl().as_bytes()).unwrap();
    };
    put(
        "s-visible",
        SessionCreated::child(
            &workspace,
            "s-query",
            dshr_session_store::SpawnSource::Agent,
        ),
        "child-visible-needle",
    );
    put(
        "s-foreign",
        SessionCreated::root(&workspace),
        "child-visible-needle unauthorized secret",
    );
    put(
        "s-outside",
        SessionCreated::child("outside", "s-query", dshr_session_store::SpawnSource::Agent),
        "child-visible-needle outside secret",
    );
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        model_tools(&[
            (
                "sessions",
                "session_search",
                json!({"query":"child-visible-needle"}),
            ),
            (
                "events",
                "session_event_search",
                json!({"query":"root-visible-needle"}),
            ),
            (
                "lineage",
                "session_trace",
                json!({"session_id":"s-visible"}),
            ),
            (
                "trace",
                "session_event_trace",
                json!({"session_id":"s-visible","seq":2}),
            ),
            (
                "read",
                "session_event_read",
                json!({"session_id":"s-visible","seq":2,"before":1}),
            ),
            (
                "denied",
                "session_event_read",
                json!({"session_id":"s-foreign","seq":2}),
            ),
            (
                "outside",
                "session_event_read",
                json!({"session_id":"s-outside","seq":2}),
            ),
            (
                "missing",
                "session_event_read",
                json!({"session_id":"s-missing","seq":2}),
            ),
        ]),
        ModelReply::text_only("query complete"),
    ]))));
    state.run_turn("Inspect history", &[], &mut |_| {}).unwrap();
    let result = |id: &str| logged_tool(&state.log, id);
    let sessions = result("sessions")["value"].as_str().unwrap().to_owned();
    assert!(
        sessions.contains("s-visible")
            && !sessions.contains("s-foreign")
            && !sessions.contains("s-outside"),
        "{sessions}"
    );
    assert!(result("events")["value"]
        .as_str()
        .unwrap()
        .contains("root-visible-needle"));
    assert!(result("lineage")["value"]
        .as_str()
        .unwrap()
        .contains("s-query"));
    assert!(result("trace")["value"]
        .as_str()
        .unwrap()
        .contains("Target: seq 2"));
    assert!(result("read")["value"]
        .as_str()
        .unwrap()
        .contains("child-visible-needle"));
    let denied = result("denied")["content"].clone();
    assert!(
        denied.to_string().contains("outside the caller workspace"),
        "{denied}"
    );
    assert_eq!(denied, result("outside")["content"]);
    assert_eq!(denied, result("missing")["content"]);
}

#[test]
#[ignore = "requires the rc.5 official subagent and control packages"]
fn stage5_official_subagents_run_wait_resume_and_fail() {
    let fixture = tempfile::tempdir().unwrap();
    let (shell, command) = if cfg!(windows) {
        ("pwsh", "(1..100 | Measure-Object -Sum).Sum")
    } else {
        ("bash", "printf 'child shell should not run'")
    };
    let workspace = stage5_runtime(
        fixture.path(),
        &["subagent", "subagent-control", "working-directory", shell],
    );
    let delegated = workspace.join("delegated");
    std::fs::create_dir(&delegated).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let index = SessionIndex::new(fixture.path());
    let mut state = ChatState::open_diagnostics(
        index.path_for("s-lead"),
        "s-lead",
        &cancel,
        ApprovalBroker::new(),
    );
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", server.local_addr().unwrap());
    state.settings.model = Arc::new(DesktopModel::OpenAi(
        OpenAiProvider::new("test-fixture").with_connection(endpoint, "fixture"),
    ));
    let wire = std::thread::spawn(move || {
        let mut stream = accept_child_model(&server);
        let request = read_child_model_request(&mut stream);
        let calls = vec![
            json!({"id":"child-cwd","type":"function","function":{"name":"working_directory","arguments":"{}"}}),
            json!({"id":"child-shell","type":"function","function":{"name":shell,"arguments":
                json!({"command":command,"description":"Check background approval"}).to_string()}}),
        ];
        write_child_model_reply(&mut stream, json!({"role":"assistant","tool_calls":calls}))
            .unwrap();
        let mut stream = accept_child_model(&server);
        read_child_model_request(&mut stream);
        write_child_model_text(&mut stream, "child-proof complete").unwrap();
        let mut stream = accept_child_model(&server);
        let follow_up = read_child_model_request(&mut stream);
        write_child_model_reply(&mut stream, json!({"role":"assistant","tool_calls":[
            {"id":"followup-shell","type":"function","function":{"name":shell,"arguments":
                json!({"command":command,"description":"Check persisted child permissions"}).to_string()}}
        ]})).unwrap();
        let mut stream = accept_child_model(&server);
        read_child_model_request(&mut stream);
        write_child_model_text(&mut stream, "follow-up-proof complete").unwrap();
        (request, follow_up)
    });
    let invoke = |id, name, args| model_tools(&[(id, name, args)]);
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        invoke(
            "spawn",
            "subagent",
            json!({"prompt":"child-proof","description":"Read sample","name":"reader","cwd":"delegated"}),
        ),
        invoke("wait", "wait_agent", json!({"timeout_ms":10_000})),
        invoke("list", "list_agents", json!({})),
        ModelReply::text_only("first turn verified"),
    ]))));
    state.run_turn("Start a child", &[], &mut |_| {}).unwrap();
    state.permission.set(PermissionMode::DangerFullAccess);
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        invoke(
            "message",
            "send_message",
            json!({"agent":"reader","message":"follow-up-proof"}),
        ),
        invoke("again", "wait_agent", json!({"timeout_ms":10_000})),
        invoke(
            "denied",
            "send_message",
            json!({"agent":"other-tree","message":"no"}),
        ),
        ModelReply::text_only("verified"),
    ]))));
    state
        .run_turn(
            "Resume the child after changing parent permissions",
            &[],
            &mut |_| {},
        )
        .unwrap();
    let result = |id: &str| logged_tool(&state.log, id);
    assert_eq!(
        result("spawn")["value"]["name"],
        "reader",
        "{:?}; {:?}",
        result("spawn"),
        state.load_errors()
    );
    assert_eq!(
        result("wait")["value"]["timedOut"],
        false,
        "{:?}",
        result("wait")
    );
    assert!(result("list")["value"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["name"] == "reader"));
    assert!(result("message")["value"]["messageId"]
        .as_str()
        .unwrap()
        .starts_with("message-"));
    assert_eq!(result("again")["value"]["timedOut"], false);
    assert!(result("denied")["content"]
        .to_string()
        .contains("outside the caller tree"));
    let (request, follow_up) = wire.join().unwrap();
    assert!(follow_up["messages"]
        .to_string()
        .contains("follow-up-proof"));
    let tools = request["tools"].as_array().unwrap();
    let description = |name| {
        tools
            .iter()
            .find(|tool| tool["function"]["name"] == name)
            .unwrap_or_else(|| panic!("missing {name} in {tools:?}"))["function"]["description"]
            .as_str()
            .unwrap()
    };
    assert!(description("working_directory").contains("empty object {}"));
    assert!(description(shell).contains("Use working_directory"));
    let child = index
        .children_of("s-lead")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let child_log = load_session_log(fixture.path(), &child.id).unwrap();
    assert!(child_log
        .events()
        .iter()
        .any(|event| event.event_type == "subagent/created"
            && event.payload["permission_mode"] == "workspace-write"));
    let resumed = logged_tool(&child_log, "followup-shell");
    assert_eq!(resumed["is_error"], true, "{resumed:?}");
    assert!(
        resumed["content"]
            .to_string()
            .contains("Interactive approval is unavailable in this background subagent"),
        "{resumed:?}"
    );
    let cwd = logged_tool(&child_log, "child-cwd");
    assert_eq!(cwd["is_error"], false, "{cwd:?}");
    let returned = Path::new(cwd["value"]["cwd"].as_str().unwrap());
    assert_eq!(
        returned.canonicalize().unwrap(),
        delegated.canonicalize().unwrap()
    );
    let shell = logged_tool(&child_log, "child-shell");
    assert_eq!(shell["is_error"], true, "{shell:?}");
    assert!(
        shell["content"]
            .to_string()
            .contains("Interactive approval is unavailable in this background subagent"),
        "{shell:?}"
    );
    let settlements: Vec<_> = child_log
        .events()
        .iter()
        .filter(|event| event.event_type == "subagent/settled")
        .collect();
    assert_eq!(settlements.len(), 2);
    assert!(settlements
        .iter()
        .all(|event| event.payload["outcome"] == "completed"));
    assert!(settlements[1].payload["reply"]
        .as_str()
        .unwrap()
        .contains("follow-up-proof"));
    assert_eq!(
        state
            .log
            .events()
            .iter()
            .filter(|event| event.event_type == "subagent/notified")
            .count(),
        2
    );

    state.settings.model = Arc::new(DesktopModel::Unconfigured);
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        invoke(
            "fail-spawn",
            "subagent",
            json!({"prompt":"fail-proof","description":"Unavailable model","name":"failure"}),
        ),
        invoke("fail-wait", "wait_agent", json!({"timeout_ms":10_000})),
        ModelReply::text_only("failure inspected"),
    ]))));
    state
        .run_turn("Check failed child", &[], &mut |_| {})
        .unwrap();
    assert_eq!(
        logged_tool(&state.log, "fail-spawn")["value"]["name"],
        "failure"
    );
    assert!(state
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "child.finished" && event.payload["outcome"] == "failed"));
}

#[test]
#[ignore = "requires the rc.5 official Teams tool package"]
fn stage5_official_teams_preserve_task_versions_and_dependencies_after_restart() {
    let fixture = tempfile::tempdir().unwrap();
    stage5_runtime(fixture.path(), &["agent-team"]);
    let cancel = Arc::new(AtomicBool::new(false));
    let open = |id: &str| {
        ChatState::open_diagnostics(
            fixture.path().join(format!("{id}.jsonl")),
            id,
            &cancel,
            ApprovalBroker::new(),
        )
    };
    let invoke = |id, name, args| model_tools(&[(id, name, args)]);
    let mut state = open("s-team");
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        invoke(
            "first",
            "team_task_create",
            json!({"subject":"Prepare","description":"Do first","write_scopes":["docs"]}),
        ),
        invoke(
            "second",
            "team_task_create",
            json!({"subject":"Review","description":"Do next","blocked_by":["task-1"]}),
        ),
        invoke(
            "blocked",
            "team_task_update",
            json!({"task_id":"task-2","expected_revision":1,"action":"claim"}),
        ),
        invoke(
            "cycle",
            "team_task_update",
            json!({"task_id":"task-1","expected_revision":1,"action":"set_dependencies","blocked_by":["task-2"]}),
        ),
        invoke(
            "claim",
            "team_task_update",
            json!({"task_id":"task-1","expected_revision":1,"action":"claim"}),
        ),
        invoke(
            "stale",
            "team_task_update",
            json!({"task_id":"task-1","expected_revision":1,"action":"complete"}),
        ),
        invoke(
            "done",
            "team_task_update",
            json!({"task_id":"task-1","expected_revision":2,"action":"complete"}),
        ),
        ModelReply::text_only("tasks settled"),
    ]))));
    state.run_turn("Track tasks", &[], &mut |_| {}).unwrap();
    let result = |id: &str| logged_tool(&state.log, id);
    assert_eq!(
        result("first")["value"]["id"],
        "task-1",
        "{:?}; {:?}",
        result("first"),
        state.load_errors()
    );
    assert_eq!(result("second")["value"]["ready"], false);
    assert_eq!(result("blocked")["is_error"], true);
    assert!(result("cycle")["content"].to_string().contains("cycle"));
    assert_eq!(result("claim")["value"]["owner"], "lead");
    assert!(result("stale")["content"].to_string().contains("stale"));
    assert_eq!(result("done")["value"]["status"], "completed");
    drop(state);

    let mut reopened = open("s-team");
    reopened.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        invoke("list", "team_task_list", json!({"ready":true})),
        ModelReply::text_only("verified"),
    ]))));
    reopened
        .run_turn("Find ready tasks", &[], &mut |_| {})
        .unwrap();
    let ready = logged_tool(&reopened.log, "list");
    assert_eq!(ready["value"]["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(ready["value"]["tasks"][0]["id"], "task-2");
    let mut unrelated = open("s-unrelated");
    unrelated.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        invoke("get", "team_task_get", json!({"task_id":"task-1"})),
        ModelReply::text_only("checked"),
    ]))));
    unrelated
        .run_turn("Inspect unrelated tasks", &[], &mut |_| {})
        .unwrap();
    assert_eq!(logged_tool(&unrelated.log, "get")["is_error"], true);
}

#[test]
#[ignore = "requires the rc.5 official workflow engine and local process sandbox"]
fn stage5_official_workflow_runs_guest_and_child() {
    let fixture = tempfile::tempdir().unwrap();
    stage5_runtime(fixture.path(), &["workflow", "ralph"]);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(
        fixture.path().join("s-flow.jsonl"),
        "s-flow",
        &cancel,
        ApprovalBroker::new(),
    );
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        model_tools(&[(
            "flow",
            "workflow",
            json!({"meta":{"name":"example","description":"Exercise the guest"},
            "script":"const value = await agent('workflow-child-proof'); return { value, sandbox: typeof process };"}),
        )]),
        ModelReply::text_only("workflow checked"),
    ]))));
    state
        .run_turn("Run the workflow", &[], &mut |_| {})
        .unwrap();
    let flow = logged_tool(&state.log, "flow");
    assert_eq!(
        flow["value"]["kind"],
        "foreground",
        "{flow:?}; {:?}",
        state.load_errors()
    );
    assert_eq!(flow["value"]["agentsStarted"], 1);
    assert!(flow["value"]["result"]["value"]
        .as_str()
        .unwrap()
        .contains("workflow-child-proof"));
    assert_eq!(flow["value"]["result"]["sandbox"], "undefined");
}

#[test]
#[ignore = "requires rc.5 Ralph and the foreground workflow sandbox"]
fn stage5_official_ralph_passes_a_bounded_report_to_a_fresh_child() {
    let fixture = tempfile::tempdir().unwrap();
    stage5_runtime(fixture.path(), &["ralph"]);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(
        fixture.path().join("s-ralph.jsonl"),
        "s-ralph",
        &cancel,
        ApprovalBroker::new(),
    );
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", server.local_addr().unwrap());
    state.settings.model = Arc::new(DesktopModel::OpenAi(
        OpenAiProvider::new("test-fixture").with_connection(endpoint, "fixture"),
    ));
    let wire = std::thread::spawn(move || {
        let mut first = accept_child_model(&server);
        let first_request = read_child_model_request(&mut first);
        write_child_model_text(&mut first, &json!({"status":"continue","summary":"first","evidence":[],"nextSteps":["handoff-proof"],"blocker":""}).to_string()).unwrap();
        let mut second = accept_child_model(&server);
        let second_request = read_child_model_request(&mut second);
        write_child_model_text(&mut second, &json!({"status":"complete","summary":"done","evidence":["evidence-proof"],"nextSteps":[],"blocker":""}).to_string()).unwrap();
        (first_request, second_request)
    });
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        model_tools(&[(
            "ralph-loop",
            "ralph",
            json!({"objective":"fresh-round-proof","max_rounds":2}),
        )]),
        ModelReply::text_only("rounds checked"),
    ]))));
    state
        .run_turn("Run Ralph with two fresh rounds", &[], &mut |_| {})
        .unwrap();
    let result = logged_tool(&state.log, "ralph-loop");
    assert_eq!(
        result["value"]["result"]["status"],
        "complete",
        "{result:?}; {:?}",
        state.load_errors()
    );
    assert_eq!(result["value"]["result"]["roundsStarted"], 2);
    let (first, second) = wire.join().unwrap();
    assert!(first.to_string().contains("fresh-round-proof"));
    assert!(second.to_string().contains("handoff-proof"));
    let children = SessionIndex::new(fixture.path())
        .children_of("s-ralph")
        .unwrap();
    assert_eq!(children.len(), 2);
    assert_ne!(children[0].id, children[1].id);
}

#[test]
#[ignore = "requires rc.5 workflow and a controllable HTTP child"]
fn stage5_official_workflow_cancels_its_active_child() {
    let fixture = tempfile::tempdir().unwrap();
    stage5_runtime(fixture.path(), &["workflow"]);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(
        fixture.path().join("s-stop-flow.jsonl"),
        "s-stop-flow",
        &cancel,
        ApprovalBroker::new(),
    );
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", server.local_addr().unwrap());
    state.settings.model = Arc::new(DesktopModel::OpenAi(
        OpenAiProvider::new("test-fixture").with_connection(endpoint, "fixture"),
    ));
    let cancel_parent = cancel.clone();
    let wire = std::thread::spawn(move || {
        let mut stream = accept_child_model(&server);
        let request = read_child_model_request(&mut stream);
        cancel_parent.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(300));
        let _ = write_child_model_text(&mut stream, "late response");
        request
    });
    state.agent = Agent::new(cancel_aware(
        OfficialScript(VecDeque::from([
            model_tools(&[(
                "cancel-workflow",
                "workflow",
                json!({"meta":{"name":"stop","description":"Cancel the guest"},
            "script":"return await agent('cancel-child-proof');"}),
            )]),
            ModelReply::text_only("cancelled workflow settled"),
        ])),
        &cancel,
    ));
    let result = state.run_turn("Run and stop", &[], &mut |_| {}).unwrap();
    assert!(result.reason == TurnReason::Cancelled);
    assert!(state
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "tool.aborted"));
    assert!(wire
        .join()
        .unwrap()
        .to_string()
        .contains("cancel-child-proof"));
    let child = SessionIndex::new(fixture.path())
        .children_of("s-stop-flow")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let log = load_session_log(fixture.path(), &child.id).unwrap();
        if let Some(finish) = log
            .events()
            .iter()
            .find(|event| event.event_type == "subagent/settled")
        {
            assert_eq!(finish.payload["outcome"], "interrupted");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child did not record cancellation"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "requires the rc.5 official schedule and recurrence packages"]
fn stage5_official_schedules_claim_each_occurrence_once_across_restart() {
    let fixture = tempfile::tempdir().unwrap();
    stage5_runtime(fixture.path(), &["schedule"]);
    let cancel = Arc::new(AtomicBool::new(false));
    let path = fixture.path().join("s-schedule.jsonl");
    let open =
        || ChatState::open_diagnostics(path.clone(), "s-schedule", &cancel, ApprovalBroker::new());
    let mut state = open();
    state.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        model_tools(&[(
            "create",
            "schedule_create",
            json!({"title":"Hourly proof","prompt":"schedule-proof-needle",
            "target":"this_session","rrule":"DTSTART:20200101T000000Z\nRRULE:FREQ=HOURLY"}),
        )]),
        ModelReply::text_only("schedule ready"),
    ]))));
    state
        .run_turn("Schedule this reminder", &[], &mut |_| {})
        .unwrap();
    let create = logged_tool(&state.log, "create");
    let id = create["value"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{create:?}; {:?}", state.load_errors()))
        .to_owned();
    assert_eq!(create["value"]["status"], "active");
    let original_stamp = create["value"]["updatedAt"].clone();
    drop(state);

    let ledger = fixture.path().join("schedules/s-schedule.json");
    let mut saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ledger).unwrap()).unwrap();
    let next = saved["tasks"][0]["scheduledAt"].as_u64().unwrap();
    // Use the previous real occurrence of this hourly rule and restart on that durable record.
    saved["tasks"][0]["scheduledAt"] = json!(next - 3_600_000);
    save_atomic(&ledger, &serde_json::to_vec(&saved).unwrap()).unwrap();

    struct ScheduleNoticeModel(bool);
    impl ModelProvider for ScheduleNoticeModel {
        fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
            let delivered = request.messages.iter().any(|message| matches!(message,
                ChatMessage::System(text) if text.contains("[SCHEDULE REMINDER BATCH]") && text.contains("schedule-proof-needle")));
            assert_eq!(delivered, self.0, "{:?}", request.messages);
            Ok(ModelReply::text_only("checked deliveries"))
        }
    }
    let mut reopened = open();
    reopened.agent = Agent::new(Box::new(ScheduleNoticeModel(true)));
    reopened
        .run_turn("Show due reminders", &[], &mut |_| {})
        .unwrap();
    drop(reopened);
    let mut recovered = open();
    recovered.agent = Agent::new(Box::new(ScheduleNoticeModel(false)));
    recovered
        .run_turn("Check reminders again", &[], &mut |_| {})
        .unwrap();

    recovered.agent = Agent::new(Box::new(OfficialScript(VecDeque::from([
        model_tools(&[
            ("view", "schedule_view", json!({"id":id})),
            (
                "stale",
                "schedule_edit",
                json!({"id":id,"action":"update","active":false,"expectedUpdatedAt":original_stamp}),
            ),
        ]),
        model_tools(&[(
            "pause",
            "schedule_edit",
            json!({"id":id,"action":"update","active":false}),
        )]),
        ModelReply::text_only("verified"),
    ]))));
    recovered
        .run_turn("Review and pause the reminder", &[], &mut |_| {})
        .unwrap();
    let view = logged_tool(&recovered.log, "view");
    assert_eq!(
        view["value"]["lastDelivery"]["targetSessionId"],
        "s-schedule"
    );
    assert_eq!(
        logged_tool(&recovered.log, "stale")["value"]["code"],
        "schedule_conflict"
    );
    assert_eq!(
        logged_tool(&recovered.log, "pause")["value"]["status"],
        "paused"
    );
}
