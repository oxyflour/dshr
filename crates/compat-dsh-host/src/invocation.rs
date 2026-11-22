use super::*;

/// 批量调用等待读循环的轮询片段：既保证及时检查超时/取消，又不空转。
const WAIT_SLICE: Duration = Duration::from_millis(50);

/// 取消后的宽限：`invoke_batch` 发出 `tool.cancel` 后最多再等这么久
/// （P1-7：不能等到 per-call 120s；sidecar 忽略 cancel 时也必须按时结算）。
const CANCEL_GRACE: Duration = Duration::from_secs(2);

pub(super) struct ActiveCall {
    pub(super) call_id: String,
    pub(super) session_id: String,
    pub(super) tool: String,
    pub(super) turn: Option<OpenToolTurn>,
    pub(super) shell_command: Option<String>,
    pub(super) shell_background: bool,
    pub(super) job_id: Option<String>,
    pub(super) terminal_id: Option<String>,
    pub(super) terminal_value: Option<String>,
    pub(super) cancelled: bool,
    pub(super) approval_key: Option<String>,
    pub(super) goal_arguments: Option<Value>,
    pub(super) agent_arguments: Option<Value>,
    pub(super) parent: Option<String>,
    pub(super) deadline: Option<Instant>,
}

impl ActiveCall {
    pub(super) fn from_batch(
        call: &BatchInvokeCall,
        session: &str,
        turn: Option<OpenToolTurn>,
    ) -> Self {
        Self {
            call_id: call.call_id.clone(),
            session_id: session.to_owned(),
            tool: call.tool.clone(),
            turn,
            shell_command: if matches!(call.tool.as_str(), "bash" | "pwsh") {
                call.arguments["command"].as_str().map(str::to_owned)
            } else {
                None
            },
            shell_background: call.arguments["run_in_background"] == true,
            job_id: call.arguments["job_id"].as_str().map(str::to_owned),
            terminal_id: call.arguments["sessionId"].as_str().map(str::to_owned),
            terminal_value: match call.tool.as_str() {
                "terminal_send" => Some(
                    call.arguments["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                ),
                "terminal_signal" => call.arguments["signal"].as_str().map(str::to_owned),
                _ => None,
            },
            cancelled: false,
            approval_key: call.approval_key.clone(),
            goal_arguments: matches!(
                call.tool.as_str(),
                "get_goal" | "create_goal" | "update_goal"
            )
            .then(|| call.arguments.clone()),
            agent_arguments: matches!(
                call.tool.as_str(),
                "subagent"
                    | "send_message"
                    | "interrupt_agent"
                    | "team_task_create"
                    | "team_task_update"
                    | "schedule_create"
                    | "schedule_edit"
            )
            .then(|| call.arguments.clone()),
            parent: None,
            deadline: (!call.timeout.is_zero()).then(|| Instant::now() + call.timeout),
        }
    }
}

impl SidecarHost {
    pub fn invoke(
        &mut self,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<InvokeOutcome, HostError> {
        let call_id = self.begin_invoke(tool, arguments)?;
        self.wait_terminal(&call_id, timeout)
    }

    pub fn begin_invoke(&mut self, tool: &str, arguments: Value) -> Result<String, HostError> {
        self.begin_invoke_in_session(tool, arguments, None)
    }

    /// 带显式会话 id 的 `tool.invoke`（`None` = 兼容旧 `s-probe` 占位）。
    pub fn begin_invoke_in_session(
        &mut self,
        tool: &str,
        arguments: Value,
        session_id: Option<&str>,
    ) -> Result<String, HostError> {
        self.seq += 1;
        let call_id = format!("call-{}", self.seq);
        let request_id = format!("req-invoke-{}", self.seq);
        self.refresh_registrations()?;
        let mut call = BatchInvokeCall::new(&call_id, tool, arguments, Duration::ZERO);
        call.session_id = session_id.map(str::to_owned);
        self.send_invoke(request_id, &call)?;
        Ok(call_id)
    }

    pub(super) fn send_invoke(
        &mut self,
        request_id: String,
        call: &BatchInvokeCall,
    ) -> Result<(), HostError> {
        let session_id = call.session_id.as_deref();
        let revision = session_id
            .and_then(|session| self.preset_bindings.get(session))
            .map(|(_, revision)| revision);
        let matches_call = |registration: &&Registration| {
            registration.name == call.tool
                && (registration.session_id.is_none()
                    || registration.session_id.as_deref() == session_id)
                && (registration.preset_revision.is_none()
                    || registration.preset_revision.as_ref() == revision)
        };
        let binding = match &call.binding {
            Some(ToolOrigin::Sidecar {
                plugin_id,
                registration_id,
                generation,
            }) => self
                .registrations
                .iter()
                .filter(matches_call)
                .find(|registration| {
                    generation == &self.generation
                        && plugin_id == &registration.plugin_id
                        && registration_id == &registration.registration_id
                })
                .ok_or_else(|| {
                    HostError::Protocol(format!(
                        "E_TOOL_UNAVAILABLE: {} ({registration_id})",
                        call.tool
                    ))
                })
                .map(Some)?,
            Some(_) => return Err(HostError::Protocol("invalid sidecar binding".to_owned())),
            None => self
                .registrations
                .iter()
                .filter(matches_call)
                .max_by_key(|registration| {
                    (
                        registration.session_id.is_some(),
                        registration.preset_revision.is_some(),
                    )
                }),
        };
        let mut envelope = Envelope::host(
            "tool.invoke",
            &request_id,
            json!({ "name": call.tool, "arguments": call.arguments }),
        )
        .with_version(self.negotiated_protocol)
        .with_generation(self.generation.clone())
        .with_call(&call.call_id)
        .with_session(session_id.unwrap_or("s-probe"));
        let turn = call.turn.filter(|_| self.agent_tool_context_supported);
        if let Some(turn) = turn {
            envelope.payload["turn"] =
                json!({ "number": turn.number, "start_seq": turn.start_seq });
        }
        if self.agent_tool_context_supported {
            if let Some((provider, model)) = &call.model_route {
                envelope.payload["model"] = json!({ "provider": provider, "model": model });
            }
        }
        if let Some(binding) = binding {
            envelope.registration_id = Some(binding.registration_id.clone());
            if !binding.plugin_id.is_empty() {
                envelope.plugin_id = Some(binding.plugin_id.clone());
            }
        }
        self.write(&envelope)?;
        self.active_calls.insert(
            request_id,
            ActiveCall::from_batch(call, session_id.unwrap_or("s-probe"), turn),
        );
        Ok(())
    }

    /// 用调用方指定的 `call_id` 发起调用：Session Log 与 IPC 必须是同一个身份。
    pub fn invoke_as(
        &mut self,
        call_id: &str,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<InvokeOutcome, HostError> {
        self.invoke_as_cancellable(call_id, tool, arguments, timeout, None)
    }

    /// 可取消的 [`SidecarHost::invoke_as`]。
    ///
    /// 等待期间每 100ms 轮询一次 `cancel`；置位时发送一次 `tool.cancel`
    /// （`tool.cancel` RPC 之前只有定义、没有调用点），再给 sidecar 最多 2s 宽限
    /// 等 `tool.aborted` / `tool.result`；宽限期满仍无终态则返回
    /// [`HostError::Cancelled`]。正常等待的片段超时**不再**直接判 `Timeout`，
    /// 绝对 deadline 语义保持不变。
    pub fn invoke_as_cancellable(
        &mut self,
        call_id: &str,
        tool: &str,
        arguments: Value,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<InvokeOutcome, HostError> {
        self.invoke_as_cancellable_in_session(call_id, None, tool, arguments, timeout, cancel)
    }

    /// [`SidecarHost::invoke_as_cancellable`] + 显式 `session_id` 透传（P2-10）。
    ///
    /// `session_id` 是 L2 服务方法按会话授权的唯一身份锚点；`None` 保持旧行为
    /// （`s-probe` 占位，供探针/旧测试）。
    pub fn invoke_as_cancellable_in_session(
        &mut self,
        call_id: &str,
        session_id: Option<&str>,
        tool: &str,
        arguments: Value,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<InvokeOutcome, HostError> {
        self.seq += 1;
        let request_id = format!("req-invoke-{}", self.seq);
        self.refresh_registrations()?;
        let mut call = BatchInvokeCall::new(call_id, tool, arguments, timeout);
        call.session_id = session_id.map(str::to_owned);
        self.send_invoke(request_id.clone(), &call)?;
        self.finish_call(&request_id, call_id, timeout, cancel)
    }

    /// 单调用 + 显式会话 id 的便捷入口（无取消句柄）。
    pub fn invoke_in_session(
        &mut self,
        call_id: &str,
        session_id: Option<&str>,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<InvokeOutcome, HostError> {
        self.invoke_as_cancellable_in_session(call_id, session_id, tool, arguments, timeout, None)
    }

    pub fn cancel(&mut self, call_id: &str) -> Result<(), HostError> {
        let request_id = self.request_for_call(call_id);
        self.send_cancel(request_id.as_deref(), call_id, "user")
    }

    pub(super) fn request_for_call(&self, call_id: &str) -> Option<String> {
        self.active_calls
            .iter()
            .find_map(|(request, call)| (call.call_id == call_id).then(|| request.clone()))
    }

    pub(super) fn send_cancel(
        &mut self,
        request_id: Option<&str>,
        call_id: &str,
        reason: &str,
    ) -> Result<(), HostError> {
        let mut payload = json!({ "call_id": call_id, "reason": reason });
        if let Some(request_id) = request_id {
            payload["tool_request_id"] = json!(request_id);
            if let Some(call) = self.active_calls.get_mut(request_id) {
                call.cancelled = true;
            }
            self.finish_program_children(request_id);
            self.release_tool_resources(request_id);
        }
        self.seq += 1;
        let envelope = Envelope::host("tool.cancel", format!("req-cancel-{}", self.seq), payload)
            .with_generation(self.generation.clone())
            .with_call(call_id.to_owned());
        self.write(&envelope)
    }

    /// 批量调用（M3 并行工具，DSH `maxParallelToolCalls`）：
    ///
    /// 按 `max_parallel` 分**波次**：每一波先把本波所有 `tool.invoke` 一次写出
    /// （request_id 区分），再在同一读循环里收集各自 `tool.chunk` / `tool.result` /
    /// `tool.aborted`，按 `request_id` 关联。返回值与 `calls` 一一对应（原顺序），
    /// 单个调用失败/超时不影响其它调用。
    ///
    /// 语义：
    /// - **per-call timeout**：每个调用独立 deadline；超时回 `HostError::Timeout`
    ///   并向 sidecar 发 `tool.cancel`，迟到结果被丢弃；
    /// - **取消**：`cancel` 置位时对本波在途调用发一次 `tool.cancel`，随后继续收结果
    ///   （`tool.aborted` 作为该调用的终态返回，聊天层映射为失败）；不再启动后续波次；
    /// - **迟到/未知结果**：丢弃已结算请求的消息，即使后续请求复用同一个 `call_id`；
    /// - **读循环仍服务** `fs.write.request` / `host.service.*` / `log.emit`，不会与
    ///   现有单调用路径互相死锁。
    pub fn invoke_batch(
        &mut self,
        calls: &[BatchInvokeCall],
        max_parallel: usize,
        cancel: Option<&AtomicBool>,
    ) -> Vec<Result<InvokeOutcome, HostError>> {
        struct BatchSlot {
            request_id: String,
            call_id: String,
            started: Instant,
            deadline: Instant,
            /// 取消宽限截止（发出 tool.cancel 后置位；P1-7）。
            cancel_deadline: Option<Instant>,
            chunks: Vec<String>,
            done: bool,
        }

        let mut results: Vec<Option<Result<InvokeOutcome, HostError>>> =
            (0..calls.len()).map(|_| None).collect();
        if calls.is_empty() {
            return Vec::new();
        }
        let parallel = max_parallel.max(1);
        let mut next = 0usize;
        'outer: while next < calls.len() {
            if let Err(error) = self.refresh_registrations() {
                for result in &mut results[next..] {
                    *result = Some(Err(HostError::Protocol(error.to_string())));
                }
                break;
            }
            let end = (next + parallel).min(calls.len());
            let mut slots: Vec<BatchSlot> = Vec::with_capacity(end - next);
            let mut by_request: HashMap<String, usize> = HashMap::new();

            // 第一波写：把本波调用的 tool.invoke 全部写出再收结果。
            for (offset, call) in calls[next..end].iter().enumerate() {
                let started = Instant::now();
                self.seq += 1;
                let request_id = format!("req-batch-{}", self.seq);
                let write = self.send_invoke(request_id.clone(), call);
                let done = write.is_err();
                if let Err(error) = write {
                    results[next + offset] = Some(Err(error));
                } else {
                    by_request.insert(request_id.clone(), offset);
                }
                slots.push(BatchSlot {
                    request_id,
                    call_id: call.call_id.clone(),
                    started,
                    deadline: started + call.timeout.max(Duration::from_millis(1)),
                    cancel_deadline: None,
                    chunks: Vec::new(),
                    done,
                });
            }

            let mut cancel_sent = false;
            let mut fatal: Option<String> = None;
            loop {
                // 取消：对本波所有在途调用发一次 tool.cancel，并置 2s 宽限；
                // 宽限内收到终态按终态结算，宽限到期未结算的一律 `Cancelled`
                //（sidecar 忽略 cancel 时不能等到 per-call 120s，P1-7）。
                if !cancel_sent && cancel.is_some_and(|token| token.load(Ordering::Relaxed)) {
                    cancel_sent = true;
                    let grace_deadline = Instant::now() + CANCEL_GRACE;
                    for slot in slots.iter_mut().filter(|slot| !slot.done) {
                        let _ = self.send_cancel(Some(&slot.request_id), &slot.call_id, "user");
                        slot.cancel_deadline = Some(grace_deadline);
                    }
                }
                let now = Instant::now();
                for (offset, slot) in slots.iter_mut().enumerate() {
                    if slot.done {
                        continue;
                    }
                    if slot.cancel_deadline.is_some_and(|deadline| now >= deadline) {
                        slot.done = true;
                        self.finish_active_call(&slot.request_id);
                        results[next + offset] = Some(Err(HostError::Cancelled(format!(
                            "tool {} did not settle within {}s of cancellation",
                            slot.call_id,
                            CANCEL_GRACE.as_secs()
                        ))));
                        continue;
                    }
                    if now < slot.deadline {
                        continue;
                    }
                    slot.done = true;
                    if slot.cancel_deadline.is_some() {
                        results[next + offset] =
                            Some(Err(HostError::Cancelled(format!("tool {}", slot.call_id))));
                    } else {
                        let _ = self.send_cancel(Some(&slot.request_id), &slot.call_id, "timeout");
                        results[next + offset] =
                            Some(Err(HostError::Timeout(format!("tool {}", slot.call_id))));
                    }
                    self.finish_active_call(&slot.request_id);
                }
                if slots.iter().all(|slot| slot.done) {
                    break;
                }
                let mut wait = WAIT_SLICE;
                for slot in slots.iter().filter(|slot| !slot.done) {
                    let mut slot_deadline = slot.deadline;
                    if let Some(cancel_deadline) = slot.cancel_deadline {
                        slot_deadline = slot_deadline.min(cancel_deadline);
                    }
                    let remaining = slot_deadline.saturating_duration_since(now);
                    wait = wait.min(remaining.max(Duration::from_millis(1)));
                }
                match self.rx.recv_timeout(wait) {
                    Ok(envelope) => {
                        match self.process_tool_lifecycle(&envelope) {
                            Ok(true) => continue,
                            Ok(false) => {}
                            Err(error) => {
                                fatal = Some(error.to_string());
                                break;
                            }
                        }
                        let served = match self.maybe_serve_fs_write(&envelope) {
                            Ok(served) => served,
                            Err(err) => {
                                fatal = Some(format!("serving fs.write during batch: {err}"));
                                break;
                            }
                        };
                        if served {
                            continue;
                        }
                        let served = match self.maybe_serve_host_service(&envelope) {
                            Ok(served) => served,
                            Err(err) => {
                                fatal = Some(format!("serving host.service during batch: {err}"));
                                break;
                            }
                        };
                        if served {
                            continue;
                        }
                        if envelope.kind == "log.emit" {
                            self.note_log_emit(&envelope);
                            continue;
                        }
                        let Some(offset) = by_request.get(&envelope.request_id).copied() else {
                            // 其它调用/上一波超时后的迟到结果：丢弃（与本波无关）。
                            continue;
                        };
                        let slot = &mut slots[offset];
                        if slot.done || envelope.call_id.as_deref() != Some(slot.call_id.as_str()) {
                            continue;
                        }
                        match envelope.kind.as_str() {
                            "tool.chunk" => {
                                if let Some(text) =
                                    envelope.payload.get("text").and_then(Value::as_str)
                                {
                                    slot.chunks.push(text.to_owned());
                                }
                            }
                            "tool.result" | "tool.aborted" => {
                                let chunks = std::mem::take(&mut slot.chunks);
                                let elapsed = slot.started.elapsed();
                                slot.done = true;
                                self.finish_active_call(&slot.request_id);
                                results[next + offset] = Some(Ok(InvokeOutcome {
                                    chunks,
                                    terminal: envelope,
                                    elapsed,
                                }));
                            }
                            _ => {}
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => {
                        for (offset, slot) in slots.iter_mut().enumerate() {
                            if !slot.done {
                                slot.done = true;
                                results[next + offset] = Some(Err(HostError::Closed));
                            }
                        }
                        fatal = Some("sidecar closed the pipe during batch".to_owned());
                        break;
                    }
                }
            }

            if let Some(message) = fatal {
                for slot in &slots {
                    self.finish_active_call(&slot.request_id);
                }
                for slot in results.iter_mut().skip(next).take(calls.len() - next) {
                    if slot.is_none() {
                        *slot = Some(Err(HostError::Protocol(message.clone())));
                    }
                }
                break 'outer;
            }
            next = end;
            if cancel_sent {
                // 已取消：不再启动后续波次；本波已发出的调用已全部结算。
                for slot in results.iter_mut().skip(next) {
                    if slot.is_none() {
                        *slot = Some(Err(HostError::Cancelled(format!(
                            "batch cancelled before wave starting at {next}"
                        ))));
                    }
                }
                break 'outer;
            }
        }

        results
            .into_iter()
            .map(|slot| {
                slot.unwrap_or_else(|| {
                    Err(HostError::Protocol(
                        "batch slot missing after invoke_batch".to_owned(),
                    ))
                })
            })
            .collect()
    }

    /// 读到达终止事件（`tool.result` / `tool.aborted`）为止，收集中间 chunk。
    pub fn wait_terminal(
        &mut self,
        call_id: &str,
        timeout: Duration,
    ) -> Result<InvokeOutcome, HostError> {
        let request_id = self
            .request_for_call(call_id)
            .ok_or_else(|| HostError::Protocol(format!("tool {call_id} has no active request")))?;
        self.finish_call(&request_id, call_id, timeout, None)
    }

    pub(super) fn finish_call(
        &mut self,
        request_id: &str,
        call_id: &str,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<InvokeOutcome, HostError> {
        let result = self.wait_terminal_cancellable(request_id, call_id, timeout, cancel);
        self.finish_active_call(request_id);
        result
    }

    pub(super) fn finish_active_call(&mut self, request_id: &str) {
        self.finish_program_children(request_id);
        self.active_calls.remove(request_id);
        self.release_tool_resources(request_id);
    }

    pub(super) fn release_tool_resources(&self, request_id: &str) {
        if let Some(router) = &self.host_services {
            if let Ok(mut router) = router.lock() {
                router.release_tool_call(request_id);
            }
        }
    }

    pub(super) fn wait_terminal_cancellable(
        &mut self,
        request_id: &str,
        call_id: &str,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<InvokeOutcome, HostError> {
        let started = Instant::now();
        let deadline = started + timeout;
        let mut chunks = Vec::new();
        // `tool.cancel` 只发一次；置位后改用"宽限 deadline"收尾。
        let mut cancel_deadline: Option<Instant> = None;
        loop {
            if cancel_deadline.is_none()
                && cancel.is_some_and(|token| token.load(Ordering::Relaxed))
            {
                self.send_cancel(Some(request_id), call_id, "user")?;
                cancel_deadline = Some(Instant::now() + CANCEL_GRACE);
            }
            let now = Instant::now();
            let effective_deadline = cancel_deadline.map_or(deadline, |grace| grace.min(deadline));
            let remaining = effective_deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                return if cancel_deadline.is_some() {
                    Err(HostError::Cancelled(format!("tool {call_id}")))
                } else {
                    let _ = self.send_cancel(Some(request_id), call_id, "timeout");
                    Err(HostError::Timeout(format!("tool {call_id}")))
                };
            }

            // 100ms 片段超时不是失败：回到循环头部重新检查 cancel / deadline。
            let envelope = match self.rx.recv_timeout(remaining.min(WAIT_SLICE)) {
                Ok(envelope) => envelope,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(HostError::Closed),
            };

            if self.process_tool_lifecycle(&envelope)? {
                continue;
            }

            // 关键：等待 tool.result 期间必须同时服务 sidecar 的写入请求，
            // 否则 sidecar 等 fs.write.result、宿主等 tool.result，双向死锁。
            if self.maybe_serve_fs_write(&envelope)? {
                continue;
            }
            // v2：等待工具结果期间同样要服务 host.service.request/result/cancel。
            if self.maybe_serve_host_service(&envelope)? {
                continue;
            }
            if envelope.kind == "log.emit" {
                self.note_log_emit(&envelope);
                continue;
            }

            if envelope.request_id != request_id || envelope.call_id.as_deref() != Some(call_id) {
                continue;
            }
            match envelope.kind.as_str() {
                "tool.chunk" => {
                    if let Some(text) = envelope.payload.get("text").and_then(Value::as_str) {
                        chunks.push(text.to_owned());
                    }
                }
                "tool.result" | "tool.aborted" => {
                    return Ok(InvokeOutcome {
                        chunks,
                        terminal: envelope,
                        elapsed: started.elapsed(),
                    });
                }
                _ => {}
            }
        }
    }
}
