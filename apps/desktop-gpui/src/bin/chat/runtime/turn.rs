use super::attachments::user_message_payload;
use super::{
    ChatExecutor, ChatState, DraftAttachment, ToolStep, TurnReason, TurnResult, TurnUpdate,
};
use crate::{creator, dsh_goals, now_ts, SUMMARY_PROMPT};
use dshr_agent_context::{estimate_tokens, user_message_from_payload};
use dshr_agent_core::{
    Agent, AgentEvent, ChatMessage, CompletionRequest, ModelProvider, ToolCallRequest,
    ToolExecutor, ToolOutcome,
};
use dshr_conversation_model::{build_from_log, Item};
use dshr_session_store::{EventSpec, SessionLog};
use dshr_tool_runtime::ToolOrigin;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;

impl ChatState {
    pub(crate) fn run_turn(
        &mut self,
        text: &str,
        attachments: &[DraftAttachment],
        emit: &mut dyn FnMut(TurnUpdate<'_>),
    ) -> Result<TurnResult, String> {
        let mut result = self.run_turn_once(text, attachments, None, emit);
        while let Some(access) = self.l2_access.clone() {
            if self.cancel.load(Ordering::Acquire)
                || !result
                    .as_ref()
                    .is_ok_and(|turn| turn.reason == TurnReason::Completed)
            {
                access.goals.disarm();
                break;
            }
            let Some(round) = access.goals.next_round(&self.log) else {
                break;
            };
            let prompt = format!(
                "Continue the active goal from the current session history.\nObjective: {}",
                round.objective
            );
            result = self.run_turn_once(&prompt, &[], Some(round), emit);
        }
        result
    }

    pub(crate) fn run_turn_once(
        &mut self,
        text: &str,
        attachments: &[DraftAttachment],
        goal_round: Option<dsh_goals::Goal>,
        emit: &mut dyn FnMut(TurnUpdate<'_>),
    ) -> Result<TurnResult, String> {
        self.refresh_agent_presets();
        if let Some(error) = &self.active_preset_error {
            return Err(error.clone());
        }
        let ts = now_ts();
        self.seq += 1;
        let user_id = format!("m-u{}", self.seq);
        let turn_id = format!("turn-{}", self.seq);
        let (payload, blobs) = user_message_payload(&user_id, text, attachments, &self.spill)?;
        if !blobs.is_empty() {
            let mut recorded: std::collections::HashSet<_> = self
                .log
                .events()
                .iter()
                .filter(|event| {
                    event.event_type == "spill.created" && event.payload["kind"] == "message.user"
                })
                .filter_map(|event| event.payload["id"].as_str().map(str::to_owned))
                .collect();
            for blob in blobs {
                if recorded.insert(blob.id.clone()) {
                    self.log
                        .append(EventSpec::durable(
                            "spill.created",
                            &ts,
                            blob.user_attachment_payload(),
                        ))
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        let current = user_message_from_payload(&payload).expect("nonempty user message");
        let source = if let Some(goal) = goal_round {
            goal.record_round(&mut self.log, &ts)?;
            goal.source()
        } else {
            json!({"kind":"user"})
        };
        self.log
            .append(EventSpec::durable("message.user", &ts, payload))
            .map_err(|error| error.to_string())?;
        self.log
            .append(EventSpec::durable(
                "turn.started",
                &ts,
                json!({ "turn_id": turn_id, "prompt_message_id": user_id, "source": source }),
            ))
            .map_err(|error| error.to_string())?;
        self.save_checked()?;
        emit(TurnUpdate::Preview(&self.log, None, None));

        // 历史从 durable 事件重建：多轮记忆靠它，重开应用也能接上。
        // 上下文预算在这里做：超大结果先外部化、超长工具结果头尾裁剪、超预算丢最旧，且不拆配对。
        let mut plan = self.plan_with_spill();
        if let Some((from_seq, to_seq)) = plan.dropped_range {
            // 丢弃了旧轮次：先让模型生成摘要，写 context.compacted，再按压缩后的历史请求。
            let summary = {
                let provider: &mut dyn ModelProvider = self.agent.provider_mut().as_mut();
                summarize_dropped(provider, &plan.dropped)
            };
            let summary_ts = now_ts();
            self.seq += 1;
            let summary_id = format!("m-sum{}", self.seq);
            let tokens_after = plan.tokens_after + estimate_tokens(&summary);
            let _ = self.log.append(EventSpec::durable(
                "message.assistant.settled",
                &summary_ts,
                json!({ "message_id": summary_id, "text": summary }),
            ));
            let _ = self.log.append(EventSpec::durable(
                "context.compacted",
                &summary_ts,
                json!({
                    "from_seq": from_seq,
                    "to_seq": to_seq,
                    "summary_message_id": summary_id,
                    "tokens_before": plan.tokens_before,
                    "tokens_after": tokens_after,
                }),
            ));
            self.save_checked()?;
            plan = self.plan_with_spill();
            eprintln!(
                "[chat] 已压缩 seq {from_seq}..{to_seq}：{} -> {} tokens",
                plan.tokens_before, plan.tokens_after
            );
            emit(TurnUpdate::Preview(&self.log, None, None));
        }
        let _ = self.log.append(EventSpec::durable(
            "context.measured",
            &ts,
            json!({
                "tokens": {
                    "input": plan.tokens_after,
                    "output": 0,
                    "total": plan.tokens_after,
                },
                "window": plan.window,
                "ratio": plan.ratio(),
                "pruned_tool_results": plan.pruned_tool_results,
                "dropped_messages": plan.dropped_messages,
                "spilled_tool_results": plan.spilled_tool_results(),
            }),
        ));
        self.save_checked()?;
        if plan.triggered() {
            eprintln!(
                "[chat] 上下文裁剪: {} -> {} tokens（窗口 {}，裁剪工具结果 {} 条，丢弃消息 {} 条）",
                plan.tokens_before,
                plan.tokens_after,
                plan.window,
                plan.pruned_tool_results,
                plan.dropped_messages
            );
        }
        emit(TurnUpdate::Preview(&self.log, None, None));
        let mut history = plan.messages;
        // 预算计入刚落盘的用户输入。Agent 会单独追加本轮消息，避免在模型请求中发送两份。
        if history.last() == Some(&current) {
            history.pop();
        }
        history.push(current);
        dshr_agent_context::spill::restore_history_attachments(
            &self.spill,
            &self.log,
            &mut history,
            |_| self.settings.model.supports_attachments(),
        );
        let current = history.pop().expect("current user message");

        let creator_enabled = !self.headless && creator::is_creator_session(&self.log);
        let preset = self
            .log
            .created()
            .and_then(|created| created.preset.clone());
        let step = ToolStep::default();
        let mut reply = String::new();
        let mut reasoning = String::new();
        let mut reason = TurnReason::Error;
        let mut event_error = None;
        // 非失败结束原因（用户取消）：决定 TurnEnded 后写 session.interrupted 而不是“处理失败”。
        let mut cancelled: Option<String> = None;
        let trial_revision_before_turn = self
            .creator_trials
            .get(&self.session_id)
            .map_or(0, |scope| scope.revision);
        // 父日志临时放进 Rc<RefCell>：流式回调（tool.* 事件）与 spawn_child 执行器
        // （child.spawned/finished）都要写同一份父日志，单借用无法同时满足。
        let log_cell = std::rc::Rc::new(std::cell::RefCell::new(std::mem::replace(
            &mut self.log,
            SessionLog::new(&self.session_id),
        )));
        {
            let mut executor = ChatExecutor {
                plugins: &mut self.plugins,
                host: &mut self.host,
                approvals: &self.approvals,
                creator: creator_enabled.then(|| creator::Session {
                    preset: preset.as_deref().expect("creator preset"),
                    workspace: &self.workspace,
                    sessions_dir: &self.sessions_dir,
                    scope: self
                        .creator_trials
                        .entry(self.session_id.clone())
                        .or_insert_with(|| {
                            creator::TrialScope::new(self.permission.clone(), &self.workspace)
                        }),
                    read_only: !self.permission.can_write(),
                }),
                step: step.clone(),
                program_hidden: HashSet::new(),
                headless: self.headless,
                diagnostics: self.diagnostics,
                log: log_cell.clone(),
                sessions_dir: &self.sessions_dir,
                parent_id: &self.session_id,
                workspace: &self.workspace,
                cancel: &self.cancel,
                settings: &self.settings,
                permission: &self.permission,
                preset_inputs: &self.preset_inputs,
                published_presets: &mut self.published_presets,
                allow_spawn: self.allow_spawn,
                l2_access: self.l2_access.clone(),
            };
            let log_for_events = log_cell.clone();
            let mut pending_call_seqs: HashMap<String, VecDeque<u64>> = HashMap::new();
            self.agent.run_turn_streaming_message(
                &history,
                current,
                &mut executor,
                &mut |event| match event {
                    AgentEvent::TextDelta(delta) => {
                        reply.push_str(delta);
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), Some(&reasoning)));
                    }
                    AgentEvent::ReasoningDelta(delta) => {
                        reasoning.push_str(delta);
                        if !self.headless {
                            emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), Some(&reasoning)));
                        }
                    }
                    AgentEvent::ToolCall(call) => {
                        let tools = step.borrow();
                        let definition = tools.get(&call.tool).map(|tool| &tool.definition);
                        let origin = definition.map_or("unknown", |tool| tool.origin.kind());
                        let presentation = definition.and_then(|tool| tool.presentation.as_deref())
                            .unwrap_or("generic");
                        let registration = match definition.map(|tool| &tool.origin) {
                            Some(ToolOrigin::Sidecar { registration_id, generation, plugin_id }) =>
                                Some((registration_id, generation, plugin_id)),
                            _ => None,
                        };
                        let mut data = json!({
                            "call_id": call.call_id,
                            "tool": call.tool,
                            "origin": origin,
                            "presentation": presentation,
                            "arguments": call.arguments
                        });
                        if let Some((id, generation, plugin_id)) = registration {
                            data["registration_id"] = json!(id);
                            data["generation"] = json!(generation);
                            data["plugin_id"] = json!(plugin_id);
                        }
                        drop(tools);
                        {
                            let mut log = log_for_events.borrow_mut();
                            if let Err(error) = record_reasoning(&mut log, &ts, &mut reasoning) {
                                event_error.get_or_insert_with(|| error.to_string());
                            }
                            let call_seq = log.len() as u64 + 1;
                            data["call_seq"] = json!(call_seq);
                            match log.append(EventSpec::durable("tool.call", &ts, data)) {
                                Ok(seq) => {
                                    pending_call_seqs.entry(call.call_id.clone()).or_default().push_back(seq);
                                    emit(TurnUpdate::Call { seq, tool: &call.tool });
                                }
                                Err(error) => { event_error.get_or_insert_with(|| error.to_string()); }
                            }
                        }
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), None));
                    }
                    AgentEvent::ToolResult { call_id, outcome } => {
                        let Some(call_seq) = pending_call_seqs.get_mut(call_id).and_then(VecDeque::pop_front) else {
                            return;
                        };
                        let status = {
                            let mut log = log_for_events.borrow_mut();
                            // P1-3：普通工具失败（拒绝/报错/预算耗尽）必须写 tool.result(is_error)，
                            // 只有“真正被取消”的调用才写 tool.aborted（reason 用 spec 枚举）。
                            let (event, status) = if outcome.is_error && self.cancel.load(Ordering::Relaxed) {
                                (EventSpec::durable(
                                    "tool.aborted",
                                    &ts,
                                    json!({
                                        "call_id": call_id,
                                        "call_seq": call_seq,
                                        "reason": "cancel"
                                    }),
                                ), "aborted")
                            } else {
                                (EventSpec::durable(
                                    "tool.result",
                                    &ts,
                                    json!({
                                        "call_id": call_id,
                                        "call_seq": call_seq,
                                        "is_error": outcome.is_error,
                                        // 对齐 DSH：content 给模型，value/meta 给展示与回放。
                                        "content": outcome.content,
                                        "value": outcome.value,
                                        "meta": outcome.meta,
                                    }),
                                ), if outcome.is_error { "error" } else { "completed" })
                            };
                            match log.append(event) {
                                Ok(_) => Some(status),
                                Err(error) => {
                                    event_error.get_or_insert_with(|| error.to_string());
                                    None
                                }
                            }
                        };
                        if let Some(status) = status {
                            emit(TurnUpdate::Result { seq: call_seq, status });
                        }
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), None));
                    }
                    AgentEvent::ToolCancelled { call_id, reason } => {
                        let Some(call_seq) = pending_call_seqs.get_mut(call_id).and_then(VecDeque::pop_front) else {
                            return;
                        };
                        let aborted = {
                            let mut log = log_for_events.borrow_mut();
                            log.append(EventSpec::durable(
                                "tool.aborted",
                                &ts,
                                json!({ "call_id": call_id, "call_seq": call_seq, "reason": reason }),
                            ))
                        };
                        match aborted {
                            Ok(_) => emit(TurnUpdate::Result { seq: call_seq, status: "aborted" }),
                            Err(error) => { event_error.get_or_insert_with(|| error.to_string()); }
                        }
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), None));
                    }
                    AgentEvent::TurnCancelled { reason } => {
                        cancelled = Some(reason.clone());
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), Some(&reasoning)));
                    }
                    AgentEvent::TurnFailed(err) => {
                        if !self.headless {
                            reply.push_str(&format!("处理失败：{err}"));
                        }
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), Some(&reasoning)));
                    }
                    AgentEvent::TurnEnded { .. } => reason = TurnReason::Completed,
                    AgentEvent::StepStarted { step, .. } if self.l2_access.is_some() => {
                        if let Err(error) = log_for_events.borrow_mut().append(EventSpec::durable(
                            "step.started", now_ts(), json!({ "turn_id": turn_id, "step": step }),
                        )) {
                            event_error.get_or_insert_with(|| error.to_string());
                        }
                    }
                    AgentEvent::TurnStarted { .. } | AgentEvent::StepStarted { .. } => {}
                },
            );
        }
        // 重复工具软提醒：本轮结束后一次性落 durable 事件（模型侧注入由 agent-core 完成）。
        {
            let repeat_ts = now_ts();
            let mut log = log_cell.borrow_mut();
            let written = append_repeat_reminders(&mut self.agent, &mut log, &repeat_ts);
            if written > 0 {
                eprintln!("[chat] tool.repeat: {written} 条（重复工具调用软提醒已落日志）");
            }
        }
        // 还原父日志：spawn_child 可能已往里追加 child.spawned / child.finished。
        self.log = std::rc::Rc::try_unwrap(log_cell)
            .map(|cell| cell.into_inner())
            .unwrap_or_else(|cell| cell.borrow().clone());
        if let Some(error) = event_error {
            return Err(error);
        }
        // 用户中断留痕（回放/审计可见；子会话的中断由 TreeCoordinator 传播）。
        // 取消可能由 Agent 的 TurnCancelled 上报，也可能发生在工具返回后，两个来源都覆盖。
        let cancelled_by_flag = self.cancel.load(Ordering::Relaxed);
        if cancelled.is_some() || cancelled_by_flag {
            if let Some(access) = &self.l2_access {
                access.children.stop_descendants(&self.session_id);
            }
            let reason = cancelled
                .clone()
                .unwrap_or_else(|| String::from("user_stop"));
            let _ = self.log.append(EventSpec::durable(
                "session.interrupted",
                now_ts(),
                json!({ "reason": reason, "by_parent": false }),
            ));
        }

        if self
            .creator_trials
            .get(&self.session_id)
            .map_or(0, |scope| scope.revision)
            != trial_revision_before_turn
            || self.adc_panels.iter().any(|panel| panel.trial)
        {
            self.refresh_adc_panels();
        }

        // 审批痕迹落盘（在工具事件之后：审批链用于审计/回放，不影响模型输入）。
        let records = self.approvals.take_records();
        for record in records {
            let approval_ts = now_ts();
            let _ = self.log.append(EventSpec::durable(
                "approval.asked",
                &approval_ts,
                json!({
                    "call_id": record.call_id,
                    "request": { "tool": record.tool, "arguments": record.arguments },
                    "policy": "desktop",
                }),
            ));
            let _ = self.log.append(EventSpec::durable(
                "approval.answered",
                &approval_ts,
                json!({
                    "call_id": record.call_id,
                    "outcome": record.outcome.as_str(),
                    "answerer": record.answerer,
                }),
            ));
        }

        self.seq += 1;
        let ts = now_ts();
        record_reasoning(&mut self.log, &ts, &mut reasoning).map_err(|error| error.to_string())?;
        // 取消且没有任何产出时不写空的 assistant 消息（避免 UI 出现空白气泡）。
        let cancelled_turn = cancelled_by_flag || cancelled.is_some();
        if cancelled_turn {
            reason = TurnReason::Cancelled;
        }
        if !(cancelled_turn && reply.trim().is_empty()) {
            self.log
                .append(EventSpec::durable(
                    "message.assistant.settled",
                    &ts,
                    json!({ "message_id": format!("m-a{}", self.seq), "text": reply }),
                ))
                .map_err(|error| error.to_string())?;
        }
        self.log
            .append(EventSpec::durable(
                "turn.ended",
                &ts,
                json!({ "turn_id": turn_id, "reason": reason.as_str() }),
            ))
            .map_err(|error| error.to_string())?;
        self.save_checked()?;
        emit(TurnUpdate::Preview(&self.log, None, None));
        Ok(TurnResult { reply, reason })
    }

    /// 同步版本（自检与无 UI 场景使用）：丢弃 UI 快照。
    pub(crate) fn submit(&mut self, text: &str) {
        let _ = self.stream_turn(text, &mut |_| {});
    }

    pub(crate) fn items(&self) -> Vec<Item> {
        build_from_log(&self.log)
    }

    /// 直接执行一次工具调用（自检用；走与真实轮次相同的策略/审批/沙箱路径）。
    pub(crate) fn execute_tool(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        self.refresh_agent_presets();
        if let Some(error) = &self.active_preset_error {
            return ToolOutcome::failure(error.clone());
        }
        let creator_enabled = !self.headless && creator::is_creator_session(&self.log);
        let preset = self
            .log
            .created()
            .and_then(|created| created.preset.clone());
        let log_cell = std::rc::Rc::new(std::cell::RefCell::new(std::mem::replace(
            &mut self.log,
            SessionLog::new(&self.session_id),
        )));
        let step = ToolStep::default();
        let mut executor = ChatExecutor {
            plugins: &mut self.plugins,
            host: &mut self.host,
            approvals: &self.approvals,
            creator: creator_enabled.then(|| creator::Session {
                preset: preset.as_deref().expect("creator preset"),
                workspace: &self.workspace,
                sessions_dir: &self.sessions_dir,
                scope: self
                    .creator_trials
                    .entry(self.session_id.clone())
                    .or_insert_with(|| {
                        creator::TrialScope::new(self.permission.clone(), &self.workspace)
                    }),
                read_only: !self.permission.can_write(),
            }),
            step: step.clone(),
            program_hidden: HashSet::new(),
            headless: self.headless,
            diagnostics: self.diagnostics,
            log: log_cell.clone(),
            sessions_dir: &self.sessions_dir,
            parent_id: &self.session_id,
            workspace: &self.workspace,
            cancel: &self.cancel,
            settings: &self.settings,
            permission: &self.permission,
            preset_inputs: &self.preset_inputs,
            published_presets: &mut self.published_presets,
            allow_spawn: self.allow_spawn,
            l2_access: self.l2_access.clone(),
        };
        executor.prepare_step();
        let outcome = executor.execute(call);
        drop(executor);
        self.log = std::rc::Rc::try_unwrap(log_cell)
            .map(|cell| cell.into_inner())
            .unwrap_or_else(|cell| cell.borrow().clone());
        outcome
    }
}

/// 把被丢弃的历史压缩成一段摘要；provider 失败时退回"截取开头"，不让压缩阻塞对话。
fn summarize_dropped(provider: &mut dyn ModelProvider, dropped: &[ChatMessage]) -> String {
    let transcript = transcript_text(dropped, 8_000);
    if transcript.trim().is_empty() {
        return String::from("（没有可压缩的历史）");
    }
    let request = CompletionRequest {
        messages: vec![
            ChatMessage::System(SUMMARY_PROMPT.to_owned()),
            ChatMessage::user(transcript.clone()),
        ],
        tool_specs: Vec::new(),
    };
    let reply = provider
        .complete_streaming(&request, &mut |_| {})
        .map(|reply| reply.text)
        .unwrap_or_default();
    let summary = reply.trim();
    if summary.is_empty() {
        let head: String = transcript.chars().take(600).collect();
        format!("（摘要生成失败，保留历史开头）\n{head}")
    } else {
        summary.chars().take(1_200).collect()
    }
}

/// 把消息拼成摘要输入；总长受 `max_chars` 限制。
fn transcript_text(messages: &[ChatMessage], max_chars: usize) -> String {
    let mut out = String::new();
    for message in messages {
        let (role, text) = match message {
            ChatMessage::System(text) => ("system", text.clone()),
            ChatMessage::User(text) => ("user", text.clone()),
            ChatMessage::UserWithAttachments { text, attachments } => {
                let names = attachments
                    .iter()
                    .map(|file| file.name.as_str())
                    .collect::<Vec<_>>()
                    .join("、");
                ("user", format!("{text}\n附件：{names}"))
            }
            ChatMessage::Assistant {
                text, tool_calls, ..
            } => {
                let mut body = text.clone();
                if !tool_calls.is_empty() {
                    let names: Vec<&str> =
                        tool_calls.iter().map(|call| call.name.as_str()).collect();
                    body.push_str(&format!("（调用工具 {}）", names.join(", ")));
                }
                ("assistant", body)
            }
            ChatMessage::Tool { content, .. } => ("tool", content.clone()),
            ChatMessage::ToolImages(images) => (
                "tool",
                images
                    .iter()
                    .map(|image| image.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        };
        out.push_str(&format!("[{role}] {text}\n"));
        if out.chars().count() >= max_chars {
            break;
        }
    }
    out.chars().take(max_chars).collect()
}

/// 把 agent 本轮触发的重复工具软提醒落成 durable `tool.repeat` 事件。
///
/// 模型侧的提醒由 `agent-core` 在每步工具执行完后以 system 消息注入（软提醒不失败）；
/// 这里只负责**宿主侧留痕**：回放/审计可见，UI 不需要专门渲染。返回写入条数。
///
/// payload 形状 `{tool, count, threshold, message}`（`message` 即注入模型的提醒文本）。
pub(crate) fn append_repeat_reminders<P: ModelProvider>(
    agent: &mut Agent<P>,
    log: &mut SessionLog,
    ts: &str,
) -> usize {
    let reminders = agent.take_repeat_reminders();
    let written = reminders.len();
    for reminder in reminders {
        let _ = log.append(EventSpec::durable(
            "tool.repeat",
            ts,
            json!({
                "tool": reminder.tool,
                "count": reminder.count,
                "threshold": reminder.threshold,
                "message": reminder.text,
            }),
        ));
    }
    written
}

fn record_reasoning(
    log: &mut SessionLog,
    timestamp: &str,
    reasoning: &mut String,
) -> Result<(), dshr_session_store::LogError> {
    if !reasoning.is_empty() {
        log.append(EventSpec::durable(
            "message.assistant.reasoning",
            timestamp,
            json!({ "text": std::mem::take(reasoning) }),
        ))?;
    }
    Ok(())
}
