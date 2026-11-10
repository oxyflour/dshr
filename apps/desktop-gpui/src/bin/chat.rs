//! 基本对话：输入框 + 会话流 + 工具调用 + 落盘恢复 + 真流式。
//!
//! 这是 M1 的产品面：能打字、能发送、能看到工具卡片、关掉再开会话还在。
//! 模型在无 key 时用 mock，有 key 时用 DeepSeek（SSE 流式）；会话/工具/日志/回放都是真实的。
//!
//! 线程模型：Agent 与工具在工作线程跑，UI 线程只消费"整份快照"——
//! 网络等待不会卡住界面，这正是把 `ModelProvider` 做成流式的目的。
//!
//! 四种模式：
//!   cargo run -p dshr-desktop-gpui --bin chat                 # 交互界面
//!   cargo run ... --bin chat -- --selftest                   # 无窗口自检
//!   cargo run ... --bin chat -- --selftest-stream            # 流式自检
//!   cargo run ... --bin chat -- --selftest-dsh               # 官方 DSH 插件自检
//!   cargo run ... --bin chat -- --live "一句话"               # 真实 LLM 单轮（需 API key）

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, px, size, AnyElement, App, Bounds, Context, Entity, Focusable, FontWeight, ScrollHandle,
    Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use serde_json::json;

use dshr_agent_context::history_for_model;
use dshr_agent_core::{
    Agent, AgentEvent, MockProvider, ModelProvider, ToolCallRequest, ToolExecutor, ToolOutcome,
    ToolSpec,
};
use dshr_compat_dsh_host::SidecarHost;
use dshr_conversation_model::{build_from_log, CardKind, Item, ToolCard};
use dshr_llm_deepseek::DeepSeekProvider;
use dshr_plugin_api::{Plugin, PluginContext, PluginError};
use dshr_plugin_host::PluginHost;
use dshr_session_store::{EventSpec, SessionLog, ToolState};
use dshr_tool_runtime::{AllowAll, ToolDefinition, ToolOrigin};
use dshr_ui_gpui::{TextInput, Theme, ThemeKind};

const SESSION_ID: &str = "s-chat";

fn now_ts() -> String {
    // 简化：用单调计数占位，真实实现应取系统时间并格式化 RFC3339。
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("t{nanos}")
}

struct ChatState {
    log: SessionLog,
    path: PathBuf,
    host: Option<SidecarHost>,
    plugins: PluginHost,
    /// 必须跨轮复用：turn id 单调递增，否则 call_id 重复会被 Session Log 拒收。
    agent: Agent<Box<dyn ModelProvider>>,
    seq: u64,
}
/// 一个最小的 Rust 原生插件：注册 `probe_native` 工具 + 一个 Panel。
struct ProbeNativePlugin;

impl Plugin for ProbeNativePlugin {
    fn id(&self) -> &'static str {
        "builtin.probe"
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "probe_native",
                "把输入文本转成大写（Rust 原生插件，不经过 Node）",
                ToolOrigin::Native {
                    plugin_id: "builtin.probe".to_owned(),
                },
            )
            .with_schema(json!({
                "type": "object",
                "required": ["text"],
                "properties": { "text": { "type": "string" } }
            }))
            .with_presentation("generic"),
        )?;
        ctx.register_panel(dshr_plugin_api::PanelContribution::new(
            "probe.panel",
            "Probe Panel",
            "workspace.panel",
        ));
        Ok(())
    }

    fn call(
        &mut self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        if tool != "probe_native" {
            return Err(PluginError::UnknownTool {
                plugin: "builtin.probe".to_owned(),
                tool: tool.to_owned(),
            });
        }
        let text = arguments
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        Ok(json!({ "native_echo": text.to_uppercase() }))
    }
}

/// 启动 Sidecar；失败时返回 None（Native 工具不受影响）。
///
/// 三种运行时（`DSHR_SIDECAR`）：
/// - 缺省：probe 侧车（自研最小协议探针）；
/// - `cordis`：真实 Cordis 4.0.5 + 只注册 `ctx.bridge.tool` 的探针插件；
/// - `dsh`：真实 Cordis 4.1.0 + **未修改的官方 DSH 插件**（依赖由 `tools/dsh-vendor.mjs`
///   从本机 DSH 安装中抽取，见 `docs/m2-dsh-official.md`）。
fn spawn_sidecar() -> Option<SidecarHost> {
    let root = std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .unwrap_or_else(|_| PathBuf::from("."));
    let mode = std::env::var("DSHR_SIDECAR").unwrap_or_default();
    let (script, plugin) = match mode.as_str() {
        "cordis" => (
            root.join("sidecar/cordis/sidecar-cordis.mjs"),
            root.join("sidecar/cordis/bridge-plugin.mjs"),
        ),
        "dsh" => {
            let dir = root.join("sidecar/dsh-official");
            if !dir.join("node_modules").exists() {
                eprintln!(
                    "[chat] 官方插件运行时未准备：先运行 `node tools/dsh-vendor.mjs`（需要本机安装 DSH）"
                );
                return None;
            }
            let script = dir.join("sidecar-dsh.mjs");
            // 官方侧车不需要插件路径参数，但协议签名保持两个文件参数。
            (script.clone(), script)
        }
        _ => (
            root.join("sidecar/probe-sidecar.mjs"),
            root.join("sidecar/probe-plugin.mjs"),
        ),
    };
    SidecarHost::spawn(script, plugin)
        .map_err(|err| eprintln!("[chat] sidecar 不可用（工具调用会显示为 aborted）: {err}"))
        .ok()
}

/// 有 DEEPSEEK_API_KEY（环境变量或 `~/.dsh/.env`）时用真实模型，否则退回 mock。
fn select_provider(cancel: &Arc<AtomicBool>) -> Box<dyn ModelProvider> {
    // 自检必须确定性：显式要求 mock 时不看环境里的 key。
    if std::env::var("DSHR_FORCE_MOCK").as_deref() == Ok("1") {
        println!("[chat] provider=mock（DSHR_FORCE_MOCK=1）");
        return Box::new(MockProvider);
    }
    match DeepSeekProvider::from_env() {
        Some(provider) => {
            println!("[chat] provider=deepseek model={}", provider.model());
            Box::new(provider.with_cancel(cancel.clone()))
        }
        None => {
            println!("[chat] provider=mock（未找到 DEEPSEEK_API_KEY）");
            Box::new(MockProvider)
        }
    }
}

/// Agent 的工具执行器：authorize 之后按来源分发给 PluginHost（native）或 Sidecar。
struct ChatExecutor<'a> {
    plugins: &'a mut PluginHost,
    host: Option<&'a mut SidecarHost>,
}

impl ToolExecutor for ChatExecutor<'_> {
    fn specs(&self) -> Vec<ToolSpec> {
        self.plugins
            .registry()
            .list()
            .iter()
            .map(|definition| ToolSpec {
                name: definition.name.clone(),
                description: definition.description.clone(),
                parameters: definition.input_schema.clone(),
            })
            .collect()
    }

    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        let definition = match self
            .plugins
            .registry()
            .authorize(&call.tool, &call.arguments)
        {
            Ok(definition) => definition.clone(),
            Err(err) => return ToolOutcome::failure(err.to_string()),
        };

        match definition.origin {
            ToolOrigin::Native { .. } => match self.plugins.call(&call.tool, &call.arguments) {
                Ok(value) => {
                    ToolOutcome::success(json!([{ "type": "json", "value": value }]), value)
                }
                Err(err) => ToolOutcome::failure(err.to_string()),
            },
            ToolOrigin::Sidecar { .. } => {
                let Some(host) = self.host.as_mut() else {
                    return ToolOutcome::failure("sidecar unavailable");
                };
                match host.invoke_as(
                    &call.call_id,
                    &call.tool,
                    call.arguments.clone(),
                    Duration::from_secs(5),
                ) {
                    Ok(outcome) if outcome.terminal.kind == "tool.result" => {
                        let payload = &outcome.terminal.payload;
                        let content = payload["content"].clone();
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
                    Ok(outcome) => ToolOutcome::failure(outcome.terminal.payload.to_string()),
                    Err(err) => ToolOutcome::failure(err.to_string()),
                }
            }
            ToolOrigin::Mcp { .. } => ToolOutcome::failure("mcp not wired"),
        }
    }
}

/// 把 sidecar 注册映射进同一个 ToolRegistry。
fn register_sidecar_tools(plugins: &mut PluginHost, host: &SidecarHost) {
    for definition in host.tool_definitions() {
        let name = definition.name.clone();
        if let Err(err) = plugins.registry_mut().register(definition) {
            eprintln!("[chat] 注册 sidecar 工具 `{name}` 失败: {err}");
        }
    }
}

impl ChatState {
    fn new(path: PathBuf, cancel: &Arc<AtomicBool>) -> Self {
        let log = if path.exists() {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            SessionLog::from_jsonl(SESSION_ID, &text).unwrap_or_else(|err| {
                eprintln!("[chat] 无法加载历史会话（忽略并从空会话开始）: {err}");
                SessionLog::new(SESSION_ID)
            })
        } else {
            SessionLog::new(SESSION_ID)
        };

        let host = spawn_sidecar();
        let mut plugins = PluginHost::new(AllowAll);
        if let Err(err) = plugins.load(Box::new(ProbeNativePlugin)) {
            eprintln!("[chat] 原生插件加载失败: {err}");
        }
        if let Some(sidecar) = &host {
            register_sidecar_tools(&mut plugins, sidecar);
        }

        Self {
            log,
            path,
            host,
            plugins,
            agent: Agent::new(select_provider(cancel)),
            seq: 0,
        }
    }

    /// 模拟 Sidecar 崩溃：销毁进程 + 撤销该 generation 的全部工具。
    fn kill_sidecar(&mut self) -> Option<String> {
        let host = self.host.take()?;
        let generation = host.generation().to_owned();
        drop(host);
        self.plugins.invalidate_generation(&generation);
        Some(generation)
    }

    /// 重启 Sidecar：新 generation，重新注册工具。
    fn restart_sidecar(&mut self) -> bool {
        let Some(host) = spawn_sidecar() else {
            return false;
        };
        register_sidecar_tools(&mut self.plugins, &host);
        self.host = Some(host);
        true
    }

    fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&self.path, self.log.to_jsonl()) {
            eprintln!("[chat] 保存会话失败: {err}");
        }
    }

    /// 跑一轮对话：用户消息 → Agent（模型 + 工具）→ 助手消息落库。
    ///
    /// **每产生一个事件就推一份完整快照**给调用方（UI 在别的线程画），
    /// 所以网络等待不会阻塞界面，流式文本也是真的逐段到达。
    ///
    /// 同步场景（自检、`--live`）传一个空 sink 即可。
    fn stream_turn(&mut self, text: &str, emit: &mut dyn FnMut(Vec<Row>)) -> Option<String> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }

        let ts = now_ts();
        self.seq += 1;
        let user_id = format!("m-u{}", self.seq);
        if let Err(err) = self.log.append(EventSpec::durable(
            "message.user",
            &ts,
            json!({ "message_id": user_id, "text": text }),
        )) {
            eprintln!("[chat] 写入用户消息失败: {err}");
            return None;
        }
        self.save();
        emit(rows_snapshot(&self.log, None));

        // 历史从 durable 事件重建：多轮记忆靠它，重开应用也能接上。
        // 截断在 `agent-context` 里做，不会拆散 assistant.tool_calls/tool 配对。
        let history = history_for_model(&self.log);

        // 来源与卡片类型在轮次开始前固定：流式回调里不能再借注册表（执行器正持有它）。
        let presentations: std::collections::BTreeMap<String, (String, String)> = self
            .plugins
            .registry()
            .list()
            .iter()
            .map(|definition| {
                (
                    definition.name.clone(),
                    (
                        definition.origin.kind().to_owned(),
                        definition
                            .presentation
                            .clone()
                            .unwrap_or_else(|| "generic".to_owned()),
                    ),
                )
            })
            .collect();

        let mut reply = String::new();
        {
            let Self {
                agent,
                plugins,
                host,
                log,
                ..
            } = self;
            let mut executor = ChatExecutor {
                plugins,
                host: host.as_mut(),
            };
            agent.run_turn_streaming(&history, text, &mut executor, &mut |event| match event {
                AgentEvent::TextDelta(delta) => {
                    reply.push_str(delta);
                    emit(rows_snapshot(log, Some(&reply)));
                }
                AgentEvent::ToolCall(call) => {
                    let (origin, presentation) = presentations
                        .get(&call.tool)
                        .cloned()
                        .unwrap_or_else(|| ("unknown".to_owned(), "generic".to_owned()));
                    let _ = log.append(EventSpec::durable(
                        "tool.call",
                        &ts,
                        json!({
                            "call_id": call.call_id,
                            "tool": call.tool,
                            "origin": origin,
                            // 展示元数据随事件持久化（spec/session-log.md §6）：
                            // 回放不依赖插件当前版本。
                            "presentation": presentation,
                            "arguments": call.arguments
                        }),
                    ));
                    emit(rows_snapshot(log, Some(&reply)));
                }
                AgentEvent::ToolResult { call_id, outcome } => {
                    if outcome.is_error {
                        let _ = log.append(EventSpec::durable(
                            "tool.aborted",
                            &ts,
                            json!({ "call_id": call_id, "reason": outcome.content.to_string() }),
                        ));
                    } else {
                        let _ = log.append(EventSpec::durable(
                            "tool.result",
                            &ts,
                            json!({
                                "call_id": call_id,
                                "is_error": false,
                                // 对齐 DSH：content 给模型，value/meta 给展示与回放。
                                "content": outcome.content,
                                "value": outcome.value,
                                "meta": outcome.meta,
                            }),
                        ));
                    }
                    emit(rows_snapshot(log, Some(&reply)));
                }
                AgentEvent::TurnFailed(err) => {
                    reply.push_str(&format!("处理失败：{err}"));
                    emit(rows_snapshot(log, Some(&reply)));
                }
                AgentEvent::TurnStarted { .. }
                | AgentEvent::StepStarted { .. }
                | AgentEvent::TurnEnded { .. } => {}
            });
        }

        self.seq += 1;
        let ts = now_ts();
        let _ = self.log.append(EventSpec::durable(
            "message.assistant.settled",
            &ts,
            json!({ "message_id": format!("m-a{}", self.seq), "text": reply }),
        ));
        self.save();
        emit(rows_snapshot(&self.log, None));
        Some(reply)
    }

    /// 同步版本（自检与无 UI 场景使用）：丢弃 UI 快照。
    fn submit(&mut self, text: &str) {
        let _ = self.stream_turn(text, &mut |_| {});
    }

    fn items(&self) -> Vec<Item> {
        build_from_log(&self.log)
    }
}

struct ChatApp {
    /// Agent / 工具 / 日志都在工作线程里改；UI 只读快照，所以这里必须加锁。
    state: Arc<Mutex<ChatState>>,
    /// 当前渲染用的整份快照（由工作线程推送）。
    rows: Vec<Row>,
    /// 真正的文本输入组件（实现 EntityInputHandler，中文 IME 组合输入可用）。
    input: Entity<TextInput>,
    scroll: ScrollHandle,
    /// 一轮正在生成：输入框仍可编辑，但回车不会发第二条。
    busy: bool,
    /// 与 provider 共享的停止句柄（UI 不持锁就能请求中断）。
    cancel: Arc<AtomicBool>,
    /// DSH 设计 token（浅色/深色）。
    theme_kind: ThemeKind,
    /// 左侧会话栏收起状态（对齐 DSH 的折叠侧栏）。
    sidebar_collapsed: bool,
    /// 首次渲染后把历史滚到最新一条（DSH 打开会话也是停在底部）。
    initial_scroll_done: bool,
}

impl ChatApp {
    /// 请求停止生成：provider 在下一个 SSE 行边界返回已生成的部分。
    fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            return;
        }
        self.cancel.store(true, Ordering::Relaxed);
        cx.notify();
    }

    /// 回车提交（TextInput 的 Submit action）。
    ///
    /// 返回 `true` 表示已受理，输入框随后自行清空；生成中返回 `false`，内容保留。
    fn on_submit(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        if self.busy || text.trim().is_empty() {
            return false;
        }
        self.start_turn(text.to_owned(), cx);
        true
    }

    /// 发送按钮：空闲时发送，生成中变成"停止"。
    fn button_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            self.stop(cx);
            return;
        }
        let text = self.input.read(cx).content().to_owned();
        if text.trim().is_empty() {
            return;
        }
        self.input.update(cx, |input, cx| input.clear(window, cx));
        self.start_turn(text, cx);
    }

    fn start_turn(&mut self, text: String, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        cx.notify();

        // 模型调用与工具执行都在工作线程：UI 线程不允许被网络阻塞。
        // 事件 → 快照 → channel → UI 合并，UI 侧没有事件重放逻辑。
        let state = self.state.clone();
        let (tx, rx) = mpsc::channel::<Vec<Row>>();
        std::thread::spawn(move || {
            let mut guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.stream_turn(&text, &mut |rows| {
                // UI 已关闭时发送失败：忽略并继续，不要 panic。
                let _ = tx.send(rows);
            });
        });

        cx.spawn(async move |this, cx| {
            loop {
                // 同一轮询窗口内只取最后一份快照：快照是全量的，旧的不需要画。
                let mut latest = None;
                let mut closed = false;
                loop {
                    match rx.try_recv() {
                        Ok(rows) => latest = Some(rows),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            closed = true;
                            break;
                        }
                    }
                }
                if let Some(rows) = latest {
                    let ok = this
                        .update(cx, |app, cx| {
                            app.rows = rows;
                            // 生成过程中始终跟随最新一行；结束后停止强制滚动，
                            // 用户往上翻历史不会被拽回去。
                            if app.busy {
                                app.scroll.scroll_to_bottom();
                            }
                            cx.notify();
                        })
                        .is_ok();
                    if !ok {
                        return;
                    }
                }
                if closed {
                    let _ = this.update(cx, |app, cx| {
                        app.busy = false;
                        cx.notify();
                    });
                    return;
                }
                // 16ms 轮询：与一帧同量级，肉眼看不到分批。
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
            }
        })
        .detach();
    }

    // ------------------------------------------------------------------
    // DSH Desktop 对齐的界面：侧栏 / 头部 / 对话流 / 输入卡片。
    // 尺寸与颜色都来自 `dshr_ui_gpui::theme`（从 DSH 安装包解析的 token）。
    // ------------------------------------------------------------------

    fn theme(&self) -> Theme {
        Theme::new(self.theme_kind)
    }

    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.theme_kind = self.theme_kind.toggled();
        cx.notify();
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }

    fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn sidebar(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let session_row = div()
            .id("session-row")
            .h(px(36.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .justify_between()
            .rounded(t.radii.md)
            .bg(p.color(p.interactive_hover))
            .child(
                div()
                    .text_size(t.type_scale.body)
                    .line_height(t.type_scale.body_line)
                    .text_color(p.color(p.label_primary))
                    .child("当前会话"),
            )
            .child(
                div()
                    .text_size(t.type_scale.small)
                    .text_color(p.color(p.label_caption))
                    .child(SESSION_ID),
            )
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| {
                this.focus_composer(window, cx);
            }));

        div()
            .w(t.layout.sidebar_width)
            .h_full()
            .flex()
            .flex_col()
            .px(px(8.0))
            .py(px(6.0))
            .bg(p.color(p.sidebar_fill))
            .child(
                // 品牌行：DSH 的 logo 由图形 + 名称组成；这里用首字母方块占位。
                div()
                    .h(px(52.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .w(px(24.0))
                            .h(px(24.0))
                            .rounded(t.radii.sm)
                            .bg(p.color(p.label_primary))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.color(p.bg_base))
                                    .child("D"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .line_height(px(22.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.color(p.label_primary))
                                    .child("DeepSeek Harness"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .line_height(px(16.0))
                                    .text_color(p.color(p.label_caption))
                                    .child("dshr · 本地桌面"),
                            ),
                    ),
            )
            .child(
                // 新建对话：DSH 侧栏顶部的主按钮。
                div()
                    .id("new-session")
                    .mx(px(4.0))
                    .mb(px(10.0))
                    .h(px(38.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.0))
                    .rounded(t.radii.md)
                    .border_1()
                    .border_color(p.color(p.border_l3))
                    .bg(p.color(p.button_elevated))
                    .cursor_pointer()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.color(p.label_primary))
                    .child("＋  新建对话")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.focus_composer(window, cx);
                    })),
            )
            .child(
                div()
                    .px(px(12.0))
                    .pb(px(4.0))
                    .pt(px(6.0))
                    .text_size(t.type_scale.small)
                    .text_color(p.color(p.label_caption))
                    .child("会话"),
            )
            .child(session_row)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .px(px(12.0))
                            .py(px(6.0))
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_tertiary))
                            .child("模型 · deepseek-chat"),
                    )
                    .child(
                        div()
                            .id("theme-toggle-sidebar")
                            .px(px(12.0))
                            .py(px(6.0))
                            .rounded(t.radii.sm)
                            .cursor_pointer()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_secondary))
                            .child(format!("界面 · {}（点击切换）", self.theme_kind.label()))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.toggle_theme(cx);
                            })),
                    )
                    .child(
                        div()
                            .px(px(12.0))
                            .py(px(6.0))
                            .text_size(px(11.0))
                            .text_color(p.color(p.label_caption))
                            .child("M2 · 官方插件已装载"),
                    ),
            )
            .into_any_element()
    }

    fn header(&mut self, t: &Theme, collapsed: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let (dot, status) = if self.busy {
            (p.state_warn, "生成中…")
        } else {
            (p.state_success, "就绪")
        };
        div()
            .min_h(t.layout.header_height)
            .px(px(24.0))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(p.color(p.border_l3))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .id("sidebar-toggle")
                            .w(px(28.0))
                            .h(px(28.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(t.radii.sm)
                            .cursor_pointer()
                            .text_color(p.color(p.label_secondary))
                            .hover(|style| style.bg(p.color(p.interactive_hover)))
                            .child(if collapsed { "≡" } else { "⟨" })
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.toggle_sidebar(cx);
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_size(t.type_scale.body)
                                    .text_color(p.color(p.label_tertiary))
                                    .child("会话"),
                            )
                            .child(
                                div()
                                    .text_size(t.type_scale.body)
                                    .text_color(p.color(p.label_caption))
                                    .child("/"),
                            )
                            .child(
                                div()
                                    .text_size(t.type_scale.body)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.color(p.label_primary))
                                    .child(SESSION_ID),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(p.color(dot)))
                            .child(
                                div()
                                    .text_size(t.type_scale.secondary)
                                    .text_color(p.color(p.label_secondary))
                                    .child(status),
                            ),
                    )
                    .child(
                        div()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_tertiary))
                            .child(format!("{} 条", self.rows.len())),
                    )
                    .child(
                        div()
                            .id("theme-toggle-header")
                            .px(px(10.0))
                            .py(px(4.0))
                            .rounded(t.radii.sm)
                            .border_1()
                            .border_color(p.color(p.border_l2))
                            .bg(p.color(p.button_elevated))
                            .cursor_pointer()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_secondary))
                            .child(format!("切换到{}", self.theme_kind.toggled().label()))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.toggle_theme(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_row(&self, row: &Row, t: &Theme) -> AnyElement {
        let p = t.palette;
        match row {
            Row::User(text) => div()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(t.layout.content_width * 0.702)
                        .px(px(14.0))
                        .py(px(10.0))
                        .rounded(t.radii.lg)
                        .bg(p.color(p.bubble))
                        .text_size(t.type_scale.body)
                        .line_height(t.type_scale.body_line)
                        .text_color(p.color(p.label_primary))
                        .child(text.clone()),
                )
                .into_any_element(),
            Row::Assistant(text) => {
                let streaming = text.ends_with('▌');
                div()
                    .flex()
                    .gap(px(10.0))
                    .child(
                        div()
                            .w(px(20.0))
                            .h(px(20.0))
                            .mt(px(2.0))
                            .rounded(t.radii.xs)
                            .bg(p.color(p.business_tertiary))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.color(p.business_primary))
                                    .child("D"),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(t.type_scale.body)
                                    .line_height(t.type_scale.body_line)
                                    .text_color(p.color(p.label_primary))
                                    .child(text.clone()),
                            )
                            .when(streaming, |el| {
                                el.child(
                                    div()
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(p.label_caption))
                                        .child("正在生成 · 可点击右侧停止"),
                                )
                            }),
                    )
                    .into_any_element()
            }
            Row::Tool {
                name,
                origin,
                status,
                detail,
                is_error,
                pending,
            } => {
                let dot = if *pending {
                    p.state_warn
                } else if *is_error {
                    p.state_error
                } else {
                    p.state_success
                };
                let status_color = if *is_error {
                    p.state_error
                } else {
                    p.label_caption
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(t.radii.sm)
                    .border_1()
                    .border_color(p.color(p.border_l2))
                    .bg(p.color(p.bg_layer_1))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(p.color(dot)))
                            .child(
                                div()
                                    .text_size(t.type_scale.secondary)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.color(p.label_primary))
                                    .child(name.clone()),
                            )
                            .child(
                                div()
                                    .px(px(6.0))
                                    .py(px(1.0))
                                    .rounded(t.radii.xs)
                                    .bg(p.color(p.tag))
                                    .text_size(t.type_scale.small)
                                    .text_color(p.color(p.label_tertiary))
                                    .child(origin.clone()),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(t.type_scale.small)
                                    .text_color(p.color(status_color))
                                    .child(status.clone()),
                            ),
                    )
                    .when(!detail.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(t.type_scale.small)
                                .line_height(t.type_scale.small_line)
                                .text_color(p.color(p.label_tertiary))
                                .child(detail.clone()),
                        )
                    })
                    .into_any_element()
            }
            Row::Meta(text) => div()
                .flex()
                .items_center()
                .justify_center()
                .py(px(4.0))
                .child(
                    div()
                        .px(px(8.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_caption))
                        .child(text.clone()),
                )
                .into_any_element(),
        }
    }

    fn transcript(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let rows = self.rows.clone();
        let mut column = div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(t.layout.content_width)
            .mx_auto()
            .gap(px(16.0))
            .py(px(20.0));
        if rows.is_empty() {
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .py(px(64.0))
                    .child(
                        div()
                            .w(px(48.0))
                            .h(px(48.0))
                            .rounded(t.radii.lg)
                            .bg(p.color(p.label_primary))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .text_size(px(20.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.color(p.bg_base))
                                    .child("D"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(t.type_scale.h2)
                            .line_height(t.type_scale.h2_line)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.color(p.label_primary))
                            .child("开始对话"),
                    )
                    .child(
                        div()
                            .text_size(t.type_scale.body)
                            .text_color(p.color(p.label_tertiary))
                            .child("输入问题，Agent 会调用工具，并把每一步写进本地会话日志。"),
                    ),
            );
        }
        for row in &rows {
            column = column.child(self.render_row(row, t));
        }
        let _ = cx;
        div()
            .id("transcript-scroll")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(px(24.0))
            .child(column)
            .into_any_element()
    }

    fn composer(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let (button_bg, button_label) = if self.busy {
            (p.state_error, "■")
        } else {
            (p.button_info, "↑")
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .px(px(16.0))
            .pb(px(8.0))
            .pt(px(4.0))
            .child(
                div()
                    .w_full()
                    .max_w(t.layout.composer_max_width)
                    .flex()
                    .flex_col()
                    .rounded(t.radii.panel)
                    .border_1()
                    .border_color(p.color(p.border_l2))
                    .bg(p.color(p.input_major))
                    .shadow_sm()
                    .child(
                        div()
                            .px(px(20.0))
                            .pt(px(14.0))
                            .min_h(px(48.0))
                            .flex()
                            .items_center()
                            .text_size(t.type_scale.body)
                            .line_height(t.type_scale.body_line)
                            .text_color(p.color(p.label_primary))
                            .child(self.input.clone()),
                    )
                    .child(
                        div()
                            .px(px(12.0))
                            .pb(px(10.0))
                            .pt(px(2.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .px(px(8.0))
                                            .py(px(2.0))
                                            .rounded(t.radii.sm)
                                            .bg(p.color(p.tag))
                                            .text_size(t.type_scale.small)
                                            .text_color(p.color(p.label_secondary))
                                            .child("deepseek-chat"),
                                    )
                                    .child(
                                        div()
                                            .text_size(t.type_scale.small)
                                            .text_color(p.color(p.label_caption))
                                            .child("本地会话 · 严格工具协议"),
                                    ),
                            )
                            .child(
                                div()
                                    .id("send-button")
                                    .w(px(34.0))
                                    .h(px(34.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(p.color(button_bg))
                                    .text_color(p.color(p.bg_base))
                                    .text_size(px(16.0))
                                    .cursor_pointer()
                                    .child(button_label)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.button_click(window, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .text_size(t.type_scale.small)
                    .text_color(p.color(p.label_caption))
                    .child("回车发送 · 生成中可停止 · 输入法组合输入可用"),
            )
            .into_any_element()
    }
}

/// 会话流里的一行。字段全部是 `String`：快照跨线程从 Agent 线程送到 UI 线程。
///
/// 这是"数据"而不是"样式"：渲染时再按 DSH 的对话样式组合。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    User(String),
    Assistant(String),
    Tool {
        name: String,
        origin: String,
        status: String,
        detail: String,
        is_error: bool,
        pending: bool,
    },
    Meta(String),
}

impl Row {
    fn main_text(&self) -> String {
        match self {
            Self::User(text) => format!("你 · {text}"),
            Self::Assistant(text) => format!("助手 · {text}"),
            Self::Tool {
                name,
                origin,
                status,
                ..
            } => format!("工具 · {name} [{origin}] · {status}"),
            Self::Meta(text) => text.clone(),
        }
    }

    fn sub_text(&self) -> Option<String> {
        match self {
            Self::Tool { detail, .. } if !detail.is_empty() => Some(detail.clone()),
            _ => None,
        }
    }
}

/// 判断一行是否是"正在生成的助手回复"（以光标块结尾）。
fn is_streaming_row(row: &Row) -> bool {
    matches!(row, Row::Assistant(text) if text.contains('▌'))
}

fn streaming_text(row: &Row) -> &str {
    match row {
        Row::Assistant(text) => text,
        _ => "",
    }
}

fn describe(item: &Item) -> Row {
    match item {
        Item::User { text } => Row::User(text.clone()),
        Item::Assistant { text } => Row::Assistant(text.clone()),
        Item::Tool(tool) => {
            let (status, pending) = match &tool.state {
                ToolState::Pending => ("运行中".to_owned(), true),
                ToolState::Settled { is_error } => {
                    if *is_error {
                        ("失败".to_owned(), false)
                    } else {
                        ("完成".to_owned(), false)
                    }
                }
                ToolState::Aborted { reason } => (format!("中止 · {reason}"), false),
            };
            Row::Tool {
                name: tool.tool.clone(),
                origin: tool.origin.clone(),
                status,
                detail: tool
                    .card
                    .as_ref()
                    .map(ToolCard::summary)
                    .unwrap_or_default(),
                is_error: matches!(tool.state, ToolState::Settled { is_error: true }),
                pending,
            }
        }
        Item::Compaction { from_seq, to_seq } => {
            Row::Meta(format!("上下文已压缩 · seq {from_seq}–{to_seq}"))
        }
    }
}

/// 从当前日志生成整份快照；`streaming` 非空时把"正在生成的回复"追加为最后一行。
///
/// 快照而不是增量：UI 只做覆盖，不用维护事件合并逻辑，也不会因为丢一条消息就错位。
fn rows_snapshot(log: &SessionLog, streaming: Option<&str>) -> Vec<Row> {
    let mut rows: Vec<Row> = build_from_log(log).iter().map(describe).collect();
    if let Some(text) = streaming {
        rows.push(Row::Assistant(format!("{text}▌")));
    }
    rows
}

impl Render for ChatApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::env::var("DSHR_UI_TRACE").as_deref() == Ok("1") {
            eprintln!(
                "[ui-trace] bounds={:?} viewport={:?} scale={} rows={} busy={} last={:?}",
                window.bounds(),
                window.viewport_size(),
                window.scale_factor(),
                self.rows.len(),
                self.busy,
                self.rows.last().map(Row::main_text)
            );
        }
        let theme = self.theme();
        let p = theme.palette;
        if !self.initial_scroll_done && !self.rows.is_empty() {
            self.scroll.scroll_to_bottom();
            self.initial_scroll_done = true;
        }
        // 窄窗口（远程会话 / 小屏）自动收起侧栏，否则 280 的侧栏会把正文挤到窗口外。
        let collapsed = self.sidebar_collapsed || window.bounds().size.width < px(600.0);
        let mut shell = div().flex().size_full().bg(p.color(p.sidebar_fill));
        if !collapsed {
            shell = shell.child(self.sidebar(&theme, cx));
        }
        shell.child(
            div()
                .flex_1()
                .h_full()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .rounded_tl(theme.radii.lg)
                .border_l_1()
                .border_color(p.color(p.border_l3))
                .bg(p.color(p.bg_base))
                .child(self.header(&theme, collapsed, cx))
                .child(self.transcript(&theme, cx))
                .child(self.composer(&theme, cx)),
        )
    }
}

fn selftest() -> i32 {
    // 不用系统 TEMP：本机 TEMP 对部分子进程不可写（见 docs/m0-runbook.md）。
    let path = PathBuf::from(".tmp/chat-selftest.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path.clone(), &cancel);

    state.submit("你好");
    state.submit("用工具试一下"); // sidecar

    // 注册表里应当同时有 native 与 sidecar 工具。
    let both_registered = state.plugins.registry().get("probe_native").is_some()
        && state.plugins.registry().get("probe_echo").is_some();

    // 模拟 Sidecar 崩溃：整代撤销，Native 必须仍然可用。
    let killed = state.kill_sidecar().is_some();
    let sidecar_tools_gone = state.plugins.registry().get("probe_echo").is_none()
        && state.plugins.registry().get("probe_native").is_some();
    state.submit("用原生工具");

    // 重启 Sidecar：新 generation 重新注册，sidecar 工具恢复。
    let restarted = state.restart_sidecar();
    let sidecar_tools_back = state.plugins.registry().get("probe_echo").is_some();
    state.submit("再次用工具");

    let items = state.items();
    println!("[selftest] items={}", items.len());
    for item in &items {
        let row = describe(item);
        match row.sub_text() {
            Some(sub) => println!("  {}\n      ↳ {sub}", row.main_text()),
            None => println!("  {}", row.main_text()),
        }
    }

    // 4 轮 = 4 用户 + 4 助手 + 3 工具卡片（sidecar、native、sidecar）
    let cards_ok = matches!(&items[3], Item::Tool(t)
            if t.presentation.as_deref() == Some("generic")
                && t.card.as_ref().is_some_and(|card| card.kind == CardKind::Generic
                    && !card.detail.is_empty()))
        && matches!(&items[6], Item::Tool(t)
            if t.card.as_ref().is_some_and(|card| card.kind == CardKind::Generic));
    let ok = items.len() == 11
        && both_registered
        && killed
        && sidecar_tools_gone
        && restarted
        && sidecar_tools_back
        && cards_ok
        && matches!(&items[3], Item::Tool(t) if t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false })
        && matches!(&items[6], Item::Tool(t) if t.origin == "native"
            && t.state == ToolState::Settled { is_error: false })
        && matches!(&items[9], Item::Tool(t) if t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false });

    // 重启：从磁盘重新加载，回放必须一致
    let reloaded = SessionLog::from_jsonl(
        SESSION_ID,
        &std::fs::read_to_string(&path).unwrap_or_default(),
    )
    .expect("reload saved session");
    let replayed = build_from_log(&reloaded);
    let same = replayed == items;
    println!(
        "[selftest] persisted={} replay_equal={same}",
        path.display()
    );

    if ok && same {
        println!("[selftest] PASS");
        0
    } else {
        println!("[selftest] FAIL");
        1
    }
}

/// 用真实 Cordis 运行时跑一轮对话（`DSHR_SIDECAR=cordis`）。
fn selftest_cordis() -> i32 {
    let path = PathBuf::from(".tmp/chat-cordis.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path.clone(), &cancel);

    state.submit("用工具试一下");

    let items = state.items();
    println!("[selftest-cordis] items={}", items.len());
    for item in &items {
        let row = describe(item);
        match row.sub_text() {
            Some(sub) => println!("  {}\n      ↳ {sub}", row.main_text()),
            None => println!("  {}", row.main_text()),
        }
    }

    let ok = items.len() == 3
        && matches!(&items[1], Item::Tool(t) if t.tool == "cordis_echo"
            && t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false });

    let replayed = build_from_log(
        &SessionLog::from_jsonl(
            SESSION_ID,
            &std::fs::read_to_string(&path).unwrap_or_default(),
        )
        .expect("reload cordis session"),
    );
    let same = replayed == items;
    println!("[selftest-cordis] replay_equal={same}");

    if ok && same {
        println!("[selftest-cordis] PASS");
        0
    } else {
        println!("[selftest-cordis] FAIL");
        1
    }
}

/// 官方 DSH 插件自检（`DSHR_SIDECAR=dsh`）。
///
/// 装载的是**未修改的官方包**（`@deepseek-ai/dsh-tool-workspace-dependencies`
/// 与只读配置的 `@deepseek-ai/dsh-tool-fs`），通过 `tools` 服务的 `register`
/// 进入 Rust Registry，再按普通 sidecar 工具调用。
///
/// 依赖 `sidecar/dsh-official/node_modules`（`node tools/dsh-vendor.mjs` 从本机 DSH 抽取）。
/// 缺失时打印 SKIP 并返回 0：CI 环境不一定装了 DSH。
fn selftest_dsh() -> i32 {
    let root = std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .unwrap_or_else(|_| PathBuf::from("."));
    if !root.join("sidecar/dsh-official/node_modules").exists() {
        println!("[selftest-dsh] SKIP: 先运行 `node tools/dsh-vendor.mjs` 准备官方插件依赖");
        return 0;
    }

    let path = PathBuf::from(".tmp/chat-dsh.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path.clone(), &cancel);

    // 官方插件经 ctx.tools.register 映射进同一个 Registry，origin 必须是 sidecar。
    let registered_workspace = state
        .plugins
        .registry()
        .get("load_workspace_dependencies")
        .is_some_and(|tool| tool.origin.kind() == "sidecar");
    let registered_read = state
        .plugins
        .registry()
        .get("read")
        .is_some_and(|tool| tool.origin.kind() == "sidecar");

    state.submit("用工具试一下");

    let items = state.items();
    println!(
        "[selftest-dsh] items={} registered(workspace={registered_workspace}, read={registered_read})",
        items.len()
    );
    for item in &items {
        let row = describe(item);
        match row.sub_text() {
            Some(sub) => println!("  {}\n      ↳ {sub}", row.main_text()),
            None => println!("  {}", row.main_text()),
        }
    }

    let tool_ok = matches!(&items[1], Item::Tool(t)
        if t.tool == "load_workspace_dependencies"
            && t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false });
    // 官方 tool 的真实返回值里必须有 python 路径（证明 execute 真的跑了）。
    let value_ok = matches!(&items[1], Item::Tool(t)
        if t.card.as_ref().is_some_and(|card| card.detail.contains("python")));

    // 直接调官方 read 工具：证明带依赖（fs/systemPrompt）的插件也能执行。
    let read_ok = {
        let host = state.host.as_mut().expect("dsh sidecar should be running");
        host.invoke_as(
            "selftest-read-1",
            "read",
            json!({ "file_path": "README.md", "limit": 3 }),
            Duration::from_secs(5),
        )
        .ok()
        .filter(|outcome| outcome.terminal.kind == "tool.result")
        .is_some_and(|outcome| {
            let payload = &outcome.terminal.payload;
            !payload["is_error"].as_bool().unwrap_or(true)
                && payload["content"]
                    .to_string()
                    .contains("Native Agent Desktop")
        })
    };
    println!("[selftest-dsh] read_tool_ok={read_ok}");

    let replayed = build_from_log(
        &SessionLog::from_jsonl(
            SESSION_ID,
            &std::fs::read_to_string(&path).unwrap_or_default(),
        )
        .expect("reload dsh session"),
    );
    let same = replayed == items;
    println!("[selftest-dsh] replay_equal={same}");

    if registered_workspace && registered_read && tool_ok && value_ok && read_ok && same {
        println!("[selftest-dsh] PASS");
        0
    } else {
        println!("[selftest-dsh] FAIL");
        1
    }
}

/// 单轮真实调用（需要 API key）；用于人工验证 provider 与真流式，不进 CI。
fn live_turn(text: &str) -> i32 {
    let path = PathBuf::from(".tmp/chat-live.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path, &cancel);

    let started = std::time::Instant::now();
    let mut updates = 0usize;
    let mut first_text = None;
    let reply = state.stream_turn(text, &mut |rows| {
        updates += 1;
        if first_text.is_none() && rows.last().is_some_and(is_streaming_row) {
            first_text = Some(started.elapsed());
        }
    });
    let total = started.elapsed();
    let Some(reply) = reply else {
        println!("[live] empty prompt");
        return 1;
    };
    println!(
        "[live] stream_updates={updates} first_text_ms={} total_ms={}",
        first_text.map_or_else(|| "n/a".to_owned(), |d| d.as_millis().to_string()),
        total.as_millis()
    );
    println!("[live] reply={reply}");
    0
}

/// 流式自检：mock provider 把回复切成多段，断言"快照是增量推送的、落库只有一条 settled"。
fn selftest_stream() -> i32 {
    let path = PathBuf::from(".tmp/chat-stream.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path.clone(), &cancel);

    let mut snapshots: Vec<Vec<Row>> = Vec::new();
    let reply = state.stream_turn("你好", &mut |rows| snapshots.push(rows));
    let items = state.items();

    let streaming: Vec<&Row> = snapshots
        .iter()
        .filter_map(|rows| rows.last())
        .filter(|row| is_streaming_row(row))
        .collect();
    let final_row = snapshots.last().and_then(|rows| rows.last());
    println!(
        "[stream] snapshots={} streaming_rows={}",
        snapshots.len(),
        streaming.len()
    );
    for row in &streaming {
        println!("  {}", row.main_text());
    }

    let reply = reply.unwrap_or_default();
    let ok = reply.contains("收到：你好")
        && snapshots.len() >= 3
        && streaming.len() >= 2
        // 流式文本单调增长：每个快照都比上一个长。
        && streaming.windows(2).all(|pair| {
            streaming_text(pair[0]).chars().count() < streaming_text(pair[1]).chars().count()
        })
        && items.len() == 2
        && matches!(&items[0], Item::User { .. })
        && matches!(&items[1], Item::Assistant { text } if text == &reply)
        && final_row.is_some_and(|row| {
            matches!(row, Row::Assistant(text) if text.contains(&reply) && !text.contains('▌'))
        });

    if ok {
        println!("[selftest-stream] PASS");
        0
    } else {
        println!("[selftest-stream] FAIL");
        1
    }
}

/// 多轮真实对话（需要 API key）：验证"多轮记忆"而不是单轮应答。
///
/// 用法：`chat --live-chat "第一句" "第二句" ...`（每个参数是一轮用户消息）
fn live_chat(script: &[String]) -> i32 {
    let path = PathBuf::from(".tmp/chat-live-chat.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path.clone(), &cancel);

    let mut failed = false;
    for (index, text) in script.iter().enumerate() {
        let started = std::time::Instant::now();
        let mut updates = 0usize;
        let reply = state
            .stream_turn(text, &mut |_| updates += 1)
            .unwrap_or_default();
        if reply.starts_with("处理失败") {
            failed = true;
        }
        println!(
            "[live-chat] turn={} ms={} stream_updates={updates} reply={reply}",
            index + 1,
            started.elapsed().as_millis()
        );
    }

    // 落盘回放：同一份日志重建出的历史必须包含所有轮次。
    let reloaded = SessionLog::from_jsonl(
        SESSION_ID,
        &std::fs::read_to_string(&path).unwrap_or_default(),
    )
    .expect("reload live chat log");
    let history = history_for_model(&reloaded);
    println!(
        "[live-chat] persisted_turns={} history_messages={}",
        script.len(),
        history.len()
    );
    if failed {
        1
    } else {
        0
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|arg| arg == "--live-chat") {
        let script: Vec<String> = args.iter().skip(index + 1).cloned().collect();
        if script.is_empty() {
            println!("[live-chat] 需要至少一条消息");
            std::process::exit(1);
        }
        std::process::exit(live_chat(&script));
    }
    if let Some(index) = args.iter().position(|arg| arg == "--live") {
        let text = args
            .get(index + 1)
            .cloned()
            .unwrap_or_else(|| "用一句话介绍你自己".to_owned());
        std::process::exit(live_turn(&text));
    }

    if std::env::args().any(|arg| arg == "--selftest-stream") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_stream());
    }
    if std::env::args().any(|arg| arg == "--selftest-dsh") {
        std::env::set_var("DSHR_SIDECAR", "dsh");
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_dsh());
    }
    if std::env::args().any(|arg| arg == "--selftest-cordis") {
        std::env::set_var("DSHR_SIDECAR", "cordis");
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_cordis());
    }
    if std::env::args().any(|arg| arg == "--selftest") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest());
    }

    let path = PathBuf::from(".tmp/chat-session.jsonl");
    let cancel = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(ChatState::new(path, &cancel)));
    let app_cancel = cancel.clone();
    let rows = {
        let guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        rows_snapshot(&guard.log, None)
    };

    application().run(move |cx: &mut App| {
        // 文本输入组件的按键绑定（含回车提交）。
        dshr_ui_gpui::init(cx);
        // 先用一个保守尺寸开窗，拿到真实 scale / display 后再调整。
        // 高 DPI（本机 1.75）下 GPUI 把 Bounds 当逻辑像素、平台再乘 scale 变成物理窗口，
        // 而远程会话的可见视口可能只有显示器逻辑尺寸的一半——直接开 1180x780 会让
        // 右侧与底部（包括输入框）被裁掉，看起来就像"输出文本消失了"。
        let bounds = Bounds::centered(None, size(px(640.0), px(480.0)), cx);
        let state = state;
        let rows = rows;
        let app_cancel = app_cancel;
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |window, cx| {
                // 按"可见视口是逻辑尺寸 / scale"来估算，保证物理窗口不超过可见区域。
                if let Some(display) = window.display(cx) {
                    let scale = window.scale_factor();
                    let available = display.visible_bounds().size;
                    let width = (available.width.as_f32() / scale - 16.0).clamp(560.0, 1180.0);
                    let height = (available.height.as_f32() / scale - 24.0).clamp(400.0, 780.0);
                    if std::env::var("DSHR_UI_TRACE").as_deref() == Ok("1") {
                        eprintln!(
                            "[ui-trace] display={:?} scale={scale} window={width}x{height}",
                            available
                        );
                    }
                    window.resize(size(px(width), px(height)));
                }
                let input = cx.new(|cx| TextInput::new("输入消息，回车发送", cx));
                let app = cx.new(|_cx| ChatApp {
                    state,
                    rows,
                    input: input.clone(),
                    scroll: ScrollHandle::new(),
                    busy: false,
                    cancel: app_cancel,
                    theme_kind: ThemeKind::Light,
                    sidebar_collapsed: false,
                    initial_scroll_done: false,
                });
                // 回车提交：拿到弱引用回 ChatApp；回调返回 true 时输入框自己清空。
                let weak = app.downgrade();
                let accepted = std::rc::Rc::new(std::cell::Cell::new(false));
                let accepted_in = accepted.clone();
                input.update(cx, |input, _cx| {
                    input.set_on_submit(Box::new(move |text, _window, cx| {
                        let accepted = accepted_in.clone();
                        let _ = weak.update(cx, |chat, cx| {
                            accepted.set(chat.on_submit(text, cx));
                        });
                        accepted_in.get()
                    }));
                });
                let focus = input.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
                // 首次 IME 组合前刷新一次坐标，缓解候选窗落到屏幕角落（issue 0001）。
                window.invalidate_character_coordinates();
                app
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
