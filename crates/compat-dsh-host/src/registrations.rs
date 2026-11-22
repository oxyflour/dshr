use super::*;

impl SidecarHost {
    pub fn registrations(&self) -> &[Registration] {
        &self.registrations
    }

    pub fn supports_agent_tool_context(&self) -> bool {
        self.agent_tool_context_supported
    }

    pub fn prepare_agent_step(&mut self, session: &str, data: Value) -> Result<Value, HostError> {
        if !self.agent_tool_context_supported || !self.active_calls.is_empty() {
            return Err(HostError::Protocol(
                "agent step context is unavailable".to_owned(),
            ));
        }
        self.seq += 1;
        let request_id = format!("req-agent-step-{}", self.seq);
        let envelope = Envelope::host("agent.step", &request_id, data)
            .with_version(self.negotiated_protocol)
            .with_generation(self.generation.clone())
            .with_session(session);
        self.write(&envelope)?;
        self.preparing_step = Some(session.to_owned());
        let result = self.wait_for("agent.step", Some(&request_id), Duration::from_secs(8));
        self.preparing_step = None;
        let result = result?;
        if result.runtime_generation != self.generation
            || result.session_id.as_deref() != Some(session)
        {
            return Err(HostError::Protocol(
                "agent step response has the wrong session or generation".to_owned(),
            ));
        }
        match result.error {
            Some(error) => Err(HostError::Protocol(format!(
                "{}: {}",
                error.code, error.message
            ))),
            None => Ok(result.payload),
        }
    }

    /// 把 sidecar 注册映射成 Registry 可登记的 `ToolDefinition`。
    ///
    /// 这是 slice 2 的关键：sidecar 工具与原生工具进入**同一个**注册表，
    /// 由同一个 `authorize` 闸门放行。
    pub fn tool_definitions(&self) -> Vec<dshr_tool_runtime::ToolDefinition> {
        self.tool_definitions_at_scope(None)
    }

    pub fn tool_definitions_at_scope(
        &self,
        session_id: Option<&str>,
    ) -> Vec<dshr_tool_runtime::ToolDefinition> {
        self.registrations
            .iter()
            .filter(|registration| {
                registration.preset_revision.is_none()
                    && registration.session_id.as_deref() == session_id
            })
            .map(|registration| registration.to_tool_definition(&self.generation))
            .collect()
    }

    pub fn preset_tool_definitions(&self, session: &str) -> Vec<dshr_tool_runtime::ToolDefinition> {
        let Some((_, revision)) = self.preset_bindings.get(session) else {
            return Vec::new();
        };
        self.registrations
            .iter()
            .filter(|registration| registration.preset_revision.as_ref() == Some(revision))
            .map(|registration| registration.to_tool_definition(&self.generation))
            .collect()
    }

    pub fn supports_agent_presets(&self) -> bool {
        self.agent_presets_supported
    }

    pub fn sync_presets(&mut self, definitions: &Value) -> Result<&Value, HostError> {
        if self.preset_definitions.as_ref() != Some(definitions) {
            let response =
                self.preset_request("preset.sync", None, json!({ "definitions": definitions }))?;
            self.preset_status = response["presets"].clone();
            self.preset_definitions = Some(definitions.clone());
        }
        Ok(&self.preset_status)
    }

    pub fn preset_status(&self) -> &Value {
        &self.preset_status
    }

    pub fn bound_preset(&self, session_id: &str) -> Option<&str> {
        self.preset_bindings
            .get(session_id)
            .map(|(id, _)| id.as_str())
    }

    pub fn release_preset(&mut self, session_id: &str) -> Result<(), HostError> {
        if self.preset_bindings.contains_key(session_id) {
            self.preset_request("preset.release", Some(session_id), json!({}))?;
            self.preset_bindings.remove(session_id);
        }
        Ok(())
    }

    pub fn bind_preset(
        &mut self,
        session: &str,
        preset: &str,
        parent: Option<&str>,
    ) -> Result<(), HostError> {
        if self.retired_sessions.contains(session) {
            return Err(HostError::Protocol(
                "session no longer accepts a preset binding".to_owned(),
            ));
        }
        if let Some((bound, _)) = self.preset_bindings.get(session) {
            return if bound == preset {
                Ok(())
            } else {
                Err(HostError::Protocol(format!(
                    "session already uses preset {bound}"
                )))
            };
        }
        let response = self.preset_request(
            "preset.bind",
            Some(session),
            json!({ "preset": preset, "parent_session_id": parent }),
        )?;
        let revision = response["revision"]
            .as_str()
            .ok_or_else(|| HostError::Protocol("missing preset revision".to_owned()))?;
        self.preset_bindings
            .insert(session.to_owned(), (preset.to_owned(), revision.to_owned()));
        Ok(())
    }

    pub(super) fn preset_request(
        &mut self,
        kind: &str,
        session: Option<&str>,
        data: Value,
    ) -> Result<Value, HostError> {
        if !self.agent_presets_supported {
            return Err(HostError::Protocol(
                "sidecar does not support agent.presets".to_owned(),
            ));
        }
        self.seq += 1;
        let request = format!("req-preset-{}", self.seq);
        let mut envelope = Envelope::host(kind, &request, data)
            .with_version(self.negotiated_protocol)
            .with_generation(self.generation.clone());
        envelope.session_id = session.map(str::to_owned);
        self.write(&envelope)?;
        let response = self.wait_for(kind, Some(&request), Duration::from_secs(10))?;
        match response.error {
            Some(error) => Err(HostError::Protocol(format!(
                "{}: {}",
                error.code, error.message
            ))),
            None => Ok(response.payload),
        }
    }

    /// Consume lifecycle messages that arrived while no tool call was in flight.
    pub fn refresh_registrations(&mut self) -> Result<(), HostError> {
        if !self.active_calls.is_empty() {
            return Ok(());
        }
        loop {
            match self.rx.try_recv() {
                Ok(envelope) => self.process_background_message(&envelope)?,
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.registrations.clear();
                    return Err(HostError::Closed);
                }
            }
        }
    }

    pub fn retire_session(&mut self, session_id: &str) -> Result<(), HostError> {
        self.retired_sessions.insert(session_id.to_owned());
        let release = if self.preset_bindings.remove(session_id).is_some() {
            self.preset_request("preset.release", Some(session_id), json!({}))
                .map(|_| ())
        } else {
            Ok(())
        };
        let mut revocations = Vec::new();
        self.registrations.retain(|registration| {
            if registration.session_id.as_deref() != Some(session_id) {
                return true;
            }
            revocations.push((
                registration.registration_id.clone(),
                registration.plugin_id.clone(),
            ));
            false
        });
        for (id, plugin) in revocations {
            self.seq += 1;
            let mut envelope =
                Envelope::host("tool.unregister", format!("retire-{}", self.seq), json!({}))
                    .with_version(self.negotiated_protocol)
                    .with_generation(self.generation.clone())
                    .with_plugin(plugin)
                    .with_session(session_id);
            envelope.registration_id = Some(id);
            self.write(&envelope)?;
        }
        release
    }

    pub(super) fn collect_registrations(&mut self, window: Duration) -> Result<(), HostError> {
        let deadline = Instant::now() + window;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            match self.rx.recv_timeout(remaining) {
                Ok(envelope) => self.process_background_message(&envelope)?,
                Err(RecvTimeoutError::Timeout) => return Ok(()),
                Err(RecvTimeoutError::Disconnected) => return Err(HostError::Closed),
            }
        }
    }

    pub(super) fn process_tool_lifecycle(
        &mut self,
        envelope: &Envelope,
    ) -> Result<bool, HostError> {
        if !matches!(envelope.kind.as_str(), "tool.register" | "tool.unregister") {
            return Ok(false);
        }
        if envelope.runtime_generation != self.generation {
            return Ok(true);
        }
        let owner = envelope.plugin_id.as_deref().unwrap_or_default();
        let scope = &envelope.session_id;
        let revision = envelope.payload["preset_revision"].as_str();
        if envelope.kind == "tool.unregister" {
            self.registrations.retain(|registration| {
                let identity_matches = match &envelope.registration_id {
                    Some(id) => id == &registration.registration_id,
                    None => {
                        envelope.payload["register_request_id"].as_str()
                            == Some(registration.register_request_id.as_str())
                    }
                };
                !(identity_matches
                    && owner == registration.plugin_id
                    && scope == &registration.session_id
                    && revision == registration.preset_revision.as_deref())
            });
            return Ok(true);
        }

        self.seq += 1;
        let id = format!("reg-{}", self.seq);
        let mut registration = parse_tool_registration(&envelope.payload, owner, &id);
        registration.session_id = scope.clone();
        registration.preset_revision = revision.map(str::to_owned);
        registration.register_request_id = envelope.request_id.clone();
        let mut ack = envelope.reply("tool.register", json!({ "name": registration.name }));
        let error = if scope.is_some() && revision.is_some() {
            Some((
                "E_SCHEMA",
                "a registration cannot have both session and preset scope",
            ))
        } else if scope
            .as_ref()
            .is_some_and(|id| self.retired_sessions.contains(id))
        {
            Some(("E_TOOL_UNAVAILABLE", "tool registration session has ended"))
        } else if registration.name.is_empty() {
            Some(("E_SCHEMA", "tool name is required"))
        } else if self.registrations.iter().any(|item| {
            item.session_id == registration.session_id
                && item.preset_revision == registration.preset_revision
                && item.name == registration.name
        }) {
            Some((
                "E_DUPLICATE_REG",
                "tool name already registered in this scope",
            ))
        } else {
            None
        };
        if let Some((code, message)) = error {
            ack.error = Some(RpcError {
                code: code.to_owned(),
                message: message.to_owned(),
                retryable: false,
            });
            self.write(&ack)?;
        } else {
            ack.registration_id = Some(id.clone());
            ack.payload["registration_id"] = json!(id);
            self.write(&ack)?;
            self.registrations.push(registration);
        }
        Ok(true)
    }
}
