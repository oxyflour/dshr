#![cfg(test)]

use super::*;

#[test]
fn flash_uploads_pdf_once_and_sends_it_with_inline_image() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let responses = [
        ("application/json", r#"{"id":"file-api-test"}"#),
        (
            "text/event-stream",
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                "data: [DONE]\n\n",
            ),
        ),
        (
            "application/json",
            r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
        ),
    ];
    let server = std::thread::spawn(move || {
        let mut captured = Vec::new();
        for (mime, response) in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut target = String::new();
            reader.read_line(&mut target).unwrap();
            let mut headers = HashMap::new();
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
            }
            let mut body = vec![0; headers["content-length"].parse().unwrap()];
            reader.read_exact(&mut body).unwrap();
            write!(socket, "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: {mime}\r\n\r\n{response}", response.len()).unwrap();
            captured.push((target, headers, body));
        }
        captured
    });

    let mut provider = DeepSeekProvider::new("fixture").with_model("deepseek-flash");
    provider.base_url = url;
    provider.timeout = Duration::from_secs(5);
    let request = CompletionRequest {
        messages: vec![ChatMessage::UserWithAttachments {
            text: "read".into(),
            attachments: vec![
                MediaAttachment {
                    name: "test.pdf".into(),
                    format: AttachmentFormat::Pdf,
                    reference: "pdf".into(),
                    bytes: Some(b"%PDF-fixture".as_slice().into()),
                },
                MediaAttachment {
                    name: "test.png".into(),
                    format: AttachmentFormat::Png,
                    reference: "png".into(),
                    bytes: Some(b"\x89PNG\r\n\x1a\n".as_slice().into()),
                },
            ],
        }],
        tool_specs: Vec::new(),
    };
    let mut output = String::new();
    assert_eq!(
        provider
            .complete_streaming(&request, &mut |delta| output.push_str(delta))
            .unwrap()
            .text,
        "ok"
    );
    assert_eq!(output, "ok");
    assert_eq!(provider.complete(&request).unwrap().text, "ok");
    let captured = server.join().unwrap();
    let (target, headers, body) = &captured[0];
    assert_eq!(target.trim(), "POST /files HTTP/1.1");
    assert_eq!(headers["authorization"], "Bearer fixture");
    assert!(headers["content-type"].starts_with("multipart/form-data; boundary="));
    let form = std::str::from_utf8(body).unwrap();
    assert!(form.find("name=\"purpose\"").unwrap() < form.find("name=\"file\"").unwrap());
    assert!(form.contains("rich_layout_recognition"));
    assert!(form.contains("name=\"expires_after[seconds]\"\r\n\r\n3600"));
    assert!(form.contains("%PDF-fixture"));
    for (target, _, body) in &captured[1..] {
        assert_eq!(target.trim(), "POST /chat/completions HTTP/1.1");
        let body: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(
            body["messages"][1]["content"],
            json!([
                { "type": "text", "text": "read" },
                { "type": "text", "text": "附件：test.pdf" },
                { "type": "file", "file_id": "file-api-test" },
                { "type": "text", "text": "附件：test.png" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0KGgo=" } }
            ])
        );
    }
}

#[test]
fn parses_plain_text_reply() {
    let body = r#"{"choices":[{"message":{"role":"assistant","content":"你好"}}]}"#;
    let reply = parse_reply(body).unwrap();
    assert_eq!(reply.text, "你好");
    assert!(reply.tool_calls.is_empty());
}

#[test]
fn parses_tool_call_reply() {
    let body = r#"{"choices":[{"message":{"role":"assistant","content":null,
        "tool_calls":[{"id":"c1","type":"function",
        "function":{"name":"probe_native","arguments":"{\"text\":\"hi\"}"}}]}}]}"#;
    let reply = parse_reply(body).unwrap();
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].id, "c1");
    assert_eq!(reply.tool_calls[0].name, "probe_native");
    assert_eq!(reply.tool_calls[0].arguments["text"], "hi");
}

#[test]
fn parses_multiple_tool_calls_in_one_reply() {
    let body = r#"{"choices":[{"message":{"role":"assistant","content":null,
        "tool_calls":[
            {"id":"c1","type":"function","function":{"name":"a","arguments":"{\"x\":1}"}},
            {"id":"c2","type":"function","function":{"name":"b","arguments":"{}"}}
        ]}}]}"#;
    let reply = parse_reply(body).unwrap();
    assert_eq!(reply.tool_calls.len(), 2);
    assert_eq!(reply.tool_calls[0].name, "a");
    assert_eq!(reply.tool_calls[1].id, "c2");
}

#[test]
fn tool_call_without_id_gets_a_stable_synthetic_id() {
    let body = r#"{"choices":[{"message":{"role":"assistant","content":null,
        "tool_calls":[{"type":"function","function":{"name":"a","arguments":"{}"}}]}}]}"#;
    let reply = parse_reply(body).unwrap();
    assert_eq!(reply.tool_calls[0].id, "call-0");
}

#[test]
fn surfaces_api_errors() {
    let body = r#"{"error":{"message":"invalid api key"}}"#;
    let err = parse_reply(body).unwrap_err();
    assert!(err.contains("invalid api key"));
}

#[test]
fn rejects_malformed_body() {
    assert!(parse_reply("not json").is_err());
    assert!(parse_reply("{}").is_err());
}

#[test]
fn sse_text_deltas_accumulate_in_order() {
    let mut decoder = SseDecoder::default();
    let mut chunks = Vec::new();
    for line in [
        r#"data: {"choices":[{"delta":{"content":"你"}}]}"#,
        "",
        r#"data: {"choices":[{"delta":{"content":"好"}}]}"#,
        ": keep-alive",
        r#"data: {"choices":[{"delta":{"content":"呀"},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ] {
        if let Some(chunk) = decoder.feed(line).expect("valid sse") {
            chunks.push(chunk);
        }
    }
    assert_eq!(chunks, vec!["你", "好", "呀"]);
    assert!(decoder.is_done());
    let reply = decoder.finish();
    assert_eq!(reply.text, "你好呀");
    assert!(reply.tool_calls.is_empty());
}

#[test]
fn sse_tool_call_fragments_are_joined() {
    let mut decoder = SseDecoder::default();
    for line in [
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"probe_native","arguments":""}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"text\":"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"hi\"}"}}]}}]}"#,
        "data: [DONE]",
    ] {
        decoder.feed(line).expect("valid sse");
    }
    let reply = decoder.finish();
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].id, "c1");
    assert_eq!(reply.tool_calls[0].name, "probe_native");
    assert_eq!(reply.tool_calls[0].arguments["text"], "hi");
}

#[test]
fn sse_multiple_tool_calls_are_merged_by_index() {
    let mut decoder = SseDecoder::default();
    for line in [
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"a","arguments":"{\"x\":"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"c2","function":{"name":"b","arguments":"{}"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}"#,
        "data: [DONE]",
    ] {
        decoder.feed(line).expect("valid sse");
    }
    let reply = decoder.finish();
    assert_eq!(reply.tool_calls.len(), 2);
    assert_eq!(reply.tool_calls[0].id, "c1");
    assert_eq!(reply.tool_calls[0].arguments["x"], 1);
    assert_eq!(reply.tool_calls[1].id, "c2");
}

#[test]
fn sse_error_payload_is_reported() {
    let mut decoder = SseDecoder::default();
    let err = decoder
        .feed(r#"data: {"error":{"message":"rate limited"}}"#)
        .unwrap_err();
    assert!(err.contains("rate limited"));
}

#[test]
fn sse_ignores_non_data_lines_and_trims_cr() {
    let mut decoder = SseDecoder::default();
    assert_eq!(decoder.feed("event: message").unwrap(), None);
    assert_eq!(
        decoder
            .feed("data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\r")
            .unwrap(),
        Some("a".to_owned())
    );
}

#[test]
fn sse_malformed_data_line_is_reported() {
    let mut decoder = SseDecoder::default();
    assert!(decoder.feed("data: not-json").is_err());
}

fn request(messages: Vec<ChatMessage>) -> CompletionRequest {
    CompletionRequest {
        messages,
        tool_specs: Vec::new(),
    }
}

#[test]
fn request_body_maps_strict_tool_protocol() {
    let provider = DeepSeekProvider::new("test-key");
    let body = provider.request_body(
        &request(vec![
            ChatMessage::user("hi"),
            ChatMessage::Assistant {
                reasoning: String::new(),
                text: String::new(),
                tool_calls: vec![ModelToolCall {
                    id: "c1".to_owned(),
                    name: "probe_native".to_owned(),
                    arguments: json!({ "text": "hi" }),
                }],
            },
            ChatMessage::Tool {
                call_id: "c1".to_owned(),
                content: "\"HI\"".to_owned(),
            },
        ]),
        false,
    );
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages[0]["role"], "system", "调用方没给 system 时自动补");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(
        messages[2]["content"],
        Value::Null,
        "有 tool_calls 时 content 必须是 null"
    );
    assert_eq!(messages[2]["tool_calls"][0]["id"], "c1");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["arguments"], "{\"text\":\"hi\"}",
        "arguments 必须是字符串化 JSON"
    );
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "c1");
    assert_eq!(messages[3]["content"], "\"HI\"");
}

#[test]
fn request_body_keeps_caller_system_prompt() {
    let provider = DeepSeekProvider::new("test-key");
    let body = provider.request_body(
        &request(vec![
            ChatMessage::System("自定义提示".to_owned()),
            ChatMessage::user("hi"),
        ]),
        false,
    );
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 2, "不应重复追加 system");
    assert_eq!(messages[0]["content"], "自定义提示");
}

#[test]
fn request_body_omits_temperature_and_max_tokens_by_default() {
    let provider = DeepSeekProvider::new("test-key")
        .with_temperature(None)
        .with_max_tokens(None);
    let body = provider.request_body(&request(vec![ChatMessage::user("hi")]), false);
    assert!(
        body.get("temperature").is_none(),
        "默认不发送 temperature（对齐 DSH）: {body}"
    );
    assert!(
        body.get("max_tokens").is_none(),
        "默认不发送 max_tokens（provider 默认）: {body}"
    );
}

#[test]
fn request_body_sends_explicit_temperature_and_max_tokens() {
    let provider = DeepSeekProvider::new("test-key")
        .with_temperature(Some(0.7))
        .with_max_tokens(Some(8_192));
    let body = provider.request_body(&request(vec![ChatMessage::user("hi")]), false);
    assert_eq!(body["temperature"], json!(0.7));
    assert_eq!(body["max_tokens"], json!(8_192));
}

// M3-DEFAULT-030..032：DeepSeek 的 reasoning effort 配置与请求字段。
#[test]
fn m3_default_030_deepseek_parses_reasoning_effort_from_env() {
    const VAR: &str = ReasoningEffort::ENV;
    let previous = std::env::var(VAR).ok();
    std::env::set_var(VAR, "high");
    let provider = DeepSeekProvider::new("test-key");
    assert_eq!(provider.reasoning_effort(), Some(ReasoningEffort::High));
    match previous {
        Some(value) => std::env::set_var(VAR, value),
        None => std::env::remove_var(VAR),
    }
}

#[test]
fn m3_default_031_deepseek_serializes_reasoning_effort_and_disable_switch() {
    let request = request(vec![ChatMessage::user("hi")]);
    for (choice, mode, level) in [
        (None, Value::Null, Value::Null),
        (Some(ReasoningEffort::Off), json!("disabled"), Value::Null),
        (Some(ReasoningEffort::Low), json!("enabled"), json!("low")),
        (Some(ReasoningEffort::High), json!("enabled"), json!("high")),
        (Some(ReasoningEffort::Max), json!("enabled"), json!("max")),
    ] {
        let body = DeepSeekProvider::new("test-key")
            .with_reasoning_effort(choice)
            .request_body(&request, false);
        assert_eq!(body["thinking"]["type"], mode);
        assert_eq!(body["reasoning_effort"], level);
    }
}

#[test]
fn m3_default_032_deepseek_off_effort_has_no_wire_value() {
    assert_eq!(ReasoningEffort::Off.wire_value(), None);
    assert_eq!(ReasoningEffort::Max.wire_value(), Some("max"));
    // 非法 env 被忽略（不 panic、不阻断构造）。
    assert_eq!(ReasoningEffort::resolve(Some("bogus")), None);
}

#[test]
fn parse_env_number_treats_blank_and_garbage_as_unset() {
    // 用本测试专属变量名，避免并行测试读到彼此的 env 写入。
    const VAR: &str = "DSHR_TEST_ONLY_DEEPSEEK_TEMPERATURE";
    std::env::remove_var(VAR);
    assert_eq!(parse_env_number::<f64>(VAR), None, "未设置 = 不发送");
    std::env::set_var(VAR, "0.25");
    assert_eq!(parse_env_number::<f64>(VAR), Some(0.25));
    std::env::set_var(VAR, "  ");
    assert_eq!(parse_env_number::<f64>(VAR), None, "空白 = 不发送");
    std::env::set_var(VAR, "abc");
    assert_eq!(parse_env_number::<f64>(VAR), None, "非法 = 不发送");
    std::env::remove_var(VAR);
}

#[test]
fn cancel_token_round_trips() {
    let token = Arc::new(AtomicBool::new(false));
    let provider = DeepSeekProvider::new("k").with_cancel(token.clone());
    assert!(
        Arc::ptr_eq(&provider.cancel_token().expect("token"), &token),
        "UI 与 provider 必须共享同一个 token"
    );
    assert!(DeepSeekProvider::new("k").cancel_token().is_none());
    assert!(!token.load(Ordering::Relaxed));
}

/// token 预置位：streaming 不发起请求，直接返回"已生成的部分"（空），且不清零 token。
#[test]
fn preset_cancel_returns_partial_without_a_request() {
    let token = Arc::new(AtomicBool::new(true));
    let mut provider = DeepSeekProvider::new("k").with_cancel(token.clone());
    let request = request(vec![ChatMessage::user("hi")]);
    let mut deltas: Vec<String> = Vec::new();
    let reply = provider
        .complete_streaming(&request, &mut |delta| deltas.push(delta.to_owned()))
        .expect("取消是部分成功（Ok），不是 Err");
    assert!(reply.text.is_empty());
    assert!(reply.tool_calls.is_empty());
    assert!(deltas.is_empty());
    assert!(
        token.load(Ordering::Relaxed),
        "provider 不得清零 token（清零只由 UI 在每轮开始时做）"
    );
}

/// 流到一半取消：只保留已生成的部分文本，剩余 chunk 不再读取/回调。
#[test]
fn streaming_stops_after_cancel_and_keeps_partial_text() {
    let token = Arc::new(AtomicBool::new(false));
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"呀\"}}]}\n",
        "data: [DONE]\n",
    );
    let mut deltas: Vec<String> = Vec::new();
    let reply = read_sse_stream(
        BufReader::new(std::io::Cursor::new(sse)),
        Some(&token),
        &mut |delta| {
            if let ModelDelta::Text(chunk) = delta {
                deltas.push(chunk.to_owned());
                // 第一段文本到达后用户按停止：后续 chunk 不能再回调。
                token.store(true, Ordering::Relaxed);
            }
        },
    )
    .expect("部分成功");
    assert_eq!(deltas, vec!["你"]);
    assert_eq!(reply.text, "你");
    assert!(reply.tool_calls.is_empty());
}

/// 没有取消 token（或 token 未置位）时，整个流都要读完。
#[test]
fn streaming_without_cancel_reads_every_chunk() {
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"分析\"}}]}\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n",
        "data: [DONE]\n",
    );
    let mut deltas: Vec<String> = Vec::new();
    let mut reasoning = String::new();
    let reply = read_sse_stream(
        BufReader::new(std::io::Cursor::new(sse)),
        None,
        &mut |delta| match delta {
            ModelDelta::Text(chunk) => deltas.push(chunk.to_owned()),
            ModelDelta::Reasoning(chunk) => reasoning.push_str(chunk),
        },
    )
    .expect("正常读完");
    assert_eq!(deltas, vec!["你", "好"]);
    assert_eq!(reply.text, "你好");
    assert_eq!(reply.reasoning, "分析");
    assert_eq!(reasoning, reply.reasoning);
}

#[test]
fn request_body_adds_stream_flag_only_when_streaming() {
    let provider = DeepSeekProvider::new("test-key");
    let request = request(vec![ChatMessage::user("hi")]);
    let plain = provider.request_body(&request, false);
    assert!(plain.get("stream").is_none());
    let streamed = provider.request_body(&request, true);
    assert_eq!(streamed["stream"], json!(true));
    assert!(streamed.get("tools").is_none());
    // 密钥不能出现在请求体里。
    assert!(!streamed.to_string().contains("test-key"));
}
