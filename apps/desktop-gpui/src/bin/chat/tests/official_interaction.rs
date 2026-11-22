#![cfg(test)]

use super::*;
use std::io::Read;

#[test]
#[ignore = "requires the rc.5 Office tool, OfficeKit, and a matching native engine"]
fn stage3_official_office_converts_recalculates_and_delivers() {
    use std::io::Write;

    fn create_odf(path: &Path, family: &str, extra: &str, content: &str) {
        let mime = format!("application/vnd.oasis.opendocument.{family}");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("mimetype", options).unwrap();
        zip.write_all(mime.as_bytes()).unwrap();
        zip.start_file("content.xml", options).unwrap();
        write!(zip,
            r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" office:version="1.2" {extra}><office:body>{content}</office:body></office:document-content>"#,
        ).unwrap();
        zip.start_file("META-INF/manifest.xml", options).unwrap();
        write!(zip,
            r#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2"><manifest:file-entry manifest:full-path="/" manifest:media-type="{mime}" manifest:version="1.2"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#,
        ).unwrap();
        zip.finish().unwrap();
    }

    #[derive(Default)]
    struct OfficeModel(usize);

    impl ModelProvider for OfficeModel {
        fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
            self.0 += 1;
            let call = |id: &str, name: &str, arguments| ModelToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                arguments,
            };
            let calls = match self.0 {
                1 => {
                    let offered: Vec<_> =
                        request.tool_specs.iter().map(|t| t.name.as_str()).collect();
                    assert!(offered.contains(&"office_convert"), "{offered:?}");
                    assert!(offered.contains(&"office_recalculate"));
                    assert!(offered.contains(&"office_render"));
                    vec![
                        call(
                            "convert",
                            "office_convert",
                            json!({ "input": "source.odt", "format": "pdf" }),
                        ),
                        call(
                            "recalculate",
                            "office_recalculate",
                            json!({ "input": "source.ods", "format": "ods" }),
                        ),
                    ]
                }
                2 => {
                    let output_path = |id| {
                        let content = request
                            .messages
                            .iter()
                            .find_map(|message| match message {
                                ChatMessage::Tool { call_id, content } if call_id == id => {
                                    Some(content.as_str())
                                }
                                _ => None,
                            })
                            .expect("office result");
                        assert!(!content.starts_with("ERROR:"), "{content}");
                        content
                            .lines()
                            .next()
                            .unwrap()
                            .split_once("): ")
                            .unwrap_or_else(|| panic!("missing output in {content}"))
                            .1
                    };
                    vec![
                        call(
                            "delivery",
                            "present",
                            json!({ "files": [
                            { "path": output_path("convert") },
                            { "path": output_path("recalculate") },
                        ] }),
                        ),
                        call(
                            "render",
                            "office_render",
                            json!({
                                "input": "source.odt", "pages": [1, 2], "dpi": 96,
                            }),
                        ),
                    ]
                }
                3 => {
                    let images = request.messages.iter().find_map(|message| match message {
                        ChatMessage::ToolImages(images) => Some(images),
                        _ => None,
                    });
                    assert_pngs(images.expect("native Office images in the model request"));
                    return Ok(ModelReply::text_only("done"));
                }
                _ => return Ok(ModelReply::text_only("done")),
            };
            Ok(ModelReply {
                reasoning: String::new(),
                text: String::new(),
                tool_calls: calls,
            })
        }
    }

    fn assert_pngs(images: &[dshr_agent_core::MediaAttachment]) {
        assert_eq!(images.len(), 2, "{images:?}");
        assert!(images.iter().all(|image| {
            image
                .bytes
                .as_ref()
                .is_some_and(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
        }));
    }

    let fixture = tempfile::tempdir().unwrap();
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    create_odf(
        &workspace.join("source.odt"),
        "text",
        r#"xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0""#,
        &format!(
            "<office:text>{}</office:text>",
            "<text:p>Office gate report</text:p>".repeat(160)
        ),
    );
    create_odf(
        &workspace.join("source.ods"),
        "spreadsheet",
        r#"xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:of="urn:oasis:names:tc:opendocument:xmlns:of:1.2""#,
        r#"<office:spreadsheet><table:table table:name="Sheet1"><table:table-row><table:table-cell table:formula="of:=1+2" office:value-type="float" office:value="0"/></table:table-row></table:table></office:spreadsheet>"#,
    );
    let sidecar = PathBuf::from(std::env::var_os("DSHR_OFFICIAL_RC5_DIR").unwrap());
    std::env::set_var("DSHR_APP_ROOT", sidecar.parent().unwrap().parent().unwrap());
    std::env::set_var("DSHR_HOME", fixture.path().join("home"));
    std::env::set_var("DSHR_WORKSPACE", &workspace);
    std::env::set_var("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_PROVIDER", "mock");
    std::env::set_var("DSHR_PERMISSION_MODE", "workspace-write");
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    std::env::set_var(
        "DSHR_DSH_PLUGINS",
        json!([
            { "name": "@deepseek-ai/dsh-skill-office" },
            { "name": "@deepseek-ai/dsh-tool-skill" },
            { "name": "@deepseek-ai/dsh-office-kit" },
            { "name": "@deepseek-ai/dsh-tool-office" },
            { "name": "@deepseek-ai/dsh-tool-present" },
        ])
        .to_string(),
    );
    let session = "s-office";
    let log = fixture.path().join("office.jsonl");
    let mut state = ChatState::open_diagnostics(
        log.clone(),
        session,
        &Arc::new(AtomicBool::new(false)),
        ApprovalBroker::new(),
    );
    state.agent = Agent::new(Box::new(OfficeModel::default()));
    let reply = state
        .run_turn("Use /office please", &[], &mut |_| {})
        .unwrap();
    assert_eq!(reply.reply, "done");
    let spill = state.spill.clone();
    drop(state);
    let replayed =
        SessionLog::from_jsonl(session, &std::fs::read_to_string(&log).unwrap()).unwrap();
    let mut history = history_from_log(&replayed);
    dshr_agent_context::spill::restore_history_attachments(&spill, &replayed, &mut history, |_| {
        true
    });
    assert_pngs(
        history
            .iter()
            .find_map(|message| match message {
                ChatMessage::ToolImages(images) => Some(images.as_slice()),
                _ => None,
            })
            .expect("replayed Office images"),
    );
    let deliveries = dsh_deliverables::replay(&replayed);
    assert_eq!(deliveries.len(), 1, "{:?}", replayed.events());
    let files = deliveries.values().next().unwrap();
    assert_eq!(files.len(), 2);
    assert!(std::fs::read(workspace.join(&files[0].path))
        .unwrap()
        .starts_with(b"%PDF-"));
    let mut ods =
        zip::ZipArchive::new(std::fs::File::open(workspace.join(&files[1].path)).unwrap()).unwrap();
    let mut sheet = String::new();
    ods.by_name("content.xml")
        .unwrap()
        .read_to_string(&mut sheet)
        .unwrap();
    assert!(sheet.contains("office:value=\"3\""), "{sheet}");
    std::env::remove_var("DSHR_APPROVAL_MODE");
}

#[test]
#[ignore = "requires the rc.5 ask-user package"]
fn stage3_official_questions_round_trip_in_the_desktop_runtime() {
    let fixture = tempfile::tempdir().unwrap();
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sidecar = PathBuf::from(std::env::var_os("DSHR_OFFICIAL_RC5_DIR").unwrap());
    std::env::set_var("DSHR_APP_ROOT", sidecar.parent().unwrap().parent().unwrap());
    std::env::set_var("DSHR_HOME", fixture.path().join("home"));
    std::env::set_var("DSHR_WORKSPACE", &workspace);
    std::env::set_var("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_PROVIDER", "mock");
    std::env::set_var(
        "DSHR_DSH_PLUGINS",
        json!([
            { "name": "@deepseek-ai/dsh-tool-ask-user" },
        ])
        .to_string(),
    );
    let abort = Arc::new(AtomicBool::new(false));
    let session = "s-question";
    let mut state = ChatState::open_diagnostics(
        fixture.path().join("questions.jsonl"),
        session,
        &abort,
        ApprovalBroker::new(),
    );
    let broker = state.l2_access.as_ref().unwrap().questions.clone();
    let call = |id: &str| ToolCallRequest {
        call_id: id.to_owned(),
        tool: "ask_user_question".to_owned(),
        arguments: json!({ "questions": [
            { "id": "choice", "question": "Choose items", "multi_select": true,
              "options": [{ "label": "One", "description": "First item" }, { "label": "Two" }] },
            { "id": "skip", "question": "Any notes?" },
        ] }),
    };

    let wait_for_prompt = |broker: &dsh_user_questions::QuestionBroker| {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(prompt) = broker.first(session, None) {
                return Some(prompt);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let run = |state: &mut ChatState, id, answer| {
        let input = broker.clone();
        let reader = std::thread::spawn(move || {
            let prompt = wait_for_prompt(&input)?;
            assert_eq!(prompt.questions[0].question, "Choose items");
            assert!(input.first("another-session", None).is_none());
            assert!(input
                .resolve("another-session", prompt.ticket, None)
                .is_err());
            assert!(input
                .resolve(
                    session,
                    prompt.ticket,
                    Some(vec![
                        (vec!["Unknown".to_owned()], String::new()),
                        (Vec::new(), String::new()),
                    ])
                )
                .is_err());
            input.resolve(session, prompt.ticket, answer).unwrap();
            Some(prompt.ticket)
        });
        let outcome = state.execute_tool(&call(id));
        let ticket = reader
            .join()
            .unwrap()
            .unwrap_or_else(|| panic!("no desktop question: {outcome:?}"));
        assert!(broker.resolve(session, ticket, None).is_err());
        outcome
    };

    let answered = run(
        &mut state,
        "answer",
        Some(vec![
            (
                vec!["One".to_owned(), "Two".to_owned()],
                "其他答复".to_owned(),
            ),
            (Vec::new(), String::new()),
        ]),
    );
    assert!(!answered.is_error, "{answered:?}");
    assert_eq!(
        answered.value["answers"],
        json!([
            { "id": "choice", "selected": ["One", "Two"], "custom": "其他答复" },
            { "id": "skip", "selected": [] },
        ])
    );

    let dismissed = run(&mut state, "dismiss", None);
    assert!(dismissed.is_error, "{dismissed:?}");

    let input = broker.clone();
    let cancel = abort.clone();
    let watcher = std::thread::spawn(move || {
        let prompt = wait_for_prompt(&input);
        cancel.store(true, Ordering::Release);
        prompt.map(|prompt| prompt.ticket)
    });
    let cancelled = state.execute_tool(&call("cancel"));
    let ticket = watcher.join().unwrap().expect("no question to cancel");
    assert!(cancelled.is_error, "{cancelled:?}");
    assert!(broker.first(session, None).is_none());
    assert!(broker.resolve(session, ticket, None).is_err());
}

#[test]
#[ignore = "requires rc.5 shell tool packages and the platform shell in PATH"]
fn stage2_official_shell_uses_desktop_approvals_and_process_cancellation() {
    let fixture = tempfile::tempdir().unwrap();
    let workspace = fixture.path().join("workspace");
    let child = workspace.join("child");
    std::fs::create_dir_all(&child).unwrap();
    let resource =
        PathBuf::from(std::env::var_os("DSHR_OFFICIAL_RC5_DIR").expect("isolated rc.5 sidecar"));
    let root = resource.parent().and_then(std::path::Path::parent).unwrap();
    std::env::set_var("DSHR_APP_ROOT", root);
    std::env::set_var("DSHR_HOME", fixture.path().join("home"));
    std::env::set_var("DSHR_WORKSPACE", &workspace);
    std::env::set_var("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_PROVIDER", "mock");
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let (name, command, denied, slow, cancellable) = if cfg!(windows) {
        (
            "pwsh",
            "Write-Output $env:DSH_SESSION_ID; Write-Output $PSHOME; [Console]::Error.WriteLine('stderr marker'); exit 7",
            "Set-Content -LiteralPath blocked.txt -Value denied",
            "Start-Sleep -Seconds 10",
            "Set-Content -LiteralPath started.txt -Value started; Start-Sleep -Seconds 10; Set-Content -LiteralPath finished.txt -Value finished",
        )
    } else {
        (
            "bash",
            "printf '%s\\n' \"$DSH_SESSION_ID\"; printf '%s\\n' 'stderr marker' >&2; exit 7",
            "printf denied > blocked.txt",
            "sleep 10",
            "printf started > started.txt; sleep 10; printf finished > finished.txt",
        )
    };
    std::env::set_var("DSHR_DSH_PLUGINS", json!([
        { "name": format!("@deepseek-ai/dsh-tool-{name}"), "config": { "enableRunInBackground": false } },
        { "name": "@deepseek-ai/dsh-tool-working-directory" },
    ]).to_string());
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(
        fixture.path().join("shell-session.jsonl"),
        "s-shell",
        &cancel,
        ApprovalBroker::new(),
    );
    state.permission.set(PermissionMode::WorkspaceWrite);
    let cwd = state.execute_tool(&ToolCallRequest {
        call_id: "cwd".to_owned(),
        tool: "working_directory".to_owned(),
        arguments: json!({ "cd": "child" }),
    });
    assert!(!cwd.is_error, "{cwd:?}");

    let call = |id: &str, command: &str, timeout: u64| ToolCallRequest {
        call_id: id.to_owned(),
        tool: name.to_owned(),
        arguments: json!({
            "description": "Run an isolated process check",
            "command": command,
            "timeoutMs": timeout,
        }),
    };
    let result = state.execute_tool(&call("run", command, 6000));
    assert!(!result.is_error, "{result:?}");
    assert_eq!(result.value["exitCode"], 7, "{result:?}");
    let mut stdout = result.value["stdout"]["text"].as_str().unwrap().lines();
    assert_eq!(stdout.next(), Some("s-shell"));
    let bundled = root.join("runtime/powershell");
    if cfg!(windows) && bundled.join("pwsh.exe").is_file() {
        let actual = PathBuf::from(stdout.next().expect("PowerShell home"));
        assert_eq!(
            actual.canonicalize().unwrap(),
            bundled.canonicalize().unwrap()
        );
    }
    assert_eq!(
        result.value["stderr"]["text"].as_str().unwrap().trim(),
        "stderr marker"
    );
    assert!(result.value["cwd"]
        .as_str()
        .is_some_and(|cwd| std::path::Path::new(cwd).ends_with("child")));

    std::env::set_var("DSHR_APPROVAL_MODE", "deny");
    let result = state.execute_tool(&call("denied", denied, 6000));
    assert!(result.is_error, "{result:?}");
    assert!(!child.join("blocked.txt").exists());
    std::env::remove_var("DSHR_APPROVAL_MODE");
    for (id, decision) in [
        ("approved-once", ApprovalOutcome::AllowOnce),
        ("asked-again", ApprovalOutcome::Deny),
    ] {
        let approvals = state.approvals.clone();
        let answer = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(6);
            while std::time::Instant::now() < deadline {
                if let Some(pending) = approvals
                    .pending()
                    .into_iter()
                    .find(|pending| pending.call_id == id)
                {
                    let one_shot = !pending.supports_always_allow();
                    approvals.resolve(id, decision);
                    return one_shot;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            approvals.cancel_pending();
            false
        });
        let result = state.execute_tool(&call(id, command, 6000));
        assert!(answer.join().unwrap(), "expected one-shot approval {id}");
        assert_eq!(
            result.is_error,
            decision == ApprovalOutcome::Deny,
            "{result:?}"
        );
    }
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let result = state.execute_tool(&call("timeout", slow, 100));
    assert_eq!(result.value["timedOut"], true, "{result:?}");
    let marker = child.join("started.txt");
    let stop = cancel.clone();
    let watcher = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Release);
        marker.exists()
    });
    let result = state.execute_tool(&call("cancel", cancellable, 10000));
    assert!(
        watcher.join().unwrap(),
        "the official shell did not reach the running state"
    );
    assert!(result.is_error, "{result:?}");
    assert!(!child.join("finished.txt").exists());
    std::env::remove_var("DSHR_APPROVAL_MODE");
}
