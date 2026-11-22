use super::*;

impl SidecarHost {
    pub(super) fn callback_from_inactive_call(&self, envelope: &Envelope) -> bool {
        envelope
            .payload
            .get("tool_request_id")
            .and_then(Value::as_str)
            .is_some_and(|id| {
                !self
                    .active_calls
                    .get(id)
                    .is_some_and(|call| self.call_is_current(call))
            })
    }

    pub(super) fn bound_service_call(&self, envelope: &Envelope) -> Option<&ActiveCall> {
        let request_id = envelope.payload["tool_request_id"].as_str()?;
        let call = self.active_calls.get(request_id)?;
        (self.call_is_current(call)
            && envelope.call_id.as_deref() == Some(call.call_id.as_str())
            && envelope.session_id.as_deref() == Some(call.session_id.as_str()))
        .then_some(call)
    }

    pub(super) fn service_callback_denial(&self, envelope: &Envelope) -> Option<&'static str> {
        match envelope.service_key.as_deref() {
            Some(TOOL_GOALS_V1) => {
                let args = &envelope.payload["args"];
                let allowed = self.host_caps.iter().any(|cap| cap == CAP_TOOL_GOALS)
                    && self.bound_service_call(envelope).is_some_and(|call| {
                        call.turn
                            .is_some_and(|turn| args["start_seq"] == turn.start_seq)
                            && call.goal_arguments.as_ref() == Some(&args["arguments"])
                            && args["tool"] == call.tool
                    });
                (!allowed)
                    .then_some("goal operation requires its matching active tool and open turn")
            }
            Some(TOOL_SESSION_QUERY_V1) => {
                let allowed = self
                    .host_caps
                    .iter()
                    .any(|cap| cap == CAP_TOOL_SESSION_QUERY)
                    && self.bound_service_call(envelope).is_some_and(|call| {
                        call.turn.is_some()
                            && matches!(
                                call.tool.as_str(),
                                "session_search"
                                    | "session_event_search"
                                    | "session_trace"
                                    | "session_event_trace"
                                    | "session_event_read"
                            )
                    });
                (!allowed).then_some("session query requires a matching active session query tool")
            }
            Some(TOOL_SUBAGENTS_V1) => {
                let method = envelope.payload["method"].as_str().unwrap_or_default();
                let args = &envelope.payload["args"];
                let enabled = self.host_caps.iter().any(|cap| cap == CAP_TOOL_SUBAGENTS);
                let step = method == "notices"
                    && self.preparing_step.is_some()
                    && envelope.session_id == self.preparing_step
                    && envelope.payload["tool_request_id"].is_null();
                let allowed = enabled
                    && (step
                        || self.bound_service_call(envelope).is_some_and(|call| {
                            call.turn.is_some()
                                && match method {
                                    "list" => matches!(
                                        call.tool.as_str(),
                                        "list_agents"
                                            | "send_message"
                                            | "interrupt_agent"
                                            | "subagent"
                                            | "team_task_list"
                                            | "team_task_update"
                                    ),
                                    "pending" => call.tool == "wait_agent",
                                    "start" if call.tool == "subagent" => {
                                        call.agent_arguments.as_ref() == Some(&args["arguments"])
                                    }
                                    "message" if call.tool == "send_message" => {
                                        call.agent_arguments.as_ref() == Some(&args["arguments"])
                                    }
                                    "interrupt" if call.tool == "interrupt_agent" => {
                                        call.agent_arguments.as_ref() == Some(&args["arguments"])
                                    }
                                    "workflow.start" | "workflow.poll" | "workflow.stop" => {
                                        matches!(call.tool.as_str(), "workflow" | "ralph")
                                    }
                                    _ => false,
                                }
                        }));
                (!allowed).then_some(
                    "subagent operation requires the matching authorized call and session",
                )
            }
            Some(TOOL_TEAMS_V1) => {
                let method = envelope.payload["method"].as_str().unwrap_or_default();
                let args = &envelope.payload["args"];
                let allowed = self.host_caps.iter().any(|cap| cap == CAP_TOOL_TEAMS)
                    && self.bound_service_call(envelope).is_some_and(|call| {
                        call.turn.is_some()
                            && match method {
                                "list" => matches!(
                                    call.tool.as_str(),
                                    "team_task_list" | "team_task_get" | "team_task_update"
                                ),
                                "create" if call.tool == "team_task_create" => {
                                    call.agent_arguments.as_ref() == Some(&args["arguments"])
                                }
                                "update" if call.tool == "team_task_update" => {
                                    call.agent_arguments.as_ref() == Some(&args["arguments"])
                                }
                                _ => false,
                            }
                    });
                (!allowed).then_some(
                    "team operation requires the matching authorized team tool and session",
                )
            }
            Some(TOOL_SCHEDULES_V1) => {
                let method = envelope.payload["method"].as_str().unwrap_or_default();
                let args = &envelope.payload["args"];
                let enabled = self.host_caps.iter().any(|cap| cap == CAP_TOOL_SCHEDULES);
                let step = matches!(method, "due" | "claim")
                    && self.preparing_step.is_some()
                    && envelope.session_id == self.preparing_step
                    && envelope.payload["tool_request_id"].is_null();
                let allowed = enabled
                    && (step
                        || self.bound_service_call(envelope).is_some_and(|call| {
                            call.turn.is_some()
                                && match method {
                                    "list" => call.tool == "schedule_list",
                                    "view" => matches!(
                                        call.tool.as_str(),
                                        "schedule_view" | "schedule_edit"
                                    ),
                                    "create" if call.tool == "schedule_create" => {
                                        call.agent_arguments.as_ref() == Some(&args["arguments"])
                                    }
                                    "edit" if call.tool == "schedule_edit" => {
                                        call.agent_arguments.as_ref() == Some(&args["arguments"])
                                    }
                                    _ => false,
                                }
                        }));
                (!allowed).then_some(
                    "schedule operation requires the matching authorized tool and root session",
                )
            }
            Some(SESSION_LOG_V1)
                if envelope.payload["method"] == "events.append"
                    && envelope.payload["args"]["event_type"]
                        .as_str()
                        .is_some_and(|name| {
                            let name = name.trim();
                            name.starts_with("subagent/")
                                || matches!(
                                    name,
                                    "goal/change"
                                        | "turn.started"
                                        | "turn.ended"
                                        | "step.started"
                                        | "message.user"
                                        | "child.spawned"
                                        | "child.finished"
                                        | "tool.program.admitted"
                                )
                        }) =>
            {
                Some("goal and turn authority events must originate with the Rust session runtime")
            }
            Some(IMAGE_ATTACHMENTS_V1) => {
                let allowed = self
                    .host_caps
                    .iter()
                    .any(|cap| cap == CAP_IMAGE_ATTACHMENTS)
                    && self.bound_service_call(envelope).is_some_and(|call| {
                        let method = envelope.payload["method"].as_str();
                        match call.tool.as_str() {
                            "read_image" => matches!(method, Some("route" | "save")),
                            "office_render" => {
                                self.agent_tool_context_supported
                                    && matches!(method, Some("route" | "rendered.page"))
                            }
                            _ => false,
                        }
                    });
                (!allowed).then_some("image attachments require a matching active image tool call")
            }
            Some(TOOL_PROCESS_V1) => {
                let allowed = self.bound_service_call(envelope).is_some_and(|call| {
                    let (cap, engine) = match call.tool.as_str() {
                        "glob" | "grep" => (CAP_SEARCH_PROCESS, "ripgrep"),
                        "pwsh" | "bash" => (CAP_SHELL_PROCESS, call.tool.as_str()),
                        _ => return false,
                    };
                    if !self.host_caps.iter().any(|available| available == cap) {
                        return false;
                    }
                    if envelope.payload["method"] != "start" {
                        return true;
                    }
                    let args = &envelope.payload["args"];
                    args["engine"] == engine
                        && (engine == "ripgrep"
                            || args["command"].as_str().is_some_and(|command| {
                                call.shell_command.as_deref() == Some(command)
                            }))
                });
                (!allowed).then_some("tool processes require the matching authorized tool call")
            }
            Some(USER_QUESTIONS_V1) => {
                let allowed = self.host_caps.iter().any(|cap| cap == CAP_USER_QUESTIONS)
                    && self
                        .bound_service_call(envelope)
                        .is_some_and(|call| call.tool == "ask_user_question");
                (!allowed)
                    .then_some("user questions require a matching active ask_user_question call")
            }
            Some(TOOL_JOBS_V1) => {
                let method = envelope.payload["method"].as_str().unwrap_or_default();
                let args = &envelope.payload["args"];
                if method == "notices" {
                    let allowed = self.host_caps.iter().any(|cap| cap == CAP_BACKGROUND_JOBS)
                        && self.preparing_step.is_some()
                        && envelope.session_id == self.preparing_step
                        && envelope.payload["tool_request_id"].is_null();
                    return (!allowed)
                        .then_some("job notices require an active agent step in the same session");
                }
                let allowed = self.host_caps.iter().any(|cap| cap == CAP_BACKGROUND_JOBS)
                    && self
                        .bound_service_call(envelope)
                        .is_some_and(|call| match method {
                            "reserve" | "start" | "confirm" => {
                                call.shell_background
                                    && matches!(call.tool.as_str(), "pwsh" | "bash")
                                    && (method != "start"
                                        || (args["engine"] == call.tool
                                            && args["command"].as_str()
                                                == call.shell_command.as_deref()))
                            }
                            "list" => {
                                matches!(call.tool.as_str(), "job_list" | "job_output" | "job_kill")
                            }
                            "inspect" => {
                                matches!(call.tool.as_str(), "job_output" | "job_kill")
                                    && call.job_id.as_deref() == args["id"].as_str()
                            }
                            "read" | "advance" => {
                                call.tool == "job_output"
                                    && call.job_id.as_deref() == args["id"].as_str()
                            }
                            "stop" => {
                                call.tool == "job_kill"
                                    && call.job_id.as_deref() == args["id"].as_str()
                            }
                            _ => false,
                        });
                (!allowed).then_some("background jobs require the matching authorized tool call")
            }
            Some(TOOL_TERMINALS_V1) => {
                let method = envelope.payload["method"].as_str().unwrap_or_default();
                let args = &envelope.payload["args"];
                let operation = args["operation"].as_str().unwrap_or_default();
                let allowed = self.host_caps.iter().any(|cap| cap == CAP_TERMINAL_PROCESS)
                    && self.bound_service_call(envelope).is_some_and(|call| {
                        let tool = call.tool.as_str();
                        let same_id = args["id"].as_str() == call.terminal_id.as_deref();
                        match method {
                            "reserve" | "confirm" => {
                                matches!(tool, "terminal_open" | "pwsh" | "bash")
                            }
                            "list" => {
                                matches!(tool, "terminal_list" | "terminal_read" | "pwsh" | "bash")
                            }
                            "closed" => {
                                (tool == "terminal_close" && same_id)
                                    || matches!(tool, "pwsh" | "bash")
                            }
                            "allow" if matches!(tool, "pwsh" | "bash") => match operation {
                                "send" => {
                                    args["origin_command"].as_str() == call.shell_command.as_deref()
                                }
                                "read" | "close" => true,
                                _ => false,
                            },
                            "allow" if same_id => match operation {
                                "send" => {
                                    tool == "terminal_send"
                                        && args["value"].as_str() == call.terminal_value.as_deref()
                                }
                                "read" => tool == "terminal_read",
                                "signal" => {
                                    tool == "terminal_signal"
                                        && args["value"].as_str() == call.terminal_value.as_deref()
                                }
                                "close" => tool == "terminal_close",
                                _ => false,
                            },
                            _ => false,
                        }
                    });
                (!allowed).then_some(
                    "terminal operation requires the matching authorized tool and session",
                )
            }
            Some(SESSION_LOG_V1)
                if envelope.payload["method"] == "events.append"
                    && envelope.payload["args"]["event_type"]
                        .as_str()
                        .is_some_and(|name| {
                            matches!(
                                name.trim(),
                                tool_terminals::TERMINAL_OPENED | tool_terminals::TERMINAL_CLOSED
                            )
                        }) =>
            {
                Some("terminal lifecycle events must originate with the Rust terminal service")
            }
            Some(SESSION_LOG_V1)
                if envelope.payload["method"] == "events.append"
                    && envelope.payload["args"]["event_type"]
                        .as_str()
                        .is_some_and(|name| name.trim() == "deliverables/presented") =>
            {
                let data = &envelope.payload["args"]["data"];
                let allowed = self.bound_service_call(envelope).is_some_and(|call| {
                    call.tool == "present"
                        && data["callId"] == call.call_id
                        && call
                            .turn
                            .is_some_and(|turn| data["turn"].as_u64() == Some(turn.number))
                });
                (!allowed)
                    .then_some("file delivery requires a matching present call inside an open turn")
            }
            _ => None,
        }
    }

    /// 读循环的公共服务点：遇到 sidecar 的写入请求就地裁决并回复。
    ///
    /// 返回 `true` 表示该信封已被消费，调用方继续循环、不再把它当作
    /// `tool.result`/`tool.register` 处理。
    pub(super) fn maybe_serve_fs_write(&mut self, envelope: &Envelope) -> Result<bool, HostError> {
        if envelope.kind != KIND_FS_WRITE_REQUEST {
            return Ok(false);
        }
        let result = if self.callback_from_inactive_call(envelope) {
            FsWriteResult::error("E_CANCELLED", "tool request is no longer active")
        } else {
            match &self.fs_write {
                Some(service) => {
                    let approval_key = envelope.payload["tool_request_id"]
                        .as_str()
                        .and_then(|id| self.active_calls.get(id))
                        .filter(|call| {
                            envelope.call_id.as_deref() == Some(call.call_id.as_str())
                                && envelope.payload["call_id"].as_str()
                                    == Some(call.call_id.as_str())
                        })
                        .and_then(|call| call.approval_key.as_deref());
                    service.handle_tool_payload(&envelope.payload, approval_key)
                }
                None => FsWriteResult::error(
                    "E_FS_CAPABILITY",
                    "fs.write.request received but fs.write was not negotiated in runtime.hello",
                ),
            }
        };
        let payload = serde_json::to_value(&result)
            .map_err(|err| HostError::Protocol(format!("serialize fs.write.result: {err}")))?;
        self.write(&envelope.reply(KIND_FS_WRITE_RESULT, payload))?;
        Ok(true)
    }

    /// 读循环的 v2 公共服务点（spec §13.4/§13.9）：
    /// - `host.service.request` → `HostServiceRouter::handle_request` → `host.service.result`；
    /// - `host.service.cancel` → router 取消标记（幂等，无答复）；
    /// - `capabilities.list` → 白名单方法表（仅 v2 启用时）。
    ///
    /// 返回 `true` 表示该信封已被消费。同步 dispatch 会占用当前读循环（本轮按
    /// spec §13.4 的"服务必须做小做快"取舍；未来可移到工作线程，`acquire_slot`
    /// 已为背压预留）。
    pub(super) fn maybe_serve_host_service(
        &mut self,
        envelope: &Envelope,
    ) -> Result<bool, HostError> {
        match envelope.kind.as_str() {
            KIND_HOST_SERVICE_REQUEST => {
                let reply = if self.callback_from_inactive_call(envelope) {
                    let mut reply = envelope.reply(KIND_HOST_SERVICE_RESULT, json!({}));
                    reply.error = Some(RpcError {
                        code: "E_CANCELLED".to_owned(),
                        message: "tool request is no longer active".to_owned(),
                        retryable: false,
                    });
                    reply
                } else if envelope.service_key.as_deref() == Some(TOOL_PROGRAM_V1) {
                    self.handle_program_callback(envelope)
                } else if let Some(message) = self.service_callback_denial(envelope) {
                    let mut reply = envelope.reply(KIND_HOST_SERVICE_RESULT, json!({}));
                    reply.error = Some(RpcError {
                        code: "E_SERVICE_DENIED".to_owned(),
                        message: message.to_owned(),
                        retryable: false,
                    });
                    reply
                } else if self.host_services_active {
                    let router = self.host_services.as_ref().ok_or_else(|| {
                        HostError::Protocol("active host services without router".to_owned())
                    })?;
                    let mut router = router.lock().map_err(|_| {
                        HostError::Protocol("host service router poisoned".to_owned())
                    })?;
                    router.handle_request(envelope)
                } else {
                    // v1 回退 / sidecar 未协商也发了 v2 kind：结构化拒绝，不 panic。
                    let mut reply = envelope.reply(KIND_HOST_SERVICE_RESULT, json!({}));
                    reply.error = Some(RpcError {
                        code: "E_PROTOCOL".to_owned(),
                        message:
                            "host.service.request received but service.proxy was not negotiated"
                                .to_owned(),
                        retryable: false,
                    });
                    reply
                };
                self.write(&reply)?;
                Ok(true)
            }
            KIND_HOST_SERVICE_CANCEL => {
                if self.host_services_active {
                    if let Some(router) = &self.host_services {
                        if let Ok(mut router) = router.lock() {
                            router.cancel(&envelope.request_id);
                        }
                    }
                }
                // 取消是通知语义：恰好一次答复不适用（spec §8/§13.6）。
                Ok(true)
            }
            "capabilities.list" => {
                let reply = if self.host_services_active {
                    let payload = self.host_services_payload().unwrap_or_else(|| json!([]));
                    envelope.reply(
                        "capabilities.list",
                        json!({
                            "protocol": self.negotiated_protocol,
                            "host_services": payload,
                        }),
                    )
                } else {
                    let mut reply = envelope.reply("capabilities.list", json!({}));
                    reply.error = Some(RpcError {
                        code: "E_VERSION".to_owned(),
                        message: format!(
                            "host services are not negotiated (protocol {})",
                            self.negotiated_protocol
                        ),
                        retryable: false,
                    });
                    reply
                };
                self.write(&reply)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
