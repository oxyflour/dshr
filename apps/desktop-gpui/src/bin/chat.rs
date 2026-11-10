//! 基本对话：输入框 + 会话流 + 工具调用 + 落盘恢复。
//!
//! 这是 M1 的产品面：能打字、能发送、能看到工具卡片、关掉再开会话还在。
//! 模型是 **mock**（没有接 LLM，那是 M2），但会话/工具/日志/回放都是真实的。
//!
//! 两种模式：
//!   cargo run -p dshr-desktop-gpui --bin chat            # 交互界面
//!   cargo run -p dshr-desktop-gpui --bin chat -- --selftest   # 无窗口自检（CI 用）

use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, uniform_list, App, Bounds, Context, FocusHandle, KeyDownEvent,
    SharedString, UniformListScrollHandle, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use serde_json::json;

use dshr_agent_core::{
    Agent, AgentEvent, MockProvider, ModelProvider, ToolCallRequest, ToolExecutor, ToolOutcome,
    ToolSpec,
};
use dshr_compat_dsh_host::SidecarHost;
use dshr_conversation_model::{build_from_log, Item};
use dshr_llm_deepseek::DeepSeekProvider;
use dshr_plugin_api::{Plugin, PluginContext, PluginError};
use dshr_plugin_host::PluginHost;
use dshr_session_store::{EventSpec, SessionLog, ToolState};
use dshr_tool_runtime::{AllowAll, ToolDefinition, ToolOrigin};

const ROW_HEIGHT: f32 = 52.0;
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
            })),
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
fn select_provider() -> Box<dyn ModelProvider> {
    // 自检必须确定性：显式要求 mock 时不看环境里的 key。
    if std::env::var("DSHR_FORCE_MOCK").as_deref() == Ok("1") {
        println!("[chat] provider=mock（DSHR_FORCE_MOCK=1）");
        return Box::new(MockProvider);
    }
    match DeepSeekProvider::from_env() {
        Some(provider) => {
            println!("[chat] provider=deepseek model={}", provider.model());
            Box::new(provider)
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
    fn new(path: PathBuf) -> Self {
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
            agent: Agent::new(select_provider()),
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

    /// 一轮对话的前半段：用户消息 → （可选）工具调用 → 返回要流式展示的回复文本。
    ///
    /// 助手消息由 `finish_turn` 在流式结束后落库，这样 UI 可以逐字渲染，
    /// 而 Session Log 里仍然只有一条 settled 消息。
    /// 一轮对话的前半段：用户消息 → Agent 循环（模型 + 工具执行）→ 返回要流式展示的文本。
    ///
    /// Agent 只产出事件；落盘由这里完成（Agent 不碰 Session Log）。
    fn begin_turn(&mut self, text: &str) -> Option<String> {
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

        let events = {
            let mut executor = ChatExecutor {
                plugins: &mut self.plugins,
                host: self.host.as_mut(),
            };
            self.agent.run_turn(text, &mut executor)
        };

        let mut reply = String::new();
        for event in events {
            match event {
                AgentEvent::TextDelta(delta) => reply.push_str(&delta),
                AgentEvent::ToolCall(call) => {
                    let origin = self
                        .plugins
                        .registry()
                        .get(&call.tool)
                        .map(|definition| definition.origin.kind())
                        .unwrap_or("unknown");
                    let _ = self.log.append(EventSpec::durable(
                        "tool.call",
                        &ts,
                        json!({
                            "call_id": call.call_id,
                            "tool": call.tool,
                            "origin": origin,
                            "arguments": call.arguments
                        }),
                    ));
                }
                AgentEvent::ToolResult { call_id, outcome } => {
                    if outcome.is_error {
                        let _ = self.log.append(EventSpec::durable(
                            "tool.aborted",
                            &ts,
                            json!({ "call_id": call_id, "reason": outcome.content.to_string() }),
                        ));
                    } else {
                        let _ = self.log.append(EventSpec::durable(
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
                }
                AgentEvent::TurnFailed(err) => {
                    reply.push_str(&format!("处理失败：{err}"));
                }
                AgentEvent::TurnStarted { .. }
                | AgentEvent::StepStarted { .. }
                | AgentEvent::TurnEnded { .. } => {}
            }
        }

        Some(reply)
    }

    fn finish_turn(&mut self, reply: &str) {
        self.seq += 1;
        let ts = now_ts();
        let _ = self.log.append(EventSpec::durable(
            "message.assistant.settled",
            &ts,
            json!({ "message_id": format!("m-a{}", self.seq), "text": reply }),
        ));
        self.save();
    }

    /// 同步版本（自检与无 UI 场景使用）。
    fn submit(&mut self, text: &str) {
        if let Some(reply) = self.begin_turn(text) {
            self.finish_turn(&reply);
        }
    }

    fn items(&self) -> Vec<Item> {
        build_from_log(&self.log)
    }
}

struct ChatApp {
    state: ChatState,
    input: String,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    /// 正在逐字渲染的助手回复（尚未写入 Session Log）。
    streaming: Option<String>,
}

impl ChatApp {
    fn send(&mut self, cx: &mut Context<Self>) {
        let text = std::mem::take(&mut self.input);
        if text.trim().is_empty() {
            return;
        }
        let Some(reply) = self.state.begin_turn(&text) else {
            return;
        };

        // 逐字渲染：每 12ms 追加一个字符，结束后才把 assistant 消息写入日志。
        self.streaming = Some(String::new());
        cx.notify();
        let reply_for_task = reply.clone();
        cx.spawn(async move |this, cx| {
            // 每次推送 8 个字符：实测 timer + 重绘一轮约 150ms，
            // 逐字符推 40 字要 2.5s 以上，视觉上反而不像"流式"。
            let chars: Vec<char> = reply_for_task.chars().collect();
            for chunk in chars.chunks(8) {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let piece: String = chunk.iter().collect();
                let ok = this
                    .update(cx, |app, cx| {
                        if let Some(buffer) = app.streaming.as_mut() {
                            buffer.push_str(&piece);
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !ok {
                    return;
                }
            }
            let _ = this.update(cx, |app, cx| {
                app.state.finish_turn(&reply);
                app.streaming = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "enter" => {
                self.send(cx);
                return;
            }
            "backspace" => {
                self.input.pop();
                cx.notify();
                return;
            }
            _ => {}
        }
        if let Some(ch) = event.keystroke.key_char.as_deref() {
            // 只接受可见字符；组合输入（IME）由后续的 EntityInputHandler 组件接管。
            if !ch.chars().next().map(|c| c.is_control()).unwrap_or(true) {
                self.input.push_str(ch);
                cx.notify();
            }
        }
    }
}

fn row_text(item: &Item) -> SharedString {
    match item {
        Item::User { text } => SharedString::from(format!("你    · {text}")),
        Item::Assistant { text } => SharedString::from(format!("助手  · {text}")),
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
            SharedString::from(format!("工具  · {} [{}] · {state}", tool.tool, tool.origin))
        }
        Item::Compaction { from_seq, to_seq } => {
            SharedString::from(format!("上下文 · 已压缩 seq {from_seq}–{to_seq}"))
        }
    }
}

impl Render for ChatApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.state.items();
        let mut rows: Vec<SharedString> = items.iter().map(row_text).collect();
        if let Some(buffer) = &self.streaming {
            rows.push(SharedString::from(format!("助手  · {buffer}▌")));
        }
        let total = rows.len();
        let rows_for_list = rows.clone();
        let input_text = if self.input.is_empty() {
            "输入消息，回车发送（mock agent）".to_owned()
        } else {
            format!("{}▌", self.input)
        };
        let dim = self.input.is_empty();

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
                    .child(format!("会话 {SESSION_ID} · {} 条", total)),
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
                                    let text = rows_for_list.get(ix).cloned().unwrap_or_default();
                                    let tone = if text.starts_with("工具") {
                                        rgb(0x1d2a3d)
                                    } else if text.starts_with("你") {
                                        rgb(0x1b1b1b)
                                    } else {
                                        rgb(0x161616)
                                    };
                                    out.push(
                                        div()
                                            .id(ix)
                                            .h(px(ROW_HEIGHT))
                                            .px_4()
                                            .flex()
                                            .items_center()
                                            .border_b_1()
                                            .border_color(rgb(0x222222))
                                            .bg(tone)
                                            .child(text)
                                            .into_any_element(),
                                    );
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
                            .track_focus(&self.focus)
                            .on_key_down(cx.listener(Self::on_key_down))
                            .flex_1()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(0x3a3a3a))
                            .bg(rgb(0x1b1b1b))
                            .text_sm()
                            .when(dim, |el| el.text_color(rgb(0x777777)))
                            .child(SharedString::from(input_text)),
                    )
                    .children([div()
                        .id("send-button")
                        .ml_3()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(0x243a5e))
                        .cursor_pointer()
                        .child("发送")
                        .on_click(cx.listener(|this, _, _, cx| this.send(cx)))]),
            )
    }
}

fn selftest() -> i32 {
    // 不用系统 TEMP：本机 TEMP 对部分子进程不可写（见 docs/m0-runbook.md）。
    let path = PathBuf::from(".tmp/chat-selftest.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    let mut state = ChatState::new(path.clone());

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
        println!("  {}", row_text(item));
    }

    // 4 轮 = 4 用户 + 4 助手 + 3 工具卡片（sidecar、native、sidecar）
    let ok = items.len() == 11
        && both_registered
        && killed
        && sidecar_tools_gone
        && restarted
        && sidecar_tools_back
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
    let mut state = ChatState::new(path.clone());

    state.submit("用工具试一下");

    let items = state.items();
    println!("[selftest-cordis] items={}", items.len());
    for item in &items {
        println!("  {}", row_text(item));
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

/// 单轮真实调用（需要 API key）；用于人工验证 provider，不进 CI。
fn live_turn(text: &str) -> i32 {
    let path = PathBuf::from(".tmp/chat-live.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    let mut state = ChatState::new(path);
    let Some(reply) = state.begin_turn(text) else {
        println!("[live] empty prompt");
        return 1;
    };
    state.finish_turn(&reply);
    println!("[live] reply={reply}");
    0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|arg| arg == "--live") {
        let text = args
            .get(index + 1)
            .cloned()
            .unwrap_or_else(|| "用一句话介绍你自己".to_owned());
        std::process::exit(live_turn(&text));
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
    let state = ChatState::new(path);

    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.0), px(640.0)), cx);
        let state = state;
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |window, cx| {
                let app = cx.new(|cx| ChatApp {
                    state,
                    input: String::new(),
                    focus: cx.focus_handle(),
                    scroll: UniformListScrollHandle::new(),
                    streaming: None,
                });
                let focus = app.read(cx).focus.clone();
                window.focus(&focus, cx);
                app
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
