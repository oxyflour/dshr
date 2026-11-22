use super::{configured_models, ChatSessionAccess, DesktopModel, DesktopSettings, LivePermission};
use crate::{
    blank_root_created, creator, dsh_subagents, last_message_seq, loader_inputs, now_ts,
    open_session_log, read_agent_presets_from, register_probe_write, register_sidecar_tools,
    session_workspace, spawn_sidecar_in, spill_dir_for, OpenedSessionLog, ProbeNativePlugin,
};
use dshr_agent_context::{plan_context_with_spill, ContextPlan, SpillPolicy, SpillStore};
use dshr_agent_core::{Agent, ModelProvider, ReasoningEffort};
use dshr_compat_dsh_host::SidecarHost;
use dshr_plugin_host::PluginHost;
use dshr_session_store::{save_atomic, try_lock_session, SessionLog};
use dshr_tool_runtime::ApprovalBroker;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// 一个已取到的插件 ADC 面板（宿主侧缓存；渲染只读，避免每帧走插件 IPC）。
#[derive(Clone)]
pub(crate) struct AdcPanelView {
    pub(crate) plugin_id: String,
    pub(crate) title: String,
    pub(crate) trial: bool,
    /// 有活动试用实例时才绑定到对应的工具调用事件。
    pub(crate) trial_call_seq: Option<u64>,
    /// 面板声明的 view 名（`ui.action` 回传时用于定位插件状态）。
    pub(crate) view: String,
    /// `render_panel` 成功时的元素树；失败为 `None`，错误在 `error`。
    pub(crate) tree: Option<serde_json::Value>,
    /// 渲染失败的可读错误（未知 view / 插件不实现 view / 进程错误）。
    pub(crate) error: Option<String>,
}

pub(crate) type ParsedAgentPresets = Result<dshr_profile::loader::AgentPresets, String>;

pub(crate) struct ChatState {
    pub(crate) log: SessionLog,
    pub(crate) path: PathBuf,
    pub(crate) workspace: PathBuf,
    pub(crate) host: Option<SidecarHost>,
    pub(crate) workspace_sidecars: HashMap<PathBuf, SidecarHost>,
    pub(crate) preset_diagnostics: Vec<String>,
    pub(crate) active_preset_error: Option<String>,
    pub(crate) default_preset: Option<String>,
    pub(crate) inherited_preset_from: Option<String>,
    pub(crate) preset_inputs: Option<(PathBuf, Vec<PathBuf>)>,
    pub(crate) published_presets: Option<ParsedAgentPresets>,
    pub(crate) sidecar_start_error: Option<String>,
    pub(crate) plugins: PluginHost,
    /// Agent 在窗口内复用，会话上下文由当前日志重建。
    pub(crate) agent: Agent<Box<dyn ModelProvider>>,
    pub(crate) settings: DesktopSettings,
    pub(crate) permission: LivePermission,
    pub(crate) seq: u64,
    /// 审批 broker：工作线程阻塞等待 UI 决定；不属于大锁保护范围。
    pub(crate) approvals: Arc<ApprovalBroker>,
    /// 上下文外部化存储：超阈值工具结果落盘为内容寻址对象（内容只在磁盘，模型见引用）。
    pub(crate) spill: SpillStore,
    /// 当前会话 id（会话树事件/子会话父链接使用）。
    pub(crate) session_id: String,
    /// 会话目录（子会话文件与导出都基于它）。
    pub(crate) sessions_dir: PathBuf,
    /// L2 Host Service 的会话读写面（None = 该会话不接 sidecar 服务）。
    pub(crate) l2_access: Option<Arc<ChatSessionAccess>>,
    /// 与 provider 共享的停止句柄（子会话继承，用户停止会传播）。
    pub(crate) cancel: Arc<AtomicBool>,
    /// 是否允许派生子会话（子会话本身为 false，保证默认单层）。
    pub(crate) allow_spawn: bool,
    pub(crate) external_plugin_errors: Vec<String>,
    pub(crate) session_errors: Vec<String>,
    /// 原会话文件存在但读不出来时置位：禁止 save 覆盖（否则会把读失败当成空会话写回）。
    pub(crate) save_blocked: Option<String>,
    /// 插件 ADC 面板缓存（`workspace.panel` 的 view → 元素树 JSON）。
    pub(crate) adc_panels: Vec<AdcPanelView>,
    pub(crate) headless: bool,
    pub(crate) diagnostics: bool,
    pub(crate) creator_trials: HashMap<String, creator::TrialScope>,
}

pub(crate) enum TurnUpdate<'a> {
    Preview(&'a SessionLog, Option<&'a str>, Option<&'a str>),
    Call { seq: u64, tool: &'a str },
    Result { seq: u64, status: &'static str },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnReason {
    Completed,
    Error,
    Cancelled,
}

impl TurnReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

pub(crate) struct TurnResult {
    pub(crate) reply: String,
    pub(crate) reason: TurnReason,
}

pub(crate) enum ChatRuntime {
    Desktop {
        allow_spawn: bool,
        sidecar: bool,
        diagnostics: bool,
    },
    Headless,
}

impl ChatState {
    pub(crate) fn open_diagnostics(
        path: PathBuf,
        session_id: &str,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
    ) -> Self {
        Self::open_diagnostics_with_options(path, session_id, cancel, approvals, true, true)
    }

    /// Opens a chat for explicit diagnostics and self-tests.
    pub(crate) fn open_diagnostics_with_options(
        path: PathBuf,
        session_id: &str,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
        allow_spawn: bool,
        with_sidecar: bool,
    ) -> Self {
        Self::open_in(
            path,
            session_id,
            cancel,
            approvals,
            DesktopSettings::from_env(&configured_models()),
            ChatRuntime::Desktop {
                allow_spawn,
                sidecar: with_sidecar,
                diagnostics: true,
            },
        )
    }

    pub(crate) fn open_with_settings(
        path: PathBuf,
        session_id: &str,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
        allow_spawn: bool,
        with_sidecar: bool,
        settings: DesktopSettings,
    ) -> Self {
        Self::open_in(
            path,
            session_id,
            cancel,
            approvals,
            settings,
            ChatRuntime::Desktop {
                allow_spawn,
                sidecar: with_sidecar,
                diagnostics: false,
            },
        )
    }

    pub(crate) fn open_in(
        path: PathBuf,
        session_id: &str,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
        settings: DesktopSettings,
        runtime: ChatRuntime,
    ) -> Self {
        let OpenedSessionLog {
            log,
            errors: session_errors,
            save_blocked,
            ..
        } = open_session_log(&path, session_id);
        let workspace = session_workspace(&log);
        let mut state = Self::from_log(path, log, workspace, cancel, approvals, settings, runtime);
        state.session_errors = session_errors;
        state.save_blocked = save_blocked;
        state
    }

    pub(crate) fn from_log(
        path: PathBuf,
        log: SessionLog,
        workspace: PathBuf,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
        settings: DesktopSettings,
        runtime: ChatRuntime,
    ) -> Self {
        Self::from_log_in(
            path, log, workspace, cancel, approvals, settings, runtime, None,
        )
    }

    pub(crate) fn from_log_in(
        path: PathBuf,
        log: SessionLog,
        workspace: PathBuf,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
        settings: DesktopSettings,
        runtime: ChatRuntime,
        inherited: Option<(Arc<dsh_subagents::ChildSupervisor>, LivePermission)>,
    ) -> Self {
        let (headless, allow_spawn, with_sidecar, diagnostics) = match runtime {
            ChatRuntime::Desktop {
                allow_spawn,
                sidecar,
                diagnostics,
            } => (false, allow_spawn, sidecar, diagnostics),
            ChatRuntime::Headless => (true, false, true, false),
        };
        let sessions_dir = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        // L2 会话读写面在 spawn 之前准备好：Router 注册时需要它（chat.rs 只负责构造/注入）。
        let l2_access = if with_sidecar {
            Some(match &inherited {
                Some((children, _)) => {
                    ChatSessionAccess::with_children(sessions_dir.clone(), children.clone())
                }
                None => ChatSessionAccess::new(sessions_dir.clone()),
            })
        } else {
            None
        };
        let permission = inherited
            .map(|(_, permission)| permission)
            .unwrap_or_else(|| LivePermission::new(settings.permission));
        let (host, sidecar_start_error) = if with_sidecar {
            match spawn_sidecar_in(
                &workspace,
                (!headless || permission.can_write()).then(|| approvals.clone()),
                (!headless).then(|| (permission.clone(), cancel.clone())),
                l2_access.clone(),
                diagnostics,
            ) {
                Ok(host) => (Some(host), None),
                Err(error) => {
                    eprintln!("[chat] {error}");
                    (None, Some(error))
                }
            }
        } else {
            (None, None)
        };
        if let Some(mode) = settings.permission {
            eprintln!("[chat] approval mode={}", mode.as_str());
        }
        let mut plugins = PluginHost::new(permission.clone());
        if diagnostics {
            if let Err(err) = plugins.load(Box::new(ProbeNativePlugin)) {
                eprintln!("[chat] 诊断插件加载失败: {err}");
            }
            register_probe_write(&mut plugins, &workspace);
        }
        if let Some(sidecar) = &host {
            register_sidecar_tools(&mut plugins, sidecar, headless);
        }
        let seq = last_message_seq(&log);
        let session_id = log.session_id().to_owned();
        let mut state = Self {
            log,
            path: path.clone(),
            workspace,
            host,
            workspace_sidecars: HashMap::new(),
            preset_diagnostics: Vec::new(),
            active_preset_error: None,
            default_preset: None,
            inherited_preset_from: None,
            preset_inputs: loader_inputs(),
            published_presets: None,
            sidecar_start_error,
            plugins,
            agent: Agent::new(settings.model.provider(cancel, settings.reasoning)),
            settings,
            permission,
            seq,
            approvals,
            spill: SpillStore::new(spill_dir_for(&path)),
            session_id,
            sessions_dir,
            l2_access,
            cancel: cancel.clone(),
            allow_spawn,
            external_plugin_errors: Vec::new(),
            session_errors: Vec::new(),
            save_blocked: None,
            adc_panels: Vec::new(),
            headless,
            diagnostics,
            creator_trials: HashMap::new(),
        };
        if !headless {
            state.refresh_agent_presets();
            state.refresh_adc_panels();
        }
        state
    }

    /// 在同一个会话目录内切换日志，复用窗口持有的工具运行时。
    /// 返回是否已新建或恢复日志，供侧栏刷新索引。
    pub(crate) fn switch_session(&mut self, path: PathBuf, session_id: &str) -> bool {
        let opened = open_session_log(&path, session_id);
        if let Some(access) = &self.l2_access {
            access.clear_snapshot();
        }
        self.seq = last_message_seq(&opened.log);
        self.log = opened.log;
        let workspace = session_workspace(&self.log);
        if workspace != self.workspace {
            let previous = std::mem::replace(&mut self.workspace, workspace);
            if self.diagnostics {
                let _ = self.plugins.unload("builtin.probe-write");
                register_probe_write(&mut self.plugins, &self.workspace);
            }
            if self.l2_access.is_some() {
                if let Some(mut host) = self.host.take() {
                    self.plugins.invalidate_generation(host.generation());
                    if !host.has_exited() {
                        self.workspace_sidecars.insert(previous, host);
                    }
                }
                let cached = self.workspace_sidecars.remove(&self.workspace);
                if let Some(mut host) = cached {
                    if !host.has_exited() {
                        self.attach_sidecar(host);
                    }
                }
                if self.host.is_none() {
                    self.start_sidecar();
                }
            }
        }
        self.spill = SpillStore::new(spill_dir_for(&path));
        self.path = path;
        self.session_id = session_id.to_owned();
        self.session_errors = opened.errors;
        self.save_blocked = opened.save_blocked;
        if !self.headless {
            self.refresh_agent_presets();
            self.refresh_adc_panels();
        }
        opened.wrote_created || opened.recovered
    }

    pub(crate) fn refresh_agent_presets(&mut self) {
        if self.headless {
            return;
        }
        let catalog = read_agent_presets_from(self.preset_inputs.as_ref());
        self.publish_agent_presets(catalog);
        self.bind_agent_preset();
    }

    pub(crate) fn publish_agent_presets(&mut self, parsed: ParsedAgentPresets) {
        self.preset_diagnostics.clear();
        let catalog = match &parsed {
            Ok(catalog) => catalog,
            Err(error) => {
                self.preset_diagnostics
                    .push(format!("预设配置读取失败：{error}"));
                self.published_presets = Some(parsed);
                return;
            }
        };
        let definitions = json!(catalog.definitions);
        let mut applied = true;
        let hosts = self
            .host
            .iter_mut()
            .chain(self.workspace_sidecars.values_mut());
        for host in hosts.filter(|host| host.supports_agent_presets()) {
            if host.has_exited() {
                continue;
            }
            match host.sync_presets(&definitions) {
                Ok(status) => {
                    self.default_preset.clone_from(&catalog.default);
                    if let Some(rows) = status.as_object() {
                        for (id, row) in rows {
                            if let Some(error) = row["error"].as_str() {
                                let message = format!("预设 {id}: {error}");
                                if !self.preset_diagnostics.contains(&message) {
                                    self.preset_diagnostics.push(message);
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    applied = false;
                    self.preset_diagnostics.push(error.to_string());
                }
            }
        }
        self.published_presets = applied.then_some(parsed);
    }

    pub(crate) fn bind_agent_preset(&mut self) {
        self.active_preset_error = None;
        let id = self
            .log
            .created()
            .and_then(|created| created.preset.as_deref())
            .filter(|id| *id != creator::PRESET);
        if let Some(host) = self
            .host
            .as_mut()
            .filter(|host| host.supports_agent_presets())
        {
            // A blank-session change can update the stored preset ID.
            // An unchanged ID keeps its existing revision across reloads.
            if host.bound_preset(&self.session_id) != id {
                if let Err(error) = host.release_preset(&self.session_id) {
                    self.active_preset_error = Some(format!("无法释放原预设：{error}"));
                    return;
                }
            }
            if let Some(id) = id {
                if let Err(error) =
                    host.bind_preset(&self.session_id, id, self.inherited_preset_from.as_deref())
                {
                    self.active_preset_error = Some(format!("无法启用预设 {id}：{error}"));
                }
            }
        } else if let Some(id) = id {
            self.active_preset_error = Some(format!("预设 {id} 需要支持 Agent 预设的官方 Sidecar"));
        }
    }

    pub(crate) fn select_blank_preset(&mut self, preset: Option<String>) -> Result<(), String> {
        let _writer = self.lock_and_refresh()?;
        let created =
            blank_root_created(&self.log).ok_or_else(|| "只能在空白的根会话更改模式".to_owned())?;
        if created.preset == preset {
            return Ok(());
        }
        let created = created.clone().with_optional_preset(preset);
        self.publish_agent_presets(read_agent_presets_from(self.preset_inputs.as_ref()));
        if let Some(preset) = created
            .preset
            .as_deref()
            .filter(|id| *id != creator::PRESET)
        {
            let host = self
                .host
                .as_mut()
                .filter(|host| host.supports_agent_presets())
                .ok_or_else(|| "此模式需要支持 Agent 预设的官方 Sidecar".to_owned())?;
            if host.has_exited() {
                return Err("官方 Sidecar 已退出，无法改选模式".to_owned());
            }
            let status = host
                .preset_status()
                .get(preset)
                .ok_or_else(|| format!("预设 {preset} 不可用"))?;
            if let Some(error) = status["error"].as_str() {
                return Err(format!("无法启用预设 {preset}：{error}"));
            }
        }
        let mut updated = SessionLog::new(&self.session_id);
        updated
            .append_created(&self.log.events()[0].ts, &created)
            .map_err(|error| error.to_string())?;
        save_atomic(&self.path, updated.to_jsonl().as_bytes())
            .map_err(|error| format!("无法保存会话模式：{error}"))?;
        self.log = updated;
        self.creator_trials.remove(&self.session_id);
        self.bind_agent_preset();
        self.refresh_adc_panels();
        match &self.active_preset_error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    pub(crate) fn load_errors(&self) -> Vec<String> {
        self.host
            .iter()
            .flat_map(|host| host.boot_errors())
            .chain(&self.sidecar_start_error)
            .chain(&self.active_preset_error)
            .chain(&self.preset_diagnostics)
            .chain(&self.external_plugin_errors)
            .chain(&self.session_errors)
            .cloned()
            .collect()
    }

    /// 刷新 `workspace.panel` 的 ADC 视图缓存（装载/重载插件后调用一次）。
    ///
    /// 渲染路径（`plugins_panel`）只读缓存：插件 IPC 不进入每帧渲染，避免 UI 线程
    /// 等外部进程（`ui.render` 超时默认 10s）。未知 view / 不实现 view 的插件只留下
    /// 可读错误，不影响其它面板。
    pub(crate) fn refresh_adc_panels(&mut self) {
        let mut panels = std::collections::BTreeMap::new();
        if let Some(scope) = self.creator_trials.get_mut(&self.session_id) {
            let calls = creator::trial_panel_calls(&self.log);
            Self::render_adc_panels(&mut scope.plugins, Some(&calls), &mut panels);
        }
        Self::render_adc_panels(&mut self.plugins, None, &mut panels);
        self.adc_panels = panels.into_values().collect();
    }

    pub(crate) fn apply_adc_action(
        &mut self,
        plugin_id: &str,
        view: &str,
        action: &str,
        payload: &serde_json::Value,
    ) -> Result<(), dshr_plugin_host::HostError> {
        let plugins = match self.creator_trials.get_mut(&self.session_id) {
            Some(scope)
                if scope.plugins.panels().iter().any(|registration| {
                    registration.plugin_id == plugin_id
                        && registration.panel.slot == "workspace.panel"
                        && registration.panel.view_name() == view
                }) =>
            {
                &mut scope.plugins
            }
            _ => &mut self.plugins,
        };
        plugins.dispatch_action(plugin_id, view, action, payload)?;
        self.refresh_adc_panels();
        Ok(())
    }

    pub(crate) fn render_adc_panels(
        plugins: &mut PluginHost,
        trial_calls: Option<&HashMap<&str, u64>>,
        panels: &mut std::collections::BTreeMap<(String, String), AdcPanelView>,
    ) {
        let registrations: Vec<_> = plugins
            .panels()
            .into_iter()
            .filter(|registration| registration.panel.slot == "workspace.panel")
            .map(|registration| {
                (
                    registration.plugin_id.clone(),
                    registration.panel.title.clone(),
                    registration.panel.view_name().to_owned(),
                )
            })
            .collect();
        for (plugin_id, title, view) in registrations {
            let trial_call_seq =
                trial_calls.and_then(|calls| calls.get(plugin_id.as_str()).copied());
            panels
                .entry((plugin_id.clone(), view.clone()))
                .or_insert_with(|| {
                    let (tree, error) = match plugins.render_panel(&plugin_id, &view) {
                        Ok(tree) => (Some(tree), None),
                        Err(error) => (None, Some(format!("view `{view}`: {error}"))),
                    };
                    AdcPanelView {
                        plugin_id,
                        title,
                        trial: trial_calls.is_some(),
                        trial_call_seq,
                        view,
                        tree,
                        error,
                    }
                });
        }
    }

    pub(crate) fn drop_session_scope(&mut self, session_id: &str) {
        for host in self
            .host
            .iter_mut()
            .chain(self.workspace_sidecars.values_mut())
        {
            if let Err(error) = host.retire_session(session_id) {
                eprintln!("[chat] retire sidecar session {session_id}: {error}");
            }
        }
        if self.creator_trials.remove(session_id).is_some() && self.session_id == session_id {
            self.refresh_adc_panels();
        }
    }

    /// 组装上下文计划：先按阈值把超大工具结果外部化（写对象 + 事件留痕），再重新计划。
    /// 外部化后模型只看到"引用 + 预览"，原始内容可从内容寻址对象无损取回。
    pub(crate) fn plan_with_spill(&mut self) -> ContextPlan {
        let policy = SpillPolicy::from_env();
        let mut plan = plan_context_with_spill(&self.log, "", &policy);
        if plan.spilled.is_empty() {
            return plan;
        }
        let ts = now_ts();
        let mut recorded = 0usize;
        for pending in &plan.spilled {
            match self.spill.put_pending(pending) {
                Ok(_) => {
                    if dshr_agent_context::spill::record_spill(&mut self.log, pending, &ts).is_ok()
                    {
                        recorded += 1;
                    }
                }
                Err(err) => eprintln!("[chat] 外部化工具结果失败: {err}"),
            }
        }
        if recorded > 0 {
            self.save();
            plan = plan_context_with_spill(&self.log, "", &policy);
        }
        plan
    }

    /// 模拟 Sidecar 崩溃：销毁进程 + 撤销该 generation 的全部工具。
    pub(crate) fn kill_sidecar(&mut self) -> Option<String> {
        let host = self.host.take()?;
        let generation = host.generation().to_owned();
        drop(host);
        self.plugins.invalidate_generation(&generation);
        Some(generation)
    }

    /// 重启 Sidecar：先撤销旧 generation，再按当前 profile/环境重新装载。
    pub(crate) fn restart_sidecar(&mut self) -> bool {
        self.workspace_sidecars.clear();
        self.restart_active_sidecar()
    }

    pub(crate) fn restart_active_sidecar(&mut self) -> bool {
        if let Some(old) = self.host.take() {
            self.plugins.invalidate_generation(old.generation());
        }
        self.start_sidecar()
    }

    pub(crate) fn start_sidecar(&mut self) -> bool {
        let host = match spawn_sidecar_in(
            &self.workspace,
            (!self.headless || self.permission.can_write()).then(|| self.approvals.clone()),
            (!self.headless).then(|| (self.permission.clone(), self.cancel.clone())),
            self.l2_access.clone(),
            self.diagnostics,
        ) {
            Ok(host) => host,
            Err(error) => {
                eprintln!("[chat] {error}");
                self.sidecar_start_error = Some(error);
                return false;
            }
        };
        self.attach_sidecar(host);
        true
    }

    pub(crate) fn attach_sidecar(&mut self, host: SidecarHost) {
        register_sidecar_tools(&mut self.plugins, &host, self.headless);
        self.host = Some(host);
        self.sidecar_start_error = None;
    }

    pub(crate) fn set_model(
        &mut self,
        model: Arc<DesktopModel>,
        reasoning: Option<ReasoningEffort>,
    ) {
        *self.agent.provider_mut() = model.provider(&self.cancel, reasoning);
        self.settings.model = model;
        self.settings.reasoning = reasoning;
    }

    pub(crate) fn save(&self) {
        if let Err(error) = self.save_checked() {
            eprintln!("[chat] {error}");
        }
    }

    pub(crate) fn save_checked(&self) -> Result<(), String> {
        if let Some(reason) = &self.save_blocked {
            return Err(format!("跳过保存（{reason}）: {}", self.path.display()));
        }
        // 临时文件 + 替换 + 上一版 .bak：崩溃/断电不会留下半截文件；
        // Windows 上 remove→rename 的极短窗口由 recover_backup 兜底。
        save_atomic(&self.path, self.log.to_jsonl().as_bytes())
            .map_err(|error| format!("保存会话 {} 失败：{error}", self.path.display()))
    }

    pub(crate) fn lock_and_refresh(&mut self) -> Result<std::fs::File, String> {
        let writer =
            try_lock_session(&self.path).map_err(|error| format!("无法取得会话写入权：{error}"))?;
        let text = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("读取会话日志失败：{error}"))?;
        let log = SessionLog::from_jsonl(&self.session_id, &text)
            .map_err(|error| format!("解析会话日志失败：{error}"))?;
        self.seq = last_message_seq(&log);
        self.log = log;
        self.save_blocked = None;
        Ok(writer)
    }
}
