use super::*;

/// M1 的替代模型：按关键词决定"回文本"还是"调工具"。
///
/// 它只依赖 `CompletionRequest`（不看历史、不保存状态），所以同一请求必然得到同一输出，
/// 这让 Agent Loop 的测试是确定性的。M2 会被真实 provider 替换。
#[derive(Debug, Default)]
pub struct MockProvider;

impl ModelProvider for MockProvider {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        // 已经有工具结果：生成第二步回答（模拟"看结果再说话"）。
        if let Some(content) = request.last_tool_content() {
            return Ok(ModelReply::text_only(summarize(content)));
        }

        let user_text = request.last_user_text().to_owned();
        let lower = user_text.to_lowercase();
        if (user_text.contains("原生") || lower.contains("native"))
            && request
                .tool_specs
                .iter()
                .any(|spec| spec.name == "probe_native")
        {
            return Ok(tool_reply(request, "probe_native", &user_text));
        }

        // 会话树：自检用"子会话/spawn_child"触发派发工具（参数是 prompt 而不是 text）。
        if (user_text.contains("子会话") || lower.contains("spawn_child"))
            && request
                .tool_specs
                .iter()
                .any(|spec| spec.name == "spawn_child")
        {
            return Ok(ModelReply {
                reasoning: String::new(),
                text: String::new(),
                tool_calls: vec![ModelToolCall {
                    id: format!("mock-call-spawn-{}", request.messages.len()),
                    name: String::from("spawn_child"),
                    arguments: json!({ "prompt": user_text }),
                }],
            });
        }

        if user_text.contains("工具") || lower.contains("tool") {
            if let Some(spec) = request
                .tool_specs
                .iter()
                .find(|spec| spec.name != "probe_native")
            {
                return Ok(tool_reply(request, &spec.name, &user_text));
            }
        }

        Ok(ModelReply::text_only(format!(
            "收到：{user_text}。当前是 mock agent，尚未接入 LLM。"
        )))
    }

    /// 把回复切成 8 字符的 delta 依次回调：让流式路径在 mock 下也可确定性地被测。
    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        let reply = self.complete(request)?;
        let chars: Vec<char> = reply.text.chars().collect();
        for chunk in chars.chunks(8) {
            let piece: String = chunk.iter().collect();
            on_delta(&piece);
        }
        Ok(reply)
    }
}

/// 构造一个工具调用。id 用消息条数做序号：同一轮内唯一、跨轮不冲突。
fn tool_reply(request: &CompletionRequest, name: &str, user_text: &str) -> ModelReply {
    ModelReply {
        reasoning: String::new(),
        text: String::new(),
        tool_calls: vec![ModelToolCall {
            id: format!("mock-call-{}-{name}", request.messages.len()),
            name: name.to_owned(),
            arguments: json!({ "text": user_text }),
        }],
    }
}

fn summarize(content: &str) -> String {
    if let Some(reason) = content.strip_prefix("ERROR: ") {
        return format!("工具执行失败：{reason}。当前是 mock agent，尚未接入 LLM。");
    }
    format!("工具返回 {content}。当前是 mock agent，尚未接入 LLM。")
}
