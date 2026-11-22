use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dshr_compat_dsh_protocol::{RpcError, KIND_HOST_SERVICE_RESULT};
use dshr_tool_runtime::{ToolDefinition, ToolOrigin, ToolRegistry};
use serde_json::{json, Value};

use super::{ActiveCall, BatchInvokeCall, Envelope, SidecarHost, CAP_PROGRAM_TOOLS};

const MAX_ACTIVE_CHILDREN: usize = 8;

type Approval =
    dyn Fn(&ToolDefinition, &str, &Value, Duration) -> Result<Option<String>, String> + Send + Sync;
type Audit = dyn Fn(Value) -> Result<(), String> + Send + Sync;

/// The Rust-selected bindings and live policy for one model step.
pub struct ProgramToolScope {
    pub session_id: String,
    pub tools: HashMap<String, (ToolDefinition, Duration)>,
    pub authorize: Arc<Approval>,
    pub record_admission: Arc<Audit>,
}

impl SidecarHost {
    pub fn set_program_tools(&mut self, scope: Option<ProgramToolScope>) {
        self.program_tools = scope;
    }

    pub(super) fn call_is_current(&self, call: &ActiveCall) -> bool {
        if call.cancelled {
            return false;
        }
        let Some(parent) = &call.parent else {
            return true;
        };
        call.deadline
            .is_some_and(|deadline| Instant::now() < deadline)
            && self
                .active_calls
                .get(parent)
                .is_some_and(|parent| !parent.cancelled)
    }

    pub(super) fn finish_program_children(&mut self, request: &str) {
        let children: Vec<_> = self
            .active_calls
            .iter()
            .filter(|(_, call)| call.parent.as_deref() == Some(request))
            .map(|(id, _)| id.clone())
            .collect();
        for id in children {
            self.active_calls.remove(&id);
            self.release_tool_resources(&id);
        }
    }

    pub(super) fn handle_program_callback(&mut self, request: &Envelope) -> Envelope {
        let mut reply = request.reply(KIND_HOST_SERVICE_RESULT, json!({}));
        match self.program_callback(request) {
            Ok(value) => reply.payload = json!({"result":value}),
            Err(error) => reply.error = Some(error),
        }
        reply
    }

    fn program_callback(&mut self, request: &Envelope) -> Result<Value, RpcError> {
        let deny = || {
            error(
                "E_SERVICE_DENIED",
                "nested tool call is outside its authorized program scope",
            )
        };
        if !self.host_services_active
            || !self.host_caps.iter().any(|cap| cap == CAP_PROGRAM_TOOLS)
            || request.plugin_id.as_deref() != Some("dsh-official")
            || request.depth > 2
            || request.runtime_generation != self.generation
        {
            return Err(deny());
        }
        let parent_request = request.payload["tool_request_id"]
            .as_str()
            .ok_or_else(deny)?;
        let parent = self
            .bound_service_call(request)
            .filter(|call| call.tool == "run_code" && call.parent.is_none())
            .ok_or_else(deny)?;
        let scope = self
            .program_tools
            .as_ref()
            .filter(|scope| scope.session_id == parent.session_id)
            .ok_or_else(deny)?;
        let args = &request.payload["args"];
        if request.payload["method"] == "finish" {
            let id = args["id"].as_str().ok_or_else(deny)?;
            let child = self
                .active_calls
                .get(id)
                .filter(|child| child.parent.as_deref() == Some(parent_request))
                .ok_or_else(deny)?;
            let expired = child
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline);
            self.active_calls.remove(id);
            self.release_tool_resources(id);
            if expired {
                return Err(error("E_TOOL_TIMEOUT", "nested tool deadline elapsed"));
            }
            return Ok(json!({}));
        }
        if request.payload["method"] != "begin" {
            return Err(deny());
        }
        let name = args["name"]
            .as_str()
            .filter(|name| *name != "run_code")
            .ok_or_else(deny)?;
        let (definition, timeout) = scope.tools.get(name).ok_or_else(deny)?;
        let ToolOrigin::Sidecar {
            plugin_id,
            registration_id,
            generation,
        } = &definition.origin
        else {
            return Err(deny());
        };
        let revision = self
            .preset_bindings
            .get(&parent.session_id)
            .map(|(_, value)| value);
        let registered = self.registrations.iter().any(|registration| {
            registration_id == &registration.registration_id
                && args["registration"] == *registration_id
                && generation == &self.generation
                && plugin_id == "dsh-official"
                && plugin_id == &registration.plugin_id
                && registration.name == name
                && registration
                    .session_id
                    .as_ref()
                    .is_none_or(|owner| owner == &parent.session_id)
                && registration
                    .preset_revision
                    .as_ref()
                    .is_none_or(|mounted| Some(mounted) == revision)
        });
        if !registered || self.retired_sessions.contains(&parent.session_id) {
            return Err(deny());
        }
        let call_id = args["call_id"]
            .as_str()
            .filter(|id| id.len() <= 512)
            .ok_or_else(deny)?;
        let index = call_id
            .strip_prefix(parent.call_id.as_str())
            .and_then(|suffix| suffix.strip_prefix(":ptc:"))
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| (1..=256).contains(value));
        if index.is_none()
            || self
                .active_calls
                .values()
                .any(|call| call.call_id == call_id)
        {
            return Err(deny());
        }
        if self
            .active_calls
            .values()
            .filter(|call| call.parent.as_deref() == Some(parent_request))
            .count()
            >= MAX_ACTIVE_CHILDREN
        {
            return Err(error(
                "E_SERVICE_DENIED",
                "too many active nested tool calls",
            ));
        }
        let parameters = args
            .get("arguments")
            .filter(|value| value.is_object())
            .ok_or_else(deny)?;
        if serde_json::to_vec(parameters).map_or(true, |encoded| encoded.len() > 256 * 1024) {
            return Err(deny());
        }
        ToolRegistry::validate_definition(definition, parameters)
            .map_err(|failure| error("E_SERVICE_ARGS", failure.to_string()))?;
        let deadline = parent.deadline.ok_or_else(deny)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(error("E_TOOL_TIMEOUT", "program deadline elapsed"));
        }
        let approval_key = (scope.authorize)(definition, call_id, parameters, remaining)
            .map_err(|message| error("E_SERVICE_DENIED", message))?;
        let timeout = (*timeout).min(deadline.saturating_duration_since(Instant::now()));
        if timeout.as_millis() == 0 {
            return Err(error("E_TOOL_TIMEOUT", "program deadline elapsed"));
        }
        let invocation = BatchInvokeCall::new(call_id, name, parameters.clone(), timeout)
            .with_approval_key(approval_key);
        let mut child = ActiveCall::from_batch(&invocation, &parent.session_id, parent.turn);
        child.parent = Some(parent_request.to_owned());
        (scope.record_admission)(json!({
            "parent_call_id":parent.call_id,"call_id":call_id,"tool":name,"registration_id":registration_id,
            "generation":generation,
        })).map_err(|message| error("E_INTERNAL", message))?;
        self.seq += 1;
        let id = format!("req-program-{}", self.seq);
        self.active_calls.insert(id.clone(), child);
        Ok(json!({"id":id,"timeout_ms":timeout.as_millis() as u64}))
    }
}

fn error(code: &str, message: impl Into<String>) -> RpcError {
    RpcError {
        code: code.to_owned(),
        message: message.into(),
        retryable: false,
    }
}
