#![cfg(test)]

use super::*;
use crate::protocol::CancelPayload;
use std::io::Cursor;

/// 线程安全的内存 sink：serve 结束后仍可取回输出。
#[derive(Clone, Default)]
struct SharedSink(Arc<Mutex<Vec<u8>>>);

impl SharedSink {
    fn lines(&self) -> Vec<String> {
        let bytes = self.0.lock().unwrap().clone();
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect()
    }
}

impl Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn fixture_decls() -> ReadyDecls {
    ReadyDecls::new(
        vec![
            crate::ToolDecl::new("echo", "echo input"),
            crate::ToolDecl::new("fail", "always fails"),
            crate::ToolDecl::new("slow", "sleeps then checks cancel"),
        ],
        vec![crate::PanelDecl::new(
            "workspace.panel",
            "fixture.one",
            "Fixture One",
            "fixture.one",
        )],
    )
}

fn hello_line() -> String {
    Envelope::with_payload(
        kind::PROCESS_HELLO,
        HelloPayload {
            protocol: PROTOCOL_VERSION.to_owned(),
            host_version: "0.0.0".to_owned(),
            plugin_id: "cpp.sdk.test".to_owned(),
            generation: 1,
            manifest: None,
        },
    )
    .unwrap()
    .encode_line()
    .unwrap()
}

fn invoke_line(request_id: &str, tool: &str, arguments: Value) -> String {
    Envelope::with_payload(
        kind::TOOL_INVOKE,
        InvokePayload {
            tool: tool.to_owned(),
            arguments,
            timeout_ms: 5_000,
            cancelable: true,
            generation: Some(1),
        },
    )
    .unwrap()
    .with_request_id(request_id)
    .encode_line()
    .unwrap()
}

fn run_session(input: String) -> Vec<String> {
    let sink = SharedSink::default();
    let writer = sink.clone();
    let handle = thread::spawn(move || {
        ChildSession::serve_with_io(
            Cursor::new(input),
            writer,
            fixture_decls(),
            |call: &ToolCall, cancel: &CancelFlag| match call.tool.as_str() {
                "echo" => {
                    let text = call
                        .argument("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Ok(ToolResult::text(text))
                }
                "fail" => Err(ToolError::failed("boom")),
                "slow" => {
                    thread::sleep(std::time::Duration::from_millis(300));
                    if cancel.is_cancelled() {
                        Err(ToolError::Cancelled)
                    } else {
                        Ok(ToolResult::text("done"))
                    }
                }
                other => Err(ToolError::internal(format!("unknown tool {other}"))),
            },
        )
    });
    handle.join().unwrap().unwrap();
    sink.lines()
}

fn parse_lines(lines: &[String]) -> Vec<Envelope> {
    lines
        .iter()
        .map(|line| Envelope::parse(line).unwrap())
        .collect()
}

// CPP-PROC-020..032：ChildSession 的会话语义。
#[test]
fn cpp_proc_020_hello_ready_roundtrip_exposes_declarations() {
    let lines = run_session(hello_line());
    let envelopes = parse_lines(&lines);
    assert_eq!(envelopes.len(), 1);
    let ready: ReadyPayload = serde_json::from_value(envelopes[0].payload.clone()).unwrap();
    assert_eq!(ready.protocol, PROTOCOL_VERSION);
    assert_eq!(ready.plugin_id, "cpp.sdk.test");
    assert_eq!(ready.tools.len(), 3);
    assert_eq!(ready.panels.len(), 1);
    assert_eq!(ready.panels[0].slot, "workspace.panel");
}

#[test]
fn cpp_proc_021_invoke_returns_tool_result() {
    let mut input = hello_line();
    input.push_str(&invoke_line(
        "req-1",
        "echo",
        serde_json::json!({ "text": "hi" }),
    ));
    let envelopes = parse_lines(&run_session(input));
    let result = envelopes
        .iter()
        .find(|envelope| envelope.kind == kind::TOOL_RESULT)
        .expect("tool.result");
    assert_eq!(result.request_id.as_deref(), Some("req-1"));
    assert!(!result.is_error());
    let tool_result: ToolResult = serde_json::from_value(result.payload.clone()).unwrap();
    assert_eq!(tool_result.text_content(), "hi");
}

#[test]
fn cpp_proc_022_tool_error_maps_to_envelope_error() {
    let mut input = hello_line();
    input.push_str(&invoke_line("req-2", "fail", serde_json::json!({})));
    let envelopes = parse_lines(&run_session(input));
    let result = envelopes
        .iter()
        .find(|envelope| envelope.kind == kind::TOOL_RESULT)
        .expect("tool.result");
    assert_eq!(result.error_code(), Some(error_code::E_INTERNAL));
    assert_eq!(result.request_id.as_deref(), Some("req-2"));
}

#[test]
fn cpp_proc_023_unknown_tool_is_schema_error() {
    let mut input = hello_line();
    input.push_str(&invoke_line("req-3", "nope", serde_json::json!({})));
    let envelopes = parse_lines(&run_session(input));
    let result = envelopes
        .iter()
        .find(|envelope| envelope.kind == kind::TOOL_RESULT)
        .expect("tool.result");
    assert_eq!(result.error_code(), Some(error_code::E_SCHEMA));
}

#[test]
fn cpp_proc_024_ping_pong_and_unknown_kind() {
    let mut input = hello_line();
    input.push_str(&Envelope::new(kind::PROCESS_PING).encode_line().unwrap());
    input.push_str(
        &Envelope::new("tool.unknown")
            .with_request_id("req-x")
            .encode_line()
            .unwrap(),
    );
    let envelopes = parse_lines(&run_session(input));
    assert_eq!(envelopes[0].kind, kind::PROCESS_READY);
    assert_eq!(envelopes[1].kind, kind::PROCESS_PONG);
    let unknown_error = envelopes
        .iter()
        .find(|envelope| envelope.is_error())
        .expect("error envelope");
    assert_eq!(unknown_error.error_code(), Some(error_code::E_PROTOCOL));
}

#[test]
fn cpp_proc_025_malformed_frame_returns_protocol_error_and_session_survives() {
    let mut input = String::from("{not json}\n");
    input.push_str(&hello_line());
    let envelopes = parse_lines(&run_session(input));
    assert_eq!(envelopes[0].error_code(), Some(error_code::E_PROTOCOL));
    assert_eq!(envelopes[1].kind, kind::PROCESS_READY);
}

#[test]
fn cpp_proc_026_shutdown_replies_bye_and_stops() {
    let mut input = hello_line();
    input.push_str(&Envelope::new(kind::PROCESS_SHUTDOWN).encode_line().unwrap());
    input.push_str(&Envelope::new(kind::PROCESS_PING).encode_line().unwrap());
    let envelopes = parse_lines(&run_session(input));
    assert_eq!(envelopes[0].kind, kind::PROCESS_READY);
    assert_eq!(envelopes[1].kind, kind::PROCESS_BYE);
    assert_eq!(envelopes.len(), 2, "bye 之后不得再处理消息");
}

#[test]
fn cpp_proc_027_tool_cancel_sets_flag_and_handler_observes_it() {
    let mut input = hello_line();
    input.push_str(&invoke_line("req-9", "slow", serde_json::json!({})));
    input.push_str(
        &Envelope::with_payload(kind::TOOL_CANCEL, CancelPayload::default())
            .unwrap()
            .with_request_id("req-9")
            .encode_line()
            .unwrap(),
    );
    input.push_str(&Envelope::new(kind::PROCESS_SHUTDOWN).encode_line().unwrap());
    let envelopes = parse_lines(&run_session(input));
    let result = envelopes
        .iter()
        .find(|envelope| envelope.kind == kind::TOOL_RESULT)
        .expect("tool.result");
    assert_eq!(result.error_code(), Some(error_code::E_CANCELLED));
}

#[test]
fn cpp_proc_028_wrong_hello_protocol_is_version_error() {
    let mut hello = Envelope::with_payload(
        kind::PROCESS_HELLO,
        HelloPayload {
            protocol: "cpp-process/v2".to_owned(),
            host_version: "0.0.0".to_owned(),
            plugin_id: "cpp.sdk.test".to_owned(),
            generation: 1,
            manifest: None,
        },
    )
    .unwrap();
    hello.request_id = Some("hello-1".to_owned());
    let envelopes = parse_lines(&run_session(hello.encode_line().unwrap()));
    assert_eq!(envelopes[0].error_code(), Some(error_code::E_VERSION));
    assert_eq!(envelopes[0].request_id.as_deref(), Some("hello-1"));
}

#[test]
fn cpp_proc_029_invoke_requires_request_id() {
    let mut input = hello_line();
    input.push_str(
        &Envelope::with_payload(
            kind::TOOL_INVOKE,
            InvokePayload {
                tool: "echo".to_owned(),
                arguments: serde_json::json!({}),
                timeout_ms: 1_000,
                cancelable: true,
                generation: Some(1),
            },
        )
        .unwrap()
        .encode_line()
        .unwrap(),
    );
    let envelopes = parse_lines(&run_session(input));
    let error = envelopes
        .iter()
        .find(|envelope| envelope.is_error())
        .unwrap();
    assert_eq!(error.error_code(), Some(error_code::E_PROTOCOL));
}

#[test]
fn cpp_proc_030_oversized_result_becomes_e_too_large() {
    let big = "x".repeat(MAX_FRAME_BYTES + 1024);
    let sink = SharedSink::default();
    let writer = sink.clone();
    let input = format!(
        "{}{}",
        hello_line(),
        invoke_line("req-big", "echo", serde_json::json!({ "text": "small" }))
    );
    let handle = thread::spawn(move || {
        ChildSession::serve_with_io(
            Cursor::new(input),
            writer,
            fixture_decls(),
            move |_call: &ToolCall, _cancel: &CancelFlag| Ok(ToolResult::text(big.clone())),
        )
    });
    handle.join().unwrap().unwrap();
    let envelopes = parse_lines(&sink.lines());
    let result = envelopes
        .iter()
        .find(|envelope| envelope.kind == kind::TOOL_RESULT)
        .expect("tool.result");
    assert_eq!(result.error_code(), Some(error_code::E_TOO_LARGE));
}

#[test]
fn cpp_proc_031_ui_render_with_views_and_without() {
    // 有 view handler：ui.result 带元素树。
    let sink = SharedSink::default();
    let writer = sink.clone();
    let mut input = hello_line();
    input.push_str(
        &Envelope::with_payload(
            kind::UI_RENDER,
            crate::RenderPayload {
                view: "fixture.one".to_owned(),
                state_seq: 7,
            },
        )
        .unwrap()
        .with_request_id("ui-1")
        .encode_line()
        .unwrap(),
    );
    input.push_str(&Envelope::new(kind::PROCESS_SHUTDOWN).encode_line().unwrap());
    let handle = thread::spawn(move || {
        ChildSession::serve_with_io_and_views(
            Cursor::new(input),
            writer,
            fixture_decls(),
            |view: &str, state_seq: u64| {
                Ok(serde_json::json!({
                    "type": "Text",
                    "props": { "text": format!("{view}@{state_seq}") },
                    "a11yLabel": "fixture view"
                }))
            },
            |_call: &ToolCall, _cancel: &CancelFlag| Ok(ToolResult::text("unused")),
        )
    });
    handle.join().unwrap().unwrap();
    let envelopes = parse_lines(&sink.lines());
    let rendered = envelopes
        .iter()
        .find(|envelope| envelope.kind == kind::UI_RESULT)
        .expect("ui.result");
    assert_eq!(rendered.request_id.as_deref(), Some("ui-1"));
    assert!(!rendered.is_error());
    assert_eq!(
        rendered.payload["tree"]["props"]["text"],
        serde_json::json!("fixture.one@7")
    );
    assert_eq!(
        rendered.payload["tree"]["a11yLabel"],
        serde_json::json!("fixture view")
    );

    // 无 view handler：ui.result 报 E_INTERNAL，不 panic。
    let mut input = hello_line();
    input.push_str(
        &Envelope::with_payload(
            kind::UI_RENDER,
            crate::RenderPayload {
                view: "unknown".to_owned(),
                state_seq: 0,
            },
        )
        .unwrap()
        .encode_line()
        .unwrap(),
    );
    let envelopes = parse_lines(&run_session(input));
    let rendered = envelopes
        .iter()
        .find(|envelope| envelope.kind == kind::UI_RESULT)
        .expect("ui.result");
    assert_eq!(rendered.error_code(), Some(error_code::E_INTERNAL));
}

#[test]
fn cpp_proc_032_shutdown_payload_defaults() {
    let shutdown: crate::ShutdownPayload =
        serde_json::from_value(serde_json::json!({ "timeout_ms": 2500 })).unwrap();
    assert_eq!(shutdown.timeout_ms, 2500);
}

// CPP-PROC-033（M4 W3）：ui.action 触发 ActionHandler，且不写 result envelope。
#[test]
fn cpp_proc_033_ui_action_invokes_handler_fire_and_forget() {
    let seen: Arc<Mutex<Vec<(String, String, serde_json::Value)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let seen_in = Arc::clone(&seen);

    let mut input = hello_line();
    input.push_str(
        &Envelope::with_payload(
            kind::UI_ACTION,
            crate::ActionPayload {
                view: "fixture.one".to_owned(),
                action: "call".to_owned(),
                payload: serde_json::json!({
                    "method": "refresh",
                    "args": { "scope": "fixture" }
                }),
            },
        )
        .unwrap()
        .encode_line()
        .unwrap(),
    );
    input.push_str(&Envelope::new(kind::PROCESS_SHUTDOWN).encode_line().unwrap());

    let sink = SharedSink::default();
    let writer = sink.clone();
    let handle = thread::spawn(move || {
        ChildSession::serve_with_io_views_actions(
            Cursor::new(input),
            writer,
            fixture_decls(),
            |_view: &str, _state_seq: u64| Ok(serde_json::json!({"type": "Text"})),
            move |view: &str, action: &str, payload: &Value| {
                seen_in
                    .lock()
                    .unwrap()
                    .push((view.to_owned(), action.to_owned(), payload.clone()));
                Ok(())
            },
            |_call: &ToolCall, _cancel: &CancelFlag| Ok(ToolResult::text("ok")),
        )
    });
    handle.join().unwrap().unwrap();

    let recorded = seen.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, "fixture.one");
    assert_eq!(recorded[0].1, "call");
    assert_eq!(recorded[0].2["method"], "refresh");
    let envelopes = parse_lines(&sink.lines());
    assert!(
        !envelopes
            .iter()
            .any(|envelope| envelope.kind == kind::UI_RESULT),
        "ui.action 不是请求/响应，不应写 ui.result"
    );
}
