use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dshr_compat_dsh_host::{
    HostService, HostServiceKind, HostServiceMethod, HostServiceTable, ServiceCallContext,
    ServiceError, SessionAccess, TOOL_SUBAGENTS_V1,
};
use dshr_session_store::{
    try_lock_session, EventSpec, SessionCreated, SessionIndex, SessionLog, SpawnSource,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    now_ts, ApprovalBroker, ChatRuntime, ChatSessionAccess, ChatState, DesktopSettings,
    LivePermission, PermissionMode, TurnReason,
};

const CREATED: &str = "subagent/created";
const STARTED: &str = "subagent/started";
const SETTLED: &str = "subagent/settled";
const NOTIFIED: &str = "subagent/notified";
const MESSAGE_SENT: &str = "subagent/message-sent";
const MAX_ACTIVE: usize = 8;
const MAX_CHILDREN: usize = 64;
const MAX_QUEUE: usize = 16;

#[derive(Clone)]
pub(super) struct ChildEnvironment {
    pub(super) settings: DesktopSettings,
    pub(super) permission: LivePermission,
    pub(super) diagnostics: bool,
}

impl ChildEnvironment {
    fn with_delegated_permission(mut self, mode: Option<&str>) -> Self {
        let mode = mode
            .and_then(PermissionMode::parse)
            .unwrap_or(PermissionMode::ReadOnly);
        self.permission = LivePermission::new(Some(mode));
        self.settings.permission = Some(mode);
        self
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Details {
    name: String,
    label: String,
    cwd: PathBuf,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default)]
    oneshot: bool,
}

struct Worker {
    owner: String,
    cancel: Arc<AtomicBool>,
    pending: VecDeque<String>,
}

#[derive(Default)]
pub(super) struct ChildSupervisor {
    running: Mutex<HashMap<String, Worker>>,
    inbox: Mutex<HashMap<String, Vec<Value>>>,
}

impl ChildSupervisor {
    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<String, Worker>> {
        self.running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<Value>>> {
        self.inbox.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(super) fn stop_descendants(&self, parent: &str) {
        let workers = self.running();
        let mut owners = HashSet::from([parent]);
        loop {
            let previous = owners.len();
            for (id, worker) in workers.iter() {
                if owners.contains(worker.owner.as_str()) {
                    worker.cancel.store(true, Ordering::Release);
                    owners.insert(id);
                }
            }
            if owners.len() == previous {
                break;
            }
        }
    }

    fn launch(
        self: &Arc<Self>,
        store: SessionIndex,
        owner: String,
        log: SessionLog,
        workspace: PathBuf,
        environment: ChildEnvironment,
        first: String,
    ) -> Result<(), ServiceError> {
        let id = log.session_id().to_owned();
        let mut workers = self.running();
        if workers.len() >= MAX_ACTIVE || workers.contains_key(&id) {
            return Err(denied("subagent execution slots are full"));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        workers.insert(
            id.clone(),
            Worker {
                owner,
                cancel: cancel.clone(),
                pending: VecDeque::new(),
            },
        );
        let runtime = self.clone();
        let requested = std::thread::Builder::new()
            .name(format!("dshr-child-{id}"))
            .spawn(move || {
                let mut state = ChatState::from_log_in(
                    store.path_for(log.session_id()),
                    log,
                    workspace,
                    &cancel,
                    ApprovalBroker::noninteractive(),
                    environment.settings.clone(),
                    ChatRuntime::Desktop {
                        allow_spawn: false,
                        sidecar: true,
                        diagnostics: environment.diagnostics,
                    },
                    Some((runtime.clone(), environment.permission)),
                );
                let id = state.session_id.clone();
                let mut prompt = first;
                loop {
                    let ts = now_ts();
                    let result = state
                        .log
                        .append(EventSpec::durable(STARTED, &ts, json!({})))
                        .map_err(|error| error.to_string())
                        .and_then(|_| state.save_checked())
                        .and_then(|_| {
                            if cancel.load(Ordering::Acquire) {
                                return Err("subagent execution cancelled".to_owned());
                            }
                            state.run_turn(&prompt, &[], &mut |_| {})
                        });
                    let phase = if cancel.load(Ordering::Acquire) {
                        "interrupted"
                    } else {
                        match result.as_ref().map(|turn| turn.reason) {
                            Ok(TurnReason::Completed) => "completed",
                            Ok(TurnReason::Cancelled) => "interrupted",
                            _ => "failed",
                        }
                    };
                    let reply = result.map(|turn| turn.reply).unwrap_or_else(|error| error);
                    let settled = state
                        .log
                        .append(EventSpec::durable(
                            SETTLED,
                            now_ts(),
                            json!({
                                "outcome":phase, "reply":bounded(&reply, 16_384),
                            }),
                        ))
                        .map_err(|error| error.to_string())
                        .and_then(|_| state.save_checked());
                    if let Err(error) = &settled {
                        eprintln!("[subagent] cannot save {id}: {error}");
                    }
                    let mut workers = runtime.running();
                    let next = if phase == "interrupted" || settled.is_err() {
                        None
                    } else {
                        workers
                            .get_mut(&id)
                            .and_then(|worker| worker.pending.pop_front())
                    };
                    if let Some(next) = next {
                        prompt = next;
                    } else {
                        workers.remove(&id);
                        break;
                    }
                }
            });
        if let Err(error) = requested {
            workers.remove(&id);
            return Err(internal(error));
        }
        Ok(())
    }
}

pub(super) struct SubagentService {
    access: Arc<ChatSessionAccess>,
    table: HostServiceTable,
    workflows: HashMap<String, Vec<String>>,
}

impl SubagentService {
    pub(super) fn new(access: Arc<ChatSessionAccess>) -> Self {
        let write = |name| {
            method(
                name,
                HostServiceKind::Write,
                json!({"arguments":{"type":"object"}}),
                json!({"result":{}}),
            )
        };
        Self {
            access,
            workflows: HashMap::new(),
            table: HostServiceTable::new(
                TOOL_SUBAGENTS_V1,
                vec![
                    method(
                        "list",
                        HostServiceKind::Read,
                        json!({}),
                        json!({"tree":{"type":"object"}}),
                    ),
                    method(
                        "pending",
                        HostServiceKind::Read,
                        json!({}),
                        json!({"notices":{"type":"array"}}),
                    ),
                    method(
                        "notices",
                        HostServiceKind::Write,
                        json!({}),
                        json!({"notices":{"type":"array"}}),
                    ),
                    HostServiceMethod {
                        args_schema: json!({"type":"object","required":["arguments"],"additionalProperties":false,
                        "properties":{"arguments":{"type":"object"}, "max_depth":{"type":"integer","minimum":0}}}),
                        ..write("start")
                    },
                    write("message"),
                    write("interrupt"),
                    write("workflow.start"),
                    method(
                        "workflow.poll",
                        HostServiceKind::Read,
                        json!({"child":{"type":"string"}}),
                        json!({"status":{},"reply":{}}),
                    ),
                    method(
                        "workflow.stop",
                        HostServiceKind::Write,
                        json!({"child":{"type":"string"}}),
                        json!({}),
                    ),
                ],
            ),
        }
    }

    fn snapshot(&self, caller: &str) -> Result<Snapshot, ServiceError> {
        let inner = self
            .access
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let published = inner
            .published
            .as_ref()
            .filter(|snapshot| snapshot.header.id == caller)
            .ok_or_else(|| denied("caller session is unavailable"))?;
        if published.cancel.load(Ordering::Acquire) {
            return Err(ServiceError::new("E_CANCELLED", "agent call was cancelled"));
        }
        let workspace = published
            .header
            .cwd
            .clone()
            .ok_or_else(|| denied("caller workspace is unavailable"))?;
        let store = SessionIndex::new(&inner.sessions_dir);
        let environment = inner
            .child_environment
            .clone()
            .ok_or_else(|| denied("subagent runtime is unavailable"))?;
        let seen = published
            .events
            .iter()
            .filter(|event| event.event_type == NOTIFIED)
            .filter_map(|event| event.data["id"].as_str().map(str::to_owned))
            .collect();
        let cwd = published
            .events
            .iter()
            .rev()
            .find(|event| event.event_type == "working-directory/change")
            .and_then(|event| event.data["cwd"].as_str())
            .unwrap_or(&workspace)
            .to_owned();
        let depth = published.header.delegation_depth;
        drop(inner);
        let tree = ChildTree::read(&store, caller, &workspace)?;
        Ok(Snapshot {
            workspace,
            store,
            environment,
            seen,
            cwd,
            depth,
            tree,
        })
    }

    fn start(
        &self,
        caller: &str,
        snapshot: Snapshot,
        args: &Value,
        max_depth: u32,
        oneshot: bool,
    ) -> Result<Value, ServiceError> {
        if snapshot.depth >= max_depth.min(super::DEFAULT_MAX_DEPTH) {
            return Err(denied("subagent depth limit reached"));
        }
        let siblings: Vec<_> = snapshot
            .tree
            .children
            .iter()
            .filter(|child| child.owner == caller)
            .collect();
        if siblings.len() >= MAX_CHILDREN {
            return Err(denied("subagent count limit reached"));
        }
        if self.access.children.running().len() >= MAX_ACTIVE {
            return Err(denied("subagent execution slots are full"));
        }
        let name = if let Some(name) = args["name"].as_str() {
            if !valid_name(name) || siblings.iter().any(|child| child.details.name == name) {
                return Err(denied("subagent name is invalid or already in use"));
            }
            name.to_owned()
        } else {
            (1..=MAX_CHILDREN)
                .map(|n| format!("agent-{n}"))
                .find(|name| !siblings.iter().any(|child| child.details.name == *name))
                .ok_or_else(|| denied("no agent name is available"))?
        };
        let prompt = text(&args["prompt"], "prompt", 64 * 1024)?.to_owned();
        let label = text(&args["description"], "description", 256)?.to_owned();
        let workspace = PathBuf::from(&snapshot.workspace);
        let root = workspace.canonicalize().map_err(internal)?;
        let base = PathBuf::from(&snapshot.cwd);
        let path = args["cwd"]
            .as_str()
            .map_or(base.clone(), |cwd| base.join(cwd));
        let cwd = path
            .canonicalize()
            .map_err(|_| denied("child working directory is unavailable"))?;
        if !cwd.is_dir() || !cwd.starts_with(&root) {
            return Err(denied("child working directory is outside the workspace"));
        }
        let details = Details {
            name: name.clone(),
            label: label.clone(),
            cwd: cwd.clone(),
            permission_mode: snapshot
                .environment
                .permission
                .mode()
                .map(|mode| mode.as_str().to_owned()),
            oneshot,
        };
        let id = snapshot.store.new_id();
        let path = snapshot.store.path_for(&id);
        let mut log = SessionLog::new(&id);
        let ts = now_ts();
        log.append_created(
            &ts,
            &SessionCreated::child(&snapshot.workspace, caller, SpawnSource::Agent),
        )
        .map_err(internal)?;
        log.append(EventSpec::durable(CREATED, &ts, json!(details)))
            .map_err(internal)?;
        log.append(EventSpec::durable(
            "working-directory/change",
            &ts,
            json!({"cwd":cwd}),
        ))
        .map_err(internal)?;
        {
            let _writer = try_lock_session(&path).map_err(internal)?;
            if path.exists() {
                return Err(denied("child session already exists"));
            }
            snapshot.store.write_log(&log).map_err(internal)?;
        }
        self.access.append_event(
            caller,
            "child.spawned",
            json!({
                "child_session_id":id, "spawn_source":"agent", "label":label,
            }),
        )?;
        if let Err(error) = self.access.children.launch(
            snapshot.store.clone(),
            caller.to_owned(),
            log.clone(),
            workspace,
            snapshot
                .environment
                .with_delegated_permission(details.permission_mode.as_deref()),
            prompt,
        ) {
            log.append(EventSpec::durable(
                SETTLED,
                now_ts(),
                json!({"outcome":"failed","reply":error.message}),
            ))
            .map_err(internal)?;
            snapshot.store.write_log(&log).map_err(internal)?;
            return Err(error);
        }
        Ok(json!({"name":name,"id":id}))
    }

    fn notices(
        &self,
        caller: &str,
        snapshot: &Snapshot,
        consume: bool,
    ) -> Result<Value, ServiceError> {
        let mut pending = self.access.children.pending();
        let mut notices: Vec<Value> = pending
            .get(caller)
            .into_iter()
            .flatten()
            .filter(|notice| {
                notice["id"]
                    .as_str()
                    .is_some_and(|id| !snapshot.seen.contains(id))
            })
            .take(MAX_QUEUE)
            .cloned()
            .collect();
        for child in snapshot
            .tree
            .children
            .iter()
            .filter(|child| child.owner == caller && !child.details.oneshot)
        {
            let Some((seq, outcome, reply)) = child.settlement() else {
                continue;
            };
            let id = format!("child-{}-{seq}", child.id);
            if snapshot.seen.contains(&id) {
                continue;
            }
            notices.push(
                json!({"id":id,"text":format!("Subagent {} [{}]: {}", child.path, outcome, reply)}),
            );
            if consume {
                self.access.append_event(
                    caller,
                    "child.finished",
                    json!({"child_session_id":child.id,"outcome":outcome}),
                )?;
            }
            if notices.len() >= MAX_QUEUE {
                break;
            }
        }
        if consume {
            for notice in &notices {
                self.access
                    .append_event(caller, NOTIFIED, json!({"id":notice["id"]}))?;
            }
            pending.remove(caller);
        }
        Ok(json!({"notices":notices}))
    }

    fn control(
        &self,
        caller: &str,
        snapshot: Snapshot,
        args: &Value,
        interrupt: bool,
    ) -> Result<Value, ServiceError> {
        let name = text(&args["agent"], "agent", 256)?;
        let target = snapshot
            .tree
            .resolve(caller, name)
            .ok_or_else(|| denied("agent is outside the caller tree"))?;
        if target == caller {
            return Err(denied("cannot control the calling agent"));
        }
        if interrupt {
            if !snapshot.tree.is_descendant(caller, target) {
                return Err(denied("agent is not a descendant of the caller"));
            }
            let workers = self.access.children.running();
            let worker = workers
                .get(target)
                .ok_or_else(|| denied("target agent is inactive"))?;
            worker.cancel.store(true, Ordering::Release);
            return Ok(json!(true));
        }
        let message = text(&args["message"], "message", 64 * 1024)?;
        let id = self
            .access
            .append_event(caller, MESSAGE_SENT, json!({"target":target}))?;
        let id = format!("message-{caller}-{id}");
        let text = format!("Message from {}:\n{message}", snapshot.tree.name_of(caller));
        if target == snapshot.tree.root {
            let mut pending = self.access.children.pending();
            let messages = pending.entry(target.to_owned()).or_default();
            if messages.len() >= MAX_QUEUE {
                return Err(denied("agent inbox is full"));
            }
            messages.push(json!({"id":id,"text":bounded(&text, 4_096)}));
        } else {
            let mut workers = self.access.children.running();
            if let Some(worker) = workers.get_mut(target) {
                if worker.pending.len() >= MAX_QUEUE {
                    return Err(denied("agent inbox is full"));
                }
                worker.pending.push_back(text);
            } else {
                drop(workers);
                let child = snapshot
                    .tree
                    .children
                    .iter()
                    .find(|child| child.id == target)
                    .ok_or_else(|| denied("agent is unavailable"))?;
                self.access.children.launch(
                    snapshot.store,
                    child.owner.clone(),
                    child.log.clone(),
                    PathBuf::from(snapshot.workspace),
                    snapshot
                        .environment
                        .with_delegated_permission(child.details.permission_mode.as_deref()),
                    text,
                )?;
            }
        }
        Ok(json!(id))
    }
}

impl HostService for SubagentService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        name: &str,
        args: &Value,
        context: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let caller = context
            .session_id
            .as_deref()
            .ok_or_else(|| denied("agent session is unavailable"))?;
        let snapshot = self.snapshot(caller)?;
        match name {
            "workflow.start" => {
                let request = context
                    .tool_request_id
                    .as_ref()
                    .ok_or_else(|| denied("workflow call is unavailable"))?;
                if self
                    .workflows
                    .get(request)
                    .is_some_and(|ids| ids.len() >= 64)
                {
                    return Err(denied("workflow child budget reached"));
                }
                let row = self.start(
                    caller,
                    snapshot,
                    &args["arguments"],
                    super::DEFAULT_MAX_DEPTH,
                    true,
                )?;
                let id = row["id"]
                    .as_str()
                    .ok_or_else(|| internal("missing workflow child ID"))?;
                self.workflows
                    .entry(request.clone())
                    .or_default()
                    .push(id.to_owned());
                Ok(json!({"result":row}))
            }
            "workflow.poll" | "workflow.stop" => {
                let id = args["child"]
                    .as_str()
                    .ok_or_else(|| denied("workflow child is unavailable"))?;
                let owned = context
                    .tool_request_id
                    .as_ref()
                    .and_then(|request| self.workflows.get(request))
                    .is_some_and(|ids| ids.iter().any(|entry| entry == id));
                if !owned {
                    return Err(denied("workflow child is outside the current call"));
                }
                if name == "workflow.stop" {
                    if let Some(worker) = self.access.children.running().get(id) {
                        worker.cancel.store(true, Ordering::Release);
                    }
                    return Ok(json!({}));
                }
                let (phase, reply) = snapshot
                    .tree
                    .children
                    .iter()
                    .find(|child| child.id == id)
                    .and_then(Child::settlement)
                    .map(|(_, phase, reply)| (phase, reply))
                    .unwrap_or(("running", ""));
                Ok(json!({"status":phase,"reply":reply}))
            }
            "list" => {
                let workers = self.access.children.running();
                Ok(json!({"tree":snapshot.tree.view(caller, &workers)}))
            }
            "start" => self
                .start(
                    caller,
                    snapshot,
                    &args["arguments"],
                    args["max_depth"]
                        .as_u64()
                        .and_then(|depth| u32::try_from(depth).ok())
                        .unwrap_or(super::DEFAULT_MAX_DEPTH),
                    false,
                )
                .map(|result| json!({"result":result})),
            "pending" | "notices" => self.notices(caller, &snapshot, name == "notices"),
            "message" | "interrupt" => self
                .control(caller, snapshot, &args["arguments"], name == "interrupt")
                .map(|result| json!({"result":result})),
            _ => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                "unknown subagent method",
            )),
        }
    }

    fn release_tool_call(&mut self, request: &str) {
        if let Some(ids) = self.workflows.remove(request) {
            let workers = self.access.children.running();
            for id in ids {
                if let Some(worker) = workers.get(&id) {
                    worker.cancel.store(true, Ordering::Release);
                }
            }
        }
    }
}

struct Snapshot {
    workspace: String,
    store: SessionIndex,
    environment: ChildEnvironment,
    seen: HashSet<String>,
    cwd: String,
    depth: u32,
    tree: ChildTree,
}

struct Child {
    id: String,
    owner: String,
    details: Details,
    path: String,
    log: SessionLog,
}

impl Child {
    fn settlement(&self) -> Option<(u64, &str, &str)> {
        let event = self
            .log
            .events()
            .iter()
            .rev()
            .find(|event| matches!(event.event_type.as_str(), STARTED | SETTLED))?;
        (event.event_type == SETTLED).then(|| {
            (
                event.seq,
                event.payload["outcome"].as_str().unwrap_or("failed"),
                event.payload["reply"].as_str().unwrap_or_default(),
            )
        })
    }
}

struct ChildTree {
    root: String,
    children: Vec<Child>,
}

pub(super) fn team_scope(
    access: &ChatSessionAccess,
    caller: &str,
) -> Result<(PathBuf, String, HashMap<String, String>), ServiceError> {
    let (dir, workspace) = {
        let inner = access
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let session = inner
            .published
            .as_ref()
            .filter(|session| session.header.id == caller)
            .ok_or_else(|| denied("team session is unavailable"))?;
        if session.cancel.load(Ordering::Acquire) {
            return Err(ServiceError::new("E_CANCELLED", "team call was cancelled"));
        }
        let workspace = session
            .header
            .cwd
            .clone()
            .ok_or_else(|| denied("team workspace is unavailable"))?;
        (inner.sessions_dir.clone(), workspace)
    };
    let tree = ChildTree::read(&SessionIndex::new(&dir), caller, &workspace)?;
    if tree.root != caller && !tree.children.iter().any(|child| child.id == caller) {
        return Err(denied("caller is outside this team"));
    }
    let mut members = HashMap::from([("lead".to_owned(), tree.root.clone())]);
    for child in tree.children {
        if !child.details.oneshot {
            members.insert(child.path, child.id);
        }
    }
    Ok((dir, tree.root, members))
}

impl ChildTree {
    fn read(store: &SessionIndex, caller: &str, workspace: &str) -> Result<Self, ServiceError> {
        let catalog = store.scan().map_err(internal)?;
        let owns = |meta: &&dshr_session_store::SessionMeta| {
            meta.workspace_id.as_deref() == Some(workspace)
        };
        let mut parents = HashMap::new();
        for meta in catalog.iter().filter(owns) {
            parents.insert(meta.id.as_str(), meta.parent_session_id.as_deref());
        }
        let mut root = caller;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(root) {
                return Err(denied("agent ancestry is invalid"));
            }
            match parents.get(root) {
                Some(Some(parent)) => root = parent,
                Some(None) => break,
                None => return Err(denied("agent ancestry is unavailable")),
            }
        }
        let mut tree = Self {
            root: root.to_owned(),
            children: Vec::new(),
        };
        let mut frontier = VecDeque::from([(tree.root.clone(), String::new())]);
        while let Some((owner, prefix)) = frontier.pop_front() {
            for meta in catalog
                .iter()
                .filter(owns)
                .filter(|meta| meta.parent_session_id.as_deref() == Some(&owner))
            {
                if tree.children.len() >= MAX_CHILDREN * 2 {
                    return Ok(tree);
                }
                let Some(log) = super::dsh_session_query::safe_load(store.dir(), &meta.id) else {
                    continue;
                };
                let Some(details) = log
                    .events()
                    .iter()
                    .find(|event| event.event_type == CREATED)
                    .and_then(|event| {
                        serde_json::from_value::<Details>(event.payload.clone()).ok()
                    })
                else {
                    continue;
                };
                if !valid_name(&details.name)
                    || tree.children.iter().any(|child| child.id == meta.id)
                {
                    continue;
                }
                let path = if prefix.is_empty() {
                    details.name.clone()
                } else {
                    format!("{prefix}/{}", details.name)
                };
                frontier.push_back((meta.id.clone(), path.clone()));
                tree.children.push(Child {
                    id: meta.id.clone(),
                    owner: owner.clone(),
                    details,
                    path,
                    log,
                });
            }
        }
        Ok(tree)
    }

    fn name_of(&self, id: &str) -> &str {
        self.children
            .iter()
            .find(|child| child.id == id)
            .map(|child| child.path.as_str())
            .unwrap_or("lead")
    }

    fn resolve(&self, caller: &str, name: &str) -> Option<&str> {
        if name == "lead" {
            return Some(&self.root);
        }
        self.children
            .iter()
            .find(|child| {
                !child.details.oneshot
                    && (child.path == name || (child.owner == caller && child.details.name == name))
            })
            .map(|child| child.id.as_str())
    }

    fn is_descendant(&self, caller: &str, target: &str) -> bool {
        let Some(child) = self.children.iter().find(|child| child.id == target) else {
            return false;
        };
        if caller == self.root {
            return true;
        }
        let prefix = self.name_of(caller);
        child
            .path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
    }

    fn view(&self, caller: &str, workers: &HashMap<String, Worker>) -> Value {
        let own = self.children.iter().find(|child| child.id == caller);
        json!({
            "root":self.root,"self":self.name_of(caller),"parent":own.map(|child|self.name_of(&child.owner)),
            "entries":self.children.iter().filter(|child| !child.details.oneshot).map(|child| json!({
                "id": child.id, "kind":"child", "mode":"continuable", "owner":child.owner,
                "name":child.details.name,"path":child.path,"label":child.details.label,
                "running":workers.contains_key(&child.id),
            })).collect::<Vec<_>>(),
        })
    }
}

fn method(
    name: &str,
    kind: HostServiceKind,
    properties: Value,
    result: Value,
) -> HostServiceMethod {
    let args_required: Vec<_> = properties
        .as_object()
        .expect("args properties")
        .keys()
        .collect();
    let result_required: Vec<_> = result
        .as_object()
        .expect("result properties")
        .keys()
        .collect();
    HostServiceMethod {
        name: name.to_owned(),
        kind,
        timeout_ms: 5_000,
        idempotent: kind == HostServiceKind::Read,
        requires_session: true,
        args_schema: json!({"type":"object","additionalProperties":false,"required":args_required,"properties":properties}),
        result_schema: json!({"type":"object","additionalProperties":false,"required":result_required,"properties":result}),
    }
}

fn valid_name(name: &str) -> bool {
    name.len() <= 32
        && name != "lead"
        && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn text<'a>(value: &'a Value, field: &str, max: usize) -> Result<&'a str, ServiceError> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty() && text.len() <= max)
        .ok_or_else(|| {
            ServiceError::args(
                format!("/arguments/{field}"),
                "text is empty or exceeds size limit",
            )
        })
}

fn bounded(text: &str, max: usize) -> &str {
    &text[..text.floor_char_boundary(max)]
}

fn denied(message: &str) -> ServiceError {
    ServiceError::new("E_SERVICE_DENIED", message)
}
fn internal(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new("E_INTERNAL", error.to_string())
}
