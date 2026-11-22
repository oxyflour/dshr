use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use dshr_compat_dsh_host::{
    HostService, HostServiceKind, HostServiceMethod, HostServiceTable, ServiceCallContext,
    ServiceError, SessionAccess, SessionEventView, TOOL_GOALS_V1,
};
use dshr_session_store::{EventSpec, SessionLog};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ChatSessionAccess;

pub(super) const CHANGE: &str = "goal/change";
const DEFAULT_ROUNDS: u64 = 256;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Goal {
    id: String,
    revision: u64,
    pub(super) objective: String,
    phase: Phase,
    rounds_started: u64,
    max_goal_rounds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocked_reason: Option<Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Active,
    Paused,
    Complete,
    Blocked,
}

impl Goal {
    pub(super) fn current(log: &SessionLog) -> Option<Self> {
        let event = log
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == CHANGE)?;
        serde_json::from_value(event.payload["goal"].clone()).ok()
    }

    pub(super) fn source(&self) -> Value {
        json!({ "kind": "goal", "goalId": self.id, "revision": self.revision, "round": self.rounds_started })
    }

    pub(super) fn record_round(&self, log: &mut SessionLog, ts: &str) -> Result<(), String> {
        log.append(EventSpec::durable(CHANGE, ts, change("round", self)))
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[derive(Default)]
pub(super) struct GoalActivation(Mutex<Option<(String, String)>>);

impl GoalActivation {
    fn is_armed(&self, session: &str, goal: &Goal) -> bool {
        let current = self.0.lock().unwrap_or_else(|error| error.into_inner());
        current
            .as_ref()
            .is_some_and(|(owner, id)| owner == session && id == &goal.id)
    }

    fn arm(&self, session: &str, goal: &Goal) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) =
            Some((session.to_owned(), goal.id.clone()));
    }

    pub(super) fn disarm(&self) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }

    pub(super) fn next_round(&self, log: &SessionLog) -> Option<Goal> {
        let mut goal = Goal::current(log)?;
        if !self.is_armed(log.session_id(), &goal) {
            return None;
        }
        if goal.phase != Phase::Active || goal.rounds_started >= goal.max_goal_rounds {
            self.disarm();
            return None;
        }
        goal.rounds_started += 1;
        Some(goal)
    }
}

struct Prepared {
    before: Option<Goal>,
    next: Goal,
    armed: bool,
    operation: String,
}

pub(super) struct GoalService {
    access: Arc<ChatSessionAccess>,
    prepared: HashMap<String, Prepared>,
    table: HostServiceTable,
}

impl GoalService {
    pub(super) fn new(access: Arc<ChatSessionAccess>) -> Self {
        Self {
            access,
            prepared: HashMap::new(),
            table: HostServiceTable {
                service_key: TOOL_GOALS_V1.to_owned(),
                limits: Default::default(),
                methods: vec![
                    method(
                        "prepare",
                        HostServiceKind::Read,
                        json!({
                            "current": {}, "next": {}, "source": {}, "subagent": {"type":"boolean"},
                        }),
                    ),
                    method("commit", HostServiceKind::Write, json!({})),
                ],
            },
        }
    }

    fn state(
        &self,
        session_id: &str,
        start_seq: u64,
    ) -> Result<(Option<Goal>, Value, bool, u64), ServiceError> {
        let inner = self
            .access
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let session = inner
            .published
            .as_ref()
            .filter(|snapshot| snapshot.header.id == session_id)
            .ok_or_else(|| denied("goal session is not active"))?;
        if session.cancel.load(Ordering::Acquire) {
            return Err(ServiceError::new("E_CANCELLED", "goal turn was cancelled"));
        }
        let turn = session
            .events
            .iter()
            .rev()
            .find(|event| matches!(event.event_type.as_str(), "turn.started" | "turn.ended"))
            .filter(|event| event.seq == start_seq && event.event_type == "turn.started")
            .ok_or_else(|| denied("goal tools require an open turn"))?;
        let subagent = session.header.parent_session_id.is_some();
        let source = if subagent {
            Value::Null
        } else if turn.data["source"]["kind"] == "goal" {
            turn.data["source"].clone()
        } else if turn.data["source"]["kind"] == "user" && has_direct_prompt(&session.events, turn)
        {
            json!({"kind":"user"})
        } else {
            Value::Null
        };
        let current = session
            .events
            .iter()
            .rev()
            .find(|event| event.event_type == CHANGE)
            .map(|event| serde_json::from_value(event.data["goal"].clone()))
            .transpose()
            .map_err(|_| ServiceError::new("E_INTERNAL", "stored goal is invalid"))?;
        Ok((current, source, subagent, session.next_seq))
    }

    fn prepare(
        &mut self,
        session: &str,
        request: &str,
        tool: &str,
        args: &Value,
        state: (Option<Goal>, Value, bool, u64),
    ) -> Result<Value, ServiceError> {
        let (current, source, subagent, seq) = state;
        let armed = current
            .as_ref()
            .is_some_and(|goal| self.access.goals.is_armed(session, goal));
        let current_json = current.as_ref().map(|goal| view(goal, armed));
        if tool == "get_goal" {
            return Ok(
                json!({"current": current_json, "next":null, "source":source, "subagent":subagent}),
            );
        }
        let operation = if tool == "create_goal" {
            "create"
        } else {
            args["action"].as_str().unwrap_or_default()
        };
        let direct = source["kind"] == "user";
        let automatic = current.as_ref().is_some_and(|goal| source == goal.source())
            && matches!(operation, "complete" | "blocked");
        if !direct && !automatic {
            return Err(denied(
                "goal mutation requires a direct user turn or its current goal round",
            ));
        }
        let (next, next_armed) = if tool == "create_goal" {
            if current
                .as_ref()
                .is_some_and(|goal| goal.phase != Phase::Complete)
            {
                return Err(invalid("complete the current goal before creating another"));
            }
            (
                Goal {
                    id: format!("goal-{session}-{seq}"),
                    revision: 1,
                    objective: required_text(&args["objective"], "objective")?,
                    phase: Phase::Active,
                    rounds_started: 0,
                    max_goal_rounds: optional_rounds(&args["max_goal_rounds"], false)?
                        .unwrap_or(DEFAULT_ROUNDS),
                    blocked_reason: None,
                },
                true,
            )
        } else {
            let before = current
                .as_ref()
                .filter(|goal| {
                    args["goal_id"].as_str() == Some(goal.id.as_str())
                        && args["revision"].as_u64() == Some(goal.revision)
                })
                .ok_or_else(|| invalid("goal ID or revision is stale"))?;
            update(before, armed, operation, args)?
        };
        let next_json = view(&next, next_armed);
        self.prepared.insert(
            request.to_owned(),
            Prepared {
                before: current,
                next,
                armed: next_armed,
                operation: operation.to_owned(),
            },
        );
        Ok(json!({"current":current_json, "next":next_json, "source":source, "subagent":subagent}))
    }
}

impl HostService for GoalService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        method: &str,
        args: &Value,
        context: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let (Some(session), Some(request)) = (&context.session_id, &context.tool_request_id) else {
            return Err(denied(
                "goal service requires the current session tool call",
            ));
        };
        let start = args["start_seq"]
            .as_u64()
            .ok_or_else(|| invalid("missing open turn"))?;
        let state = self.state(session, start)?;
        match method {
            "prepare" => self.prepare(
                session,
                request,
                args["tool"].as_str().unwrap_or_default(),
                &args["arguments"],
                state,
            ),
            "commit" => {
                let pending = self
                    .prepared
                    .remove(request)
                    .filter(|pending| pending.before == state.0)
                    .ok_or_else(|| {
                        invalid("goal update was not prepared or its revision has changed")
                    })?;
                self.access.append_event(
                    session,
                    CHANGE,
                    change(&pending.operation, &pending.next),
                )?;
                if pending.armed {
                    self.access.goals.arm(session, &pending.next);
                } else {
                    self.access.goals.disarm();
                }
                Ok(json!({}))
            }
            _ => Err(ServiceError::new("E_SERVICE_METHOD", "unknown goal method")),
        }
    }

    fn release_tool_call(&mut self, request: &str) {
        self.prepared.remove(request);
    }
}

fn has_direct_prompt(events: &[SessionEventView], turn: &SessionEventView) -> bool {
    let Some(id) = turn.data["prompt_message_id"].as_str() else {
        return false;
    };
    events
        .iter()
        .rev()
        .filter(|event| event.seq < turn.seq)
        .take_while(|event| !matches!(event.event_type.as_str(), "turn.started" | "turn.ended"))
        .any(|event| event.event_type == "message.user" && event.data["message_id"] == id)
}

fn update(
    before: &Goal,
    armed: bool,
    operation: &str,
    args: &Value,
) -> Result<(Goal, bool), ServiceError> {
    let mut goal = before.clone();
    let activation = match operation {
        "edit" => {
            let objective = args["objective"].as_str().filter(|text| !text.is_empty());
            let cap = optional_rounds(&args["max_goal_rounds"], true)?;
            if objective.is_none() && cap.is_none() {
                return Err(invalid("the edit has no changes"));
            }
            if objective.is_some() {
                goal.objective = required_text(&args["objective"], "objective")?;
            }
            if let Some(cap) = cap {
                goal.max_goal_rounds = cap;
            }
            armed && goal.rounds_started < goal.max_goal_rounds
        }
        "pause" if goal.phase == Phase::Active => {
            goal.phase = Phase::Paused;
            false
        }
        "resume"
            if matches!(goal.phase, Phase::Active | Phase::Blocked)
                && !armed
                && goal.rounds_started < goal.max_goal_rounds =>
        {
            goal.phase = Phase::Active;
            goal.blocked_reason = None;
            true
        }
        "complete" if goal.phase != Phase::Complete => {
            goal.phase = Phase::Complete;
            goal.blocked_reason = None;
            false
        }
        "blocked" if goal.phase == Phase::Active => {
            goal.phase = Phase::Blocked;
            goal.blocked_reason = Some(json!({
                "code":"model-reported", "message":required_text(&args["blocked_reason"], "blocked_reason")?,
            }));
            false
        }
        _ => return Err(invalid("goal transition is unavailable")),
    };
    goal.revision = goal
        .revision
        .checked_add(1)
        .filter(|revision| *revision <= MAX_SAFE_INTEGER)
        .ok_or_else(|| invalid("goal revision limit reached"))?;
    Ok((goal, activation))
}

fn optional_rounds(value: &Value, omit_zero: bool) -> Result<Option<u64>, ServiceError> {
    if value.is_null() || (omit_zero && *value == 0) {
        return Ok(None);
    }
    value
        .as_u64()
        .filter(|n| (1..=MAX_SAFE_INTEGER).contains(n))
        .map(Some)
        .ok_or_else(|| invalid("max_goal_rounds must be a positive safe integer"))
}

fn required_text(value: &Value, name: &str) -> Result<String, ServiceError> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty() && text.len() <= 16_384)
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("{name} must contain 1 to 16384 bytes of text")))
}

fn view(goal: &Goal, armed: bool) -> Value {
    let mut value = serde_json::to_value(goal).expect("serializable goal");
    value["activation"] = json!(if armed { "armed" } else { "disarmed" });
    value
}

fn change(operation: &str, goal: &Goal) -> Value {
    json!({"version":1, "operation":operation, "goal":goal})
}

fn method(name: &str, kind: HostServiceKind, result: Value) -> HostServiceMethod {
    let required: Vec<_> = result
        .as_object()
        .expect("result properties")
        .keys()
        .collect();
    HostServiceMethod {
        name: name.to_owned(),
        kind,
        requires_session: true,
        idempotent: false,
        timeout_ms: 5_000,
        args_schema: json!({
            "type":"object", "additionalProperties":false, "required":["start_seq","tool","arguments"],
            "properties":{"start_seq":{"type":"integer"},"tool":{"type":"string"},"arguments":{"type":"object"}},
        }),
        result_schema: json!({"type":"object","additionalProperties":false,"required":required,"properties":result}),
    }
}

fn denied(message: &str) -> ServiceError {
    ServiceError::new("E_SERVICE_DENIED", message)
}
fn invalid(message: impl Into<String>) -> ServiceError {
    ServiceError::new("E_SERVICE_ARGS", message)
}
