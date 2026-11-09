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

use dshr_compat_dsh_host::SidecarHost;
use dshr_conversation_model::{build_from_log, Item};
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

    /// 一轮对话：用户消息 → （可选）工具调用 → 助手回复。
    ///
    /// 模型是 mock；工具调用走真实 sidecar。
    fn submit(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
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
            return;
        }

        let wants_native = text.contains("原生") || text.to_ascii_lowercase().contains("native");
        let wants_tool =
            !wants_native && (text.contains("工具") || text.to_ascii_lowercase().contains("tool"));
        let mut tool_summary: Option<String> = None;

        if wants_native {
            self.seq += 1;
            let call_id = format!("call-n{}", self.seq);
            let _ = self.log.append(EventSpec::durable(
                "tool.call",
                &ts,
                json!({
                    "call_id": call_id,
                    "tool": "probe_native",
                    "origin": "native",
                    "arguments": { "text": text }
                }),
            ));

            match self.plugins.call("probe_native", &json!({ "text": text })) {
                Ok(value) => {
                    let _ = self.log.append(EventSpec::durable(
                        "tool.result",
                        &ts,
                        json!({
                            "call_id": call_id,
                            "is_error": false,
                            "content": [{ "type": "json", "value": value }]
                        }),
                    ));
                    tool_summary = Some(format!("原生工具返回 {value}"));
                }
                Err(err) => {
                    let _ = self.log.append(EventSpec::durable(
                        "tool.aborted",
                        &ts,
                        json!({ "call_id": call_id, "reason": err.to_string() }),
                    ));
                    tool_summary = Some("原生工具调用失败".to_owned());
                }
            }
        }

        if wants_tool {
            // sidecar 工具名从注册表里取：probe 插件是 probe_echo，
            // Cordis 插件是 cordis_echo，同一套 UI 不需要知道区别。
            let tool_name = self
                .host
                .as_ref()
                .and_then(|host| host.registrations().first().map(|reg| reg.name.clone()))
                .unwrap_or_else(|| "probe_echo".to_owned());
            self.seq += 1;
            let call_id = format!("call-{}", self.seq);
            let _ = self.log.append(EventSpec::durable(
                "tool.call",
                &ts,
                json!({
                    "call_id": call_id,
                    "tool": tool_name,
                    "origin": "sidecar",
                    "arguments": { "text": text }
                }),
            ));

            // 统一闸门：sidecar 工具与原生工具走同一个 Registry 的 authorize。
            let arguments = json!({ "text": text });
            if let Err(err) = self.plugins.registry().authorize(&tool_name, &arguments) {
                let _ = self.log.append(EventSpec::durable(
                    "tool.aborted",
                    &ts,
                    json!({ "call_id": call_id, "reason": err.to_string() }),
                ));
                tool_summary = Some(format!("工具被拒绝：{err}"));
            } else {
                match self.host.as_mut() {
                    Some(host) => {
                        match host.invoke_as(
                            &call_id,
                            &tool_name,
                            arguments,
                            Duration::from_secs(5),
                        ) {
                            Ok(outcome) => {
                                for (seq, chunk) in outcome.chunks.iter().enumerate() {
                                    let _ = self.log.append(EventSpec::ephemeral(
                                        "tool.chunk",
                                        &ts,
                                        json!({ "call_id": call_id, "seq": seq, "text": chunk }),
                                    ));
                                }
                                if outcome.terminal.kind == "tool.aborted" {
                                    let _ = self.log.append(EventSpec::durable(
                                        "tool.aborted",
                                        &ts,
                                        json!({
                                            "call_id": call_id,
                                            "reason": outcome.terminal.payload["reason"]
                                                .as_str()
                                                .unwrap_or("cancelled")
                                        }),
                                    ));
                                    tool_summary = Some("工具被中止".to_owned());
                                } else {
                                    let content = outcome.terminal.payload["content"].clone();
                                    let echo = content[0]["value"].clone();
                                    let _ = self.log.append(EventSpec::durable(
                                        "tool.result",
                                        &ts,
                                        json!({
                                            "call_id": call_id,
                                            "is_error": false,
                                            "content": content
                                        }),
                                    ));
                                    tool_summary = Some(format!("工具返回 {}", echo));
                                }
                            }
                            Err(err) => {
                                let _ = self.log.append(EventSpec::durable(
                                    "tool.aborted",
                                    &ts,
                                    json!({ "call_id": call_id, "reason": err.to_string() }),
                                ));
                                tool_summary = Some("工具调用失败".to_owned());
                            }
                        }
                    }
                    None => {
                        let _ = self.log.append(EventSpec::durable(
                            "tool.aborted",
                            &ts,
                            json!({ "call_id": call_id, "reason": "sidecar unavailable" }),
                        ));
                        tool_summary = Some("Sidecar 未启动".to_owned());
                    }
                }
            }
        }

        self.seq += 1;
        let reply = match tool_summary {
            Some(summary) => format!("{summary}。当前是 mock agent，尚未接入 LLM。"),
            None => format!("收到：{text}。当前是 mock agent，尚未接入 LLM。"),
        };
        let _ = self.log.append(EventSpec::durable(
            "message.assistant.settled",
            &ts,
            json!({ "message_id": format!("m-a{}", self.seq), "text": reply }),
        ));

        self.save();
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
}

impl ChatApp {
    fn send(&mut self, cx: &mut Context<Self>) {
        let text = std::mem::take(&mut self.input);
        if text.trim().is_empty() {
            return;
        }
        self.state.submit(&text);
        cx.notify();
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
        let rows: Vec<SharedString> = items.iter().map(row_text).collect();
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
                    .child(format!("会话 {SESSION_ID} · {} 条", items.len())),
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

fn main() {
    if std::env::args().any(|arg| arg == "--selftest-cordis") {
        std::env::set_var("DSHR_SIDECAR", "cordis");
        std::process::exit(selftest_cordis());
    }
    if std::env::args().any(|arg| arg == "--selftest") {
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
