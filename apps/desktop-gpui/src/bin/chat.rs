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
//!   cargo run ... --bin chat -- --live "一句话"               # 真实 LLM 单轮（需 API key）

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, uniform_list, App, Bounds, Context, Entity, Focusable,
    UniformListScrollHandle, Window, WindowBounds, WindowOptions,
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
use dshr_ui_gpui::TextInput;

const ROW_HEIGHT: f32 = 56.0;
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
/// `DSHR_SIDECAR=cordis` 时改用真实的 Cordis 运行时（同一版本 DSH 内嵌的 4.0.5），
/// 插件通过 `inject: ['bridge']` 注册工具——用于验证兼容性主张。
fn spawn_sidecar() -> Option<SidecarHost> {
    let root = std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .unwrap_or_else(|_| PathBuf::from("."));
    let cordis = std::env::var("DSHR_SIDECAR").as_deref() == Ok("cordis");
    let (script, plugin) = if cordis {
        (
            root.join("sidecar/cordis/sidecar-cordis.mjs"),
            root.join("sidecar/cordis/bridge-plugin.mjs"),
        )
    } else {
        (
            root.join("sidecar/probe-sidecar.mjs"),
            root.join("sidecar/probe-plugin.mjs"),
        )
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
    scroll: UniformListScrollHandle,
    /// 一轮正在生成：输入框仍可编辑，但回车不会发第二条。
    busy: bool,
    /// 与 provider 共享的停止句柄（UI 不持锁就能请求中断）。
    cancel: Arc<AtomicBool>,
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
}

/// 一行的渲染数据：主行 + （工具卡片的）副行。
///
/// 用 `String` 而不是 GPUI 类型：快照要跨线程从 Agent 线程送到 UI 线程。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    main: String,
    sub: Option<String>,
    tone: Tone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    User,
    Assistant,
    Tool,
    Meta,
}

impl Tone {
    fn background(self) -> gpui::Rgba {
        match self {
            Self::User => rgb(0x1b1b1b),
            Self::Assistant => rgb(0x161616),
            Self::Tool => rgb(0x1d2a3d),
            Self::Meta => rgb(0x2a2418),
        }
    }
}

fn describe(item: &Item) -> Row {
    match item {
        Item::User { text } => Row {
            main: format!("你    · {text}"),
            sub: None,
            tone: Tone::User,
        },
        Item::Assistant { text } => Row {
            main: format!("助手  · {text}"),
            sub: None,
            tone: Tone::Assistant,
        },
        Item::Tool(tool) => {
            let state = match &tool.state {
                ToolState::Pending => "运行中".to_owned(),
                ToolState::Settled { is_error } => {
                    if *is_error {
                        "失败".to_owned()
                    } else {
                        "成功".to_owned()
                    }
                }
                ToolState::Aborted { reason } => format!("中止({reason})"),
            };
            Row {
                main: format!("工具  · {} [{}] · {state}", tool.tool, tool.origin),
                sub: tool.card.as_ref().map(ToolCard::summary),
                tone: Tone::Tool,
            }
        }
        Item::Compaction { from_seq, to_seq } => Row {
            main: format!("上下文 · 已压缩 seq {from_seq}–{to_seq}"),
            sub: None,
            tone: Tone::Meta,
        },
    }
}

/// 从当前日志生成整份快照；`streaming` 非空时把"正在生成的回复"追加为最后一行。
///
/// 快照而不是增量：UI 只做覆盖，不用维护事件合并逻辑，也不会因为丢一条消息就错位。
fn rows_snapshot(log: &SessionLog, streaming: Option<&str>) -> Vec<Row> {
    let mut rows: Vec<Row> = build_from_log(log).iter().map(describe).collect();
    if let Some(text) = streaming {
        rows.push(Row {
            main: format!("助手  · {text}▌"),
            sub: None,
            tone: Tone::Assistant,
        });
    }
    rows
}

impl Render for ChatApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let total = self.rows.len();
        let rows_for_list = self.rows.clone();
        let header = if self.busy {
            format!("会话 {SESSION_ID} · {} 条 · 生成中（可停止）", total)
        } else {
            format!("会话 {SESSION_ID} · {} 条", total)
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x141414))
            .text_color(rgb(0xd6d6d6))
            .child(
                div()
                    .h(px(44.0))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(rgb(0x2a2a2a))
                    .text_sm()
                    .child(header),
            )
            .child(
                div().flex_1().overflow_hidden().child(
                    uniform_list(
                        "chat-transcript",
                        rows_for_list.len(),
                        cx.processor(
                            move |_this: &mut ChatApp, range: std::ops::Range<usize>, _w, _c| {
                                let mut out = Vec::with_capacity(range.len());
                                for ix in range {
                                    let row = rows_for_list.get(ix).cloned().unwrap_or(Row {
                                        main: String::new(),
                                        sub: None,
                                        tone: Tone::Assistant,
                                    });
                                    let mut cell = div()
                                        .id(ix)
                                        .h(px(ROW_HEIGHT))
                                        .px_4()
                                        .flex()
                                        .flex_col()
                                        .justify_center()
                                        .border_b_1()
                                        .border_color(rgb(0x222222))
                                        .bg(row.tone.background())
                                        .child(div().text_sm().child(row.main));
                                    if let Some(sub) = row.sub {
                                        cell = cell.child(
                                            div().text_xs().text_color(rgb(0x8f9bb3)).child(sub),
                                        );
                                    }
                                    out.push(cell.into_any_element());
                                }
                                out
                            },
                        ),
                    )
                    .track_scroll(&self.scroll)
                    .h_full(),
                ),
            )
            .child(
                div()
                    .h(px(56.0))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(rgb(0x2a2a2a))
                    .child(
                        div()
                            .flex_1()
                            .h(px(38.0))
                            .flex()
                            .items_center()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(0x3a3a3a))
                            .bg(rgb(0x1b1b1b))
                            .text_sm()
                            .child(self.input.clone()),
                    )
                    .children([div()
                        .id("send-button")
                        .ml_3()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(if self.busy {
                            rgb(0x5e2a2a)
                        } else {
                            rgb(0x243a5e)
                        })
                        .cursor_pointer()
                        .child(if self.busy { "停止" } else { "发送" })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.button_click(window, cx);
                        }))]),
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
        match row.sub {
            Some(sub) => println!("  {}\n      ↳ {sub}", row.main),
            None => println!("  {}", row.main),
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
        match row.sub {
            Some(sub) => println!("  {}\n      ↳ {sub}", row.main),
            None => println!("  {}", row.main),
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
        if first_text.is_none() && rows.last().is_some_and(|row| row.main.contains('▌')) {
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
        .filter(|row| row.main.contains('▌'))
        .collect();
    let final_row = snapshots.last().and_then(|rows| rows.last());
    println!(
        "[stream] snapshots={} streaming_rows={}",
        snapshots.len(),
        streaming.len()
    );
    for row in &streaming {
        println!("  {}", row.main);
    }

    let reply = reply.unwrap_or_default();
    let ok = reply.contains("收到：你好")
        && snapshots.len() >= 3
        && streaming.len() >= 2
        // 流式文本单调增长：每个快照都比上一个长。
        && streaming
            .windows(2)
            .all(|pair| pair[0].main.chars().count() < pair[1].main.chars().count())
        && items.len() == 2
        && matches!(&items[0], Item::User { .. })
        && matches!(&items[1], Item::Assistant { text } if text == &reply)
        && final_row.is_some_and(|row| row.main.contains(&reply) && !row.main.contains('▌'));

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
        let bounds = Bounds::centered(None, size(px(900.0), px(640.0)), cx);
        let state = state;
        let rows = rows;
        let app_cancel = app_cancel;
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |window, cx| {
                let input = cx.new(|cx| TextInput::new("输入消息，回车发送", cx));
                let app = cx.new(|_cx| ChatApp {
                    state,
                    rows,
                    input: input.clone(),
                    scroll: UniformListScrollHandle::new(),
                    busy: false,
                    cancel: app_cancel,
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
