use dshr_agent_core::{
    spawn_child_spec, SessionTreeRuntime, SpawnChildArgs, SpawnContext, StepContext,
    ToolCallRequest, ToolExecutor, ToolOutcome, ToolSpec, DEFAULT_MAX_DEPTH,
};
use dshr_compat_dsh_host::{Envelope, OpenToolTurn, ProgramToolScope, SessionAccess, SidecarHost};
use dshr_plugin_host::PluginHost;
use dshr_session_store::{try_lock_session, EventSpec, SessionCreated, SessionIndex, SessionLog};
use dshr_tool_runtime::{
    ApprovalBroker, ApprovalOutcome, ApprovalPolicy, ApprovalResolution, LiveApproval,
    PermissionMode, PolicyDecision, Presentation, RegistryError, ToolDefinition, ToolOrigin,
    ToolPolicy,
};
use serde_json::json;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::session_access::ChatSessionAccess;
use crate::{
    creator, dsh_subagents, hex_lower, now_ts, register_sidecar_tools, runtime_components,
    session_workspace, workspace_root, ChatRuntime, ChatState, DesktopSettings, ParsedAgentPresets,
};

#[derive(Clone)]
pub(crate) struct LivePermission(Arc<Mutex<Option<PermissionMode>>>);

impl LivePermission {
    pub(crate) fn new(mode: Option<PermissionMode>) -> Self {
        Self(Arc::new(Mutex::new(mode)))
    }

    pub(crate) fn mode(&self) -> Option<PermissionMode> {
        *self.0.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn set(&self, mode: PermissionMode) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(mode);
    }

    pub(crate) fn can_write(&self) -> bool {
        self.mode() != Some(PermissionMode::ReadOnly)
    }

    pub(crate) fn file_write_policy(&self) -> PolicyDecision {
        match self.mode() {
            Some(PermissionMode::DangerFullAccess) => PolicyDecision::Allow,
            Some(PermissionMode::ReadOnly) => {
                PolicyDecision::Deny("file write denied in read-only mode".to_owned())
            }
            Some(PermissionMode::WorkspaceWrite) | None => PolicyDecision::Ask,
        }
    }
}

impl ToolPolicy for LivePermission {
    fn check(&self, tool: &ToolDefinition, arguments: &serde_json::Value) -> PolicyDecision {
        self.mode()
            .map_or_else(ApprovalPolicy::ask_all, ApprovalPolicy::for_mode)
            .check(tool, arguments)
    }
}

pub(crate) fn tool_approval_key(
    session: &str,
    workspace: &Path,
    tool: &ToolDefinition,
) -> Option<String> {
    if tool.card() == Presentation::Terminal
        || matches!(
            tool.name.as_str(),
            "pwsh"
                | "powershell"
                | "bash"
                | "shell"
                | "exec"
                | "terminal_open"
                | "terminal_send"
                | "terminal_signal"
        )
    {
        return None;
    }
    let owner = match &tool.origin {
        ToolOrigin::Native { plugin_id } | ToolOrigin::Sidecar { plugin_id, .. } => plugin_id,
        ToolOrigin::Mcp { .. } => return None,
    };
    if owner.is_empty() {
        return None;
    }
    let scope = json!([
        session,
        workspace.to_string_lossy(),
        tool.origin.kind(),
        owner,
        tool.name,
        tool.input_schema,
        tool.presentation,
    ]);
    Some(hex_lower(
        ring::digest::digest(&ring::digest::SHA256, scope.to_string().as_bytes()).as_ref(),
    ))
}

/// sidecar 工具调用超时：按工具声明（`ToolDefinition.timeout_ms`，对齐 DSH 的 `timeoutMs`）
/// 而不是写死 5s。`DSHR_TOOL_TIMEOUT_MS` 可整体覆盖（诊断/慢机器），统一 clamp。
pub(crate) fn sidecar_tool_timeout(definition: &ToolDefinition) -> Duration {
    let override_ms = std::env::var("DSHR_TOOL_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    clamp_tool_timeout_ms(override_ms.unwrap_or(definition.timeout_ms))
}

/// 超时夹到 1s..=600s：太小会误伤正常工具，太大（或 0）会让界面像卡死。
pub(crate) fn clamp_tool_timeout_ms(millis: u64) -> Duration {
    Duration::from_millis(millis.clamp(1_000, 600_000))
}

/// Agent 的工具执行器：authorize 之后按来源分发给 PluginHost（native）或 Sidecar。
pub(crate) struct ChatExecutor<'a> {
    pub(crate) plugins: &'a mut PluginHost,
    pub(crate) host: &'a mut Option<SidecarHost>,
    pub(crate) approvals: &'a Arc<ApprovalBroker>,
    pub(crate) creator: Option<creator::Session<'a>>,
    pub(crate) step: ToolStep,
    pub(crate) program_hidden: HashSet<String>,
    pub(crate) headless: bool,
    pub(crate) diagnostics: bool,
    /// 父会话日志（与流式回调共享同一个 RefCell：`spawn_child` 要向它追加
    /// child.spawned/finished，回调同时在追加 tool.call/tool.result）。
    pub(crate) log: std::rc::Rc<std::cell::RefCell<SessionLog>>,
    /// 会话目录（子会话文件落在这里）。
    pub(crate) sessions_dir: &'a std::path::Path,
    /// 父会话 id。
    pub(crate) parent_id: &'a str,
    pub(crate) workspace: &'a Path,
    /// 父会话取消句柄：子轮次共享，用户停止会同时停掉子会话。
    pub(crate) cancel: &'a Arc<AtomicBool>,
    pub(crate) settings: &'a DesktopSettings,
    pub(crate) permission: &'a LivePermission,
    pub(crate) preset_inputs: &'a Option<(PathBuf, Vec<PathBuf>)>,
    pub(crate) published_presets: &'a mut Option<ParsedAgentPresets>,
    /// 是否允许派生子会话（子会话本身不再派发，默认单层）。
    pub(crate) allow_spawn: bool,
    /// L2 Host Service 的会话读写面（None = 该轮不接 sidecar 服务）。
    pub(crate) l2_access: Option<Arc<ChatSessionAccess>>,
}

#[derive(Clone, Copy)]
enum ToolLocation {
    Global,
    Trial,
    Creator,
    SpawnChild,
}

#[derive(Clone)]
pub(crate) struct ResolvedTool {
    pub(crate) definition: ToolDefinition,
    location: ToolLocation,
    pub(super) views: Vec<super::adc_tool_views::ToolViewBinding>,
}

pub(crate) type ToolStep = std::rc::Rc<RefCell<std::collections::BTreeMap<String, ResolvedTool>>>;

impl ToolExecutor for ChatExecutor<'_> {
    fn specs(&self) -> Vec<ToolSpec> {
        let background_child = self.is_background_child();
        let tools = self.step.borrow();
        tools
            .values()
            .map(|tool| ToolSpec {
                name: tool.definition.name.clone(),
                description: match (background_child, tool.definition.name.as_str()) {
                    (true, "working_directory") => format!(
                        "{} Use this tool with an empty object {{}} to read your current session directory.",
                        tool.definition.description
                    ),
                    (true, "pwsh" | "bash") if tools.contains_key("working_directory") => format!(
                        "{} Use working_directory to read the current session directory. Report commands that require approval to the parent session.",
                        tool.definition.description
                    ),
                    _ => tool.definition.description.clone(),
                },
                parameters: tool.definition.input_schema.clone(),
            })
            .collect()
    }

    fn prepare_step(&mut self) -> Vec<ToolSpec> {
        self.program_hidden.clear();
        if let Some(host) = self.host.as_mut() {
            if let Err(error) = host.refresh_registrations() {
                eprintln!("[chat] sidecar tool registry: {error}");
            }
            self.plugins.invalidate_generation(host.generation());
            register_sidecar_tools(self.plugins, host, self.headless);
        }

        let mut tools = self.step.borrow_mut();
        tools.clear();
        let workspace_dependencies_ready = runtime_components::workspace_dependencies_ready();
        let mut insert = |definition: ToolDefinition, location| {
            if !self.diagnostics && definition.name.starts_with("probe_") {
                return;
            }
            if !workspace_dependencies_ready
                && definition.name == runtime_components::WORKSPACE_DEPS_TOOL
                && matches!(&definition.origin, ToolOrigin::Sidecar { plugin_id, .. } if plugin_id == "dsh-official")
            {
                return;
            }
            let mut views = Vec::new();
            if !self.headless && matches!(definition.origin, ToolOrigin::Native { .. }) {
                let trial = matches!(location, ToolLocation::Trial);
                let host = match location {
                    ToolLocation::Global => Some(&*self.plugins),
                    ToolLocation::Trial => {
                        self.creator.as_ref().map(|creator| &creator.scope.plugins)
                    }
                    _ => None,
                };
                if let Some(host) = host {
                    views =
                        super::adc_tool_views::call_view_bindings(host, &definition.name, trial);
                }
            }
            tools.insert(
                definition.name.clone(),
                ResolvedTool {
                    definition,
                    location,
                    views,
                },
            );
        };
        for definition in self.plugins.registry().list() {
            insert(definition.clone(), ToolLocation::Global);
        }
        if !self.headless {
            if let Some(host) = self.host.as_ref() {
                for definition in host.preset_tool_definitions(self.parent_id) {
                    insert(definition, ToolLocation::Global);
                }
                for definition in host.tool_definitions_at_scope(Some(self.parent_id)) {
                    insert(definition, ToolLocation::Global);
                }
            }
        }
        if let Some(creator) = &self.creator {
            for definition in creator.scope.plugins.registry().list() {
                insert(definition.clone(), ToolLocation::Trial);
            }
        }
        // Preserve the core entry points if a package declares the same name.
        let mut add_builtin = |spec: ToolSpec, location| {
            insert(
                ToolDefinition::new(
                    spec.name,
                    spec.description,
                    ToolOrigin::Native {
                        plugin_id: "core".to_owned(),
                    },
                )
                .with_schema(spec.parameters),
                location,
            );
        };
        if self.creator.is_some() {
            for spec in creator::specs() {
                add_builtin(spec, ToolLocation::Creator);
            }
        }
        if self.allow_spawn {
            add_builtin(spawn_child_spec(), ToolLocation::SpawnChild);
        }
        drop(tools);
        self.specs()
    }

    fn prepare_step_context(&mut self, step: u32, user_text: Option<&str>) -> StepContext {
        if !self
            .host
            .as_ref()
            .is_some_and(SidecarHost::supports_agent_tool_context)
        {
            return StepContext::default();
        }
        let registrations: std::collections::BTreeMap<_, _> = self
            .step
            .borrow()
            .iter()
            .filter_map(|(name, tool)| match &tool.definition.origin {
                ToolOrigin::Sidecar {
                    plugin_id,
                    registration_id,
                    ..
                } if plugin_id == "dsh-official" => Some((name.clone(), registration_id.clone())),
                _ => None,
            })
            .collect();
        let contextual_tools: Vec<_> = registrations
            .keys()
            .filter(|name| {
                matches!(
                    name.as_str(),
                    "skill"
                        | "web_search"
                        | "web_fetch"
                        | "office_render"
                        | "office_convert"
                        | "office_recalculate"
                        | "job_list"
                        | "job_output"
                        | "job_kill"
                        | "terminal_open"
                        | "terminal_send"
                        | "terminal_read"
                        | "terminal_signal"
                        | "terminal_list"
                        | "terminal_close"
                        | "get_goal"
                        | "create_goal"
                        | "update_goal"
                        | "session_search"
                        | "session_event_search"
                        | "session_trace"
                        | "session_event_trace"
                        | "session_event_read"
                        | "subagent"
                        | "list_agents"
                        | "send_message"
                        | "wait_agent"
                        | "interrupt_agent"
                        | "team_task_create"
                        | "team_task_list"
                        | "team_task_get"
                        | "team_task_update"
                        | "workflow"
                        | "ralph"
                        | "schedule_create"
                        | "schedule_view"
                        | "schedule_list"
                        | "schedule_edit"
                        | "cordis_inspect_list"
                        | "cordis_inspect_query"
                        | "run_code"
                )
            })
            .cloned()
            .collect();
        if contextual_tools.is_empty() {
            return StepContext::default();
        }
        self.publish_l2_snapshot();
        let model = &self.settings.model;
        let turn = open_tool_turn(&self.log.borrow()).map(|turn| {
            json!({
                "number": turn.number, "start_seq": turn.start_seq,
            })
        });
        let reply = self
            .host
            .as_mut()
            .expect("checked sidecar capabilities")
            .prepare_agent_step(
                self.parent_id,
                json!({
                    "step": step,
                    "model": {
                        "provider": model.provider_name(), "model": model.name(),
                        "image_input": model.supports_attachments(),
                    },
                    "registrations": registrations,
                    "turn": turn,
                    "user_text": user_text,
                }),
            );
        self.drain_l2_appends();
        if let Ok(reply) = &reply {
            if registrations.contains_key("run_code") {
                self.program_hidden = reply["deferred_tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|name| *name != "run_code" && registrations.contains_key(*name))
                    .map(str::to_owned)
                    .collect();
            }
        }
        let mut context = match reply {
            Ok(reply) => StepContext {
                unavailable_tools: serde_json::from_value(reply["unavailable_tools"].clone())
                    .unwrap_or_default(),
                system_sections: serde_json::from_value(reply["system_sections"].clone())
                    .unwrap_or_default(),
                turn_instructions: serde_json::from_value(reply["turn_instructions"].clone())
                    .unwrap_or_default(),
            },
            Err(error) => {
                eprintln!("[chat] sidecar agent step: {error}");
                StepContext {
                    unavailable_tools: contextual_tools,
                    ..Default::default()
                }
            }
        };
        context
            .unavailable_tools
            .retain(|name| registrations.contains_key(name));
        self.step
            .borrow_mut()
            .retain(|name, _| !context.unavailable_tools.contains(name));
        context
            .unavailable_tools
            .extend(self.program_hidden.iter().cloned());
        context
    }

    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        self.execute_batch(std::slice::from_ref(call), 1).remove(0)
    }

    fn tool_images(&self, outcome: &ToolOutcome) -> Vec<dshr_agent_core::MediaAttachment> {
        let Some(access) = &self.l2_access else {
            return Vec::new();
        };
        dshr_agent_context::spill::restore_tool_images(
            &access.spill_store(),
            &self.log.borrow(),
            &outcome.content,
        )
    }

    fn execute_batch(
        &mut self,
        calls: &[ToolCallRequest],
        max_parallel: usize,
    ) -> Vec<ToolOutcome> {
        let mut outcomes = vec![ToolOutcome::failure("batch outcome missing"); calls.len()];
        let mut sidecar_calls = Vec::new();
        let mut sidecar_indices = Vec::new();
        let mut native_calls = [Vec::new(), Vec::new()];
        let mut native_indices = [Vec::new(), Vec::new()];
        let mut replaced_trials = std::collections::BTreeSet::new();
        let turn = open_tool_turn(&self.log.borrow());

        for (index, call) in calls.iter().enumerate() {
            if self.program_hidden.contains(&call.tool) {
                outcomes[index] = ToolOutcome::failure(format!(
                    "tool {} requires run_code in this model step",
                    call.tool
                ));
                continue;
            }
            let tool = self.step.borrow().get(&call.tool).cloned();
            let Some(ResolvedTool {
                definition,
                location,
                ..
            }) = tool
            else {
                outcomes[index] = ToolOutcome::failure(format!(
                    "tool {} was not offered for this model step",
                    call.tool
                ));
                continue;
            };
            match location {
                ToolLocation::Creator => {
                    let names: Vec<_> = self
                        .step
                        .borrow()
                        .iter()
                        .filter(|&(_name, tool)| {
                            matches!(tool.location, ToolLocation::Global | ToolLocation::Trial)
                        })
                        .map(|(name, _tool)| name.clone())
                        .collect();
                    let creator = self.creator.as_mut().expect("creator step");
                    creator.read_only = !self.permission.can_write();
                    outcomes[index] = creator.execute(call, &names, self.approvals, self.cancel);
                    if call.tool == creator::TRY_PLUGIN_TOOL
                        && outcomes[index].value["replaced"] == true
                    {
                        if let Some(id) = outcomes[index].value["plugin"].as_str() {
                            replaced_trials.insert(id.to_owned());
                        }
                    }
                    continue;
                }
                ToolLocation::SpawnChild => {
                    outcomes[index] = self.execute_spawn_child(call);
                    continue;
                }
                ToolLocation::Global | ToolLocation::Trial => {}
            }
            let grant_key = if !self.headless
                && matches!(location, ToolLocation::Global)
                && self.permission.mode().is_some()
            {
                tool_approval_key(self.parent_id, self.workspace, &definition)
            } else {
                None
            };
            if let Err(outcome) = self.authorize_call(call, &definition, grant_key.as_deref()) {
                outcomes[index] = outcome;
                continue;
            }
            match &definition.origin {
                ToolOrigin::Sidecar { .. } => {
                    sidecar_indices.push(index);
                    sidecar_calls.push(
                        dshr_compat_dsh_host::BatchInvokeCall::new(
                            call.call_id.clone(),
                            call.tool.clone(),
                            call.arguments.clone(),
                            sidecar_tool_timeout(&definition),
                        )
                        .with_session(self.parent_id)
                        .with_turn(turn)
                        .with_model_route(
                            self.settings.model.provider_name(),
                            self.settings.model.name(),
                        )
                        .with_binding(definition.origin)
                        .with_approval_key(grant_key),
                    );
                }
                ToolOrigin::Native { .. } => {
                    let scope = usize::from(matches!(location, ToolLocation::Trial));
                    native_indices[scope].push(index);
                    native_calls[scope].push((call.tool.clone(), call.arguments.clone()));
                }
                ToolOrigin::Mcp { .. } => {
                    outcomes[index] = ToolOutcome::failure("mcp not wired");
                }
            }
        }

        if !sidecar_calls.is_empty() {
            self.publish_l2_snapshot();
            let program = calls
                .iter()
                .any(|call| call.tool == "run_code")
                .then(|| self.program_scope())
                .flatten();
            if let Some(host) = self.host.as_mut() {
                host.set_program_tools(program);
                let results = host.invoke_batch(&sidecar_calls, max_parallel, Some(self.cancel));
                host.set_program_tools(None);
                self.drain_l2_appends();
                for (index, result) in sidecar_indices.into_iter().zip(results) {
                    outcomes[index] = match result {
                        Ok(result) if result.terminal.kind == "tool.result" => {
                            outcome_from_terminal(&result.terminal)
                        }
                        Ok(result) => ToolOutcome::failure(result.terminal.payload.to_string()),
                        Err(error) => ToolOutcome::failure(error.to_string()),
                    };
                }
            } else {
                for index in sidecar_indices {
                    outcomes[index] = ToolOutcome::failure("sidecar unavailable");
                }
            }
        }

        for (scope, (indices, calls)) in native_indices.into_iter().zip(native_calls).enumerate() {
            let (indices, calls): (Vec<_>, Vec<_>) = indices.into_iter().zip(calls)
                .filter(|(index, (name, _))| {
                    let from_replaced_trial = scope == 1 && self.step.borrow().get(name)
                        .is_some_and(|tool| matches!(&tool.definition.origin,
                            ToolOrigin::Native { plugin_id } if replaced_trials.contains(plugin_id)));
                    if from_replaced_trial {
                        outcomes[*index] = ToolOutcome::failure(
                            "This trial was replaced during the current step. Call the newly offered tool on the next step.",
                        );
                    }
                    !from_replaced_trial
                }).unzip();
            if calls.is_empty() {
                continue;
            }
            let plugins = if scope == 0 {
                &mut *self.plugins
            } else {
                &mut self.creator.as_mut().expect("trial step").scope.plugins
            };
            let results = plugins.call_approved_batch_cancellable(
                &calls,
                max_parallel.max(1),
                Some(self.cancel),
            );
            for ((index, (tool, _)), result) in indices.into_iter().zip(calls).zip(results) {
                outcomes[index] = match result {
                    Ok(value) => super::adc_tool_views::native_tool_outcome(plugins, &tool, value),
                    Err(error) => ToolOutcome::failure(error),
                };
            }
        }
        outcomes
    }
}

fn open_tool_turn(log: &SessionLog) -> Option<OpenToolTurn> {
    let event = log
        .events()
        .iter()
        .rev()
        .find(|event| matches!(event.event_type.as_str(), "turn.started" | "turn.ended"))?;
    (event.event_type == "turn.started").then(|| OpenToolTurn {
        number: log
            .events()
            .iter()
            .filter(|event| event.event_type == "turn.started")
            .count() as u64,
        start_seq: event.seq,
    })
}

fn approval_denial_message(
    approval: ApprovalResolution,
    background_child: bool,
    tool: &str,
) -> String {
    match approval.answerer {
        "headless" if background_child => format!("Tool `{tool}` requires approval. Interactive approval is unavailable in this background subagent. Report this operation to the parent session."),
        "headless" => format!("Tool `{tool}` requires approval. Interactive approval is unavailable in this run."),
        "cancelled" => format!("Approval for tool `{tool}` was cancelled."),
        "timeout" => format!("Approval for tool `{tool}` timed out."),
        "auto" => format!("Tool `{tool}` was denied by the configured approval decision."),
        "permission_mode" => format!("Tool `{tool}` was denied by the current permission mode."),
        _ => format!("user denied tool `{tool}`"),
    }
}

impl ChatExecutor<'_> {
    fn is_background_child(&self) -> bool {
        !self.approvals.is_interactive()
            && self
                .log
                .borrow()
                .created()
                .is_some_and(SessionCreated::is_child)
    }

    fn program_scope(&self) -> Option<ProgramToolScope> {
        let access = self.l2_access.clone()?;
        let session_id = self.parent_id.to_owned();
        let owner = session_id.clone();
        let workspace = self.workspace.to_path_buf();
        let cancel = self.cancel.clone();
        let permission = self.permission.clone();
        let approvals = self.approvals.clone();
        let background_child = self.is_background_child();
        let headless = self.headless;
        let tools = self
            .step
            .borrow()
            .iter()
            .filter(|&(name, tool)| {
                self.program_hidden.contains(name) && matches!(tool.location, ToolLocation::Global)
            })
            .map(|(name, tool)| {
                (
                    name.clone(),
                    (
                        tool.definition.clone(),
                        sidecar_tool_timeout(&tool.definition),
                    ),
                )
            })
            .collect();
        let audit_owner = owner.clone();
        Some(ProgramToolScope {
            session_id,
            tools,
            authorize: Arc::new(move |definition, call_id, arguments, remaining| {
                if cancel.load(Ordering::Acquire) {
                    return Err("program was cancelled".to_owned());
                }
                let key = if !headless && permission.mode().is_some() {
                    tool_approval_key(&owner, &workspace, definition)
                } else {
                    None
                };
                let decide = || permission.check(definition, arguments);
                match decide() {
                    PolicyDecision::Allow => return Ok(key),
                    PolicyDecision::Deny(message) => return Err(message),
                    PolicyDecision::Ask => {}
                }
                let approval = approvals.request_with_policy_resolution(
                    call_id,
                    &definition.name,
                    arguments,
                    remaining,
                    &cancel,
                    LiveApproval {
                        decide: &decide,
                        grant_key: key.as_deref(),
                    },
                );
                if let PolicyDecision::Deny(message) = decide() {
                    return Err(message);
                }
                if cancel.load(Ordering::Acquire) {
                    return Err("program was cancelled".to_owned());
                }
                match approval.outcome {
                    ApprovalOutcome::AllowOnce | ApprovalOutcome::AllowAlways => Ok(key),
                    ApprovalOutcome::Deny => Err(approval_denial_message(
                        approval,
                        background_child,
                        &definition.name,
                    )),
                }
            }),
            record_admission: Arc::new(move |event| {
                access
                    .append_event(&audit_owner, "tool.program.admitted", event)
                    .map(|_| ())
                    .map_err(|error| error.message)
            }),
        })
    }

    fn execute_spawn_child(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        let log = self.log.clone();
        let outcome = TreeCoordinator::spawn_child(&mut log.borrow_mut(), call, self);
        outcome
    }

    #[allow(clippy::result_large_err)]
    fn authorize_call(
        &self,
        call: &ToolCallRequest,
        definition: &ToolDefinition,
        grant_key: Option<&str>,
    ) -> Result<(), ToolOutcome> {
        let registry = self.plugins.registry();
        match registry.authorize_definition(definition, &call.arguments) {
            Ok(_) => Ok(()),
            Err(RegistryError::NeedsApproval { tool }) => {
                let policy = || self.permission.check(definition, &call.arguments);
                let approval = self.approvals.request_with_policy_resolution(
                    &call.call_id,
                    &tool,
                    &call.arguments,
                    Duration::from_secs(300),
                    self.cancel.as_ref(),
                    LiveApproval {
                        decide: &policy,
                        grant_key,
                    },
                );
                if let PolicyDecision::Deny(reason) = policy() {
                    return Err(ToolOutcome::failure(reason));
                }
                if self.cancel.load(Ordering::Acquire) {
                    return Err(ToolOutcome::failure(format!(
                        "Approval for tool `{tool}` was cancelled."
                    )));
                }
                match approval.outcome {
                    ApprovalOutcome::AllowOnce | ApprovalOutcome::AllowAlways => {
                        dshr_tool_runtime::ToolRegistry::validate_definition(
                            definition,
                            &call.arguments,
                        )
                        .map_err(|error| ToolOutcome::failure(error.to_string()))
                    }
                    ApprovalOutcome::Deny => Err(ToolOutcome::failure(approval_denial_message(
                        approval,
                        self.is_background_child(),
                        &tool,
                    ))),
                }
            }
            Err(error) => Err(ToolOutcome::failure(error.to_string())),
        }
    }

    fn publish_l2_snapshot(&self) {
        if let Some(access) = &self.l2_access {
            access
                .inner
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .child_environment = Some(dsh_subagents::ChildEnvironment {
                settings: self.settings.clone(),
                permission: self.permission.clone(),
                diagnostics: self.diagnostics,
            });
            access.publish(
                &self.log.borrow(),
                self.settings.model.supports_attachments(),
                self.cancel.clone(),
            );
        }
    }

    fn drain_l2_appends(&self) {
        let Some(access) = &self.l2_access else {
            return;
        };
        let appends = access.take_appends(self.parent_id);
        if appends.is_empty() {
            return;
        }
        let ts = now_ts();
        let mut log = self.log.borrow_mut();
        for (event_type, data) in appends {
            let _ = log.append(EventSpec::durable(event_type, &ts, data));
        }
    }
}

/// sidecar `tool.result` 终态 → `ToolOutcome`（content 给模型、value/meta 给 UI/回放）。
fn outcome_from_terminal(terminal: &Envelope) -> ToolOutcome {
    let payload = &terminal.payload;
    let content = payload["content"].clone();
    if content.is_null() {
        if let Some(error) = &terminal.error {
            return ToolOutcome::failure(format!("{}: {}", error.code, error.message));
        }
    }
    // value 优先取自 payload.value；旧插件只在 content 里放值，做一次兼容回退。
    let value = payload.get("value").cloned().unwrap_or_else(|| {
        content
            .get(0)
            .and_then(|block| block.get("value"))
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    });
    ToolOutcome {
        is_error: payload["is_error"].as_bool().unwrap_or(false),
        content,
        value,
        meta: payload.get("meta").cloned(),
    }
}

/// 会话树产品面接线：`spawn_child` = 建子会话 → 同步跑一轮 → 回写父子日志。
///
/// 运行态树由"父会话**内存**日志 + 其余会话磁盘日志"重建
/// （`SessionTreeRuntime::from_logs`）：磁盘上的父日志可能还缺本轮刚追加的
/// `tool.call`，直接用磁盘版本会导致 seq 分叉。
struct TreeCoordinator;

impl TreeCoordinator {
    fn spawn_child(
        log: &mut SessionLog,
        call: &ToolCallRequest,
        executor: &mut ChatExecutor<'_>,
    ) -> ToolOutcome {
        let parent_id = executor.parent_id;
        let cancel = executor.cancel;
        let args = match SpawnChildArgs::from_call(call) {
            Ok(args) => args,
            Err(err) => return ToolOutcome::failure(format!("spawn_child 参数非法: {err}")),
        };
        let store = SessionIndex::new(executor.sessions_dir);
        let child_id = store.new_id();
        let child_path = store.path_for(&child_id);
        let _writer = match try_lock_session(&child_path) {
            Ok(writer) => writer,
            Err(error) => return ToolOutcome::failure(format!("无法取得子会话写入权：{error}")),
        };
        if child_path.exists() || child_path.with_extension("jsonl.bak").exists() {
            return ToolOutcome::failure("子会话 ID 已被使用");
        }
        let ts = now_ts();

        let mut logs: Vec<SessionLog> = Vec::new();
        for meta in store.scan().unwrap_or_default() {
            if meta.id == parent_id {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&meta.path) {
                if let Ok(other) = SessionLog::from_jsonl(&meta.id, &text) {
                    logs.push(other);
                }
            }
        }
        let mut runtime = match SessionTreeRuntime::from_logs(logs, DEFAULT_MAX_DEPTH) {
            Ok(runtime) => runtime,
            Err(err) => return ToolOutcome::failure(format!("会话树不可用: {err}")),
        };
        let workspace = log
            .created()
            .map(|created| created.workspace_id.clone())
            .unwrap_or_else(|| workspace_root().display().to_string());
        if let Err(err) = runtime.register_root_with_log(parent_id, &workspace, log.clone()) {
            return ToolOutcome::failure(format!("注册父会话失败: {err}"));
        }
        let spawned =
            match runtime.spawn_child(&child_id, parent_id, &args, &SpawnContext::at(ts.clone())) {
                Ok(spawned) => spawned,
                Err(err) => return ToolOutcome::failure(format!("派发子会话失败: {err}")),
            };

        // 子会话独立日志先落盘（含 session.created 的父链接），再跑它的第一轮。
        if let Err(err) = store.write_log(&spawned.log) {
            return ToolOutcome::failure(format!("写入子会话失败: {err}"));
        }
        let mut settings = executor.settings.clone();
        settings.permission = executor.permission.mode();
        let mut child = ChatState::from_log(
            child_path,
            spawned.log,
            session_workspace(log),
            cancel,
            executor.approvals.clone(),
            settings,
            ChatRuntime::Desktop {
                allow_spawn: false,
                sidecar: false,
                diagnostics: executor.diagnostics,
            },
        );
        child.permission = executor.permission.clone();
        child
            .plugins
            .registry_mut()
            .set_policy(child.permission.clone());
        let uses_preset = child
            .log
            .created()
            .and_then(|created| created.preset.as_deref())
            .is_some_and(|preset| preset != creator::PRESET);
        if uses_preset {
            child.host = executor.host.take();
            child.preset_inputs = executor.preset_inputs.clone();
            child.l2_access = executor.l2_access.clone();
            if args.preset.is_none() {
                child.inherited_preset_from = Some(parent_id.to_owned());
            }
        }
        let reply = child.stream_turn(&args.prompt, &mut |_| {});
        if uses_preset {
            *executor.host = child.host.take();
            // The child may have synchronized a newer catalog on this same Sidecar.
            *executor.published_presets = None;
        }
        child.save();

        let interrupted = cancel.load(Ordering::Relaxed);
        let outcome = if interrupted {
            "interrupted"
        } else if reply.is_some() {
            "done"
        } else {
            "failed"
        };
        // 中断传播：父被停止时给仍活跃的后代写 session.interrupted_by_parent。
        //
        // 子日志以 `child.log` 为准（它含子会话自己的对话事件）；runtime 里的子日志副本
        // 只有 session.created + 中断事件，直接回写会丢掉子会话的回答。
        if interrupted {
            let _ = runtime.interrupt_tree(parent_id, "user_stop", &now_ts());
            let _ = child
                .log
                .append_interrupted_by_parent(now_ts(), parent_id, "user_stop");
        }
        child.save();
        let _ = runtime.finish_child(&child_id, outcome, &now_ts());
        let _ = runtime.sync_log(parent_id, log);
        // 父日志（含 child.spawned/finished）落盘：会话索引/树查询都从磁盘重建。
        let _ = store.write_log(log);

        let preview: String = reply
            .as_deref()
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect();
        let value = json!({
            "child_session_id": child_id,
            "parent_session_id": parent_id,
            "depth": spawned.depth,
            "outcome": outcome,
            "reply": preview,
        });
        ToolOutcome::success(json!([{ "type": "json", "value": value.clone() }]), value)
    }
}
