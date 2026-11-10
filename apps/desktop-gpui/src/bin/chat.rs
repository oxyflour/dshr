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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{
    div, px, size, AnyElement, App, Bounds, Context, Entity, Focusable, Font, FontWeight, Rgba,
    ScrollHandle, TextOverflow, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use serde_json::json;

use dshr_agent_context::{estimate_tokens, history_for_model, history_from_log, plan_context};
use dshr_agent_core::{
    Agent, AgentEvent, ChatMessage, CompletionRequest, MockProvider, ModelProvider,
    ToolCallRequest, ToolExecutor, ToolOutcome, ToolSpec,
};
use dshr_compat_dsh_host::SidecarHost;
use dshr_conversation_model::{build_from_log, CardKind, Item, ToolCard};
use dshr_llm_deepseek::DeepSeekProvider;
use dshr_llm_openai::OpenAiProvider;
use dshr_llm_retry::{RetryPolicy, RetryingProvider};
use dshr_plugin_api::{Plugin, PluginContext, PluginError};
use dshr_plugin_host::PluginHost;
use dshr_profile::{install_package, Profile, ProfileSource};
use dshr_sandbox::Workspace;
use dshr_session_store::{EventSpec, SessionIndex, SessionLog, SessionMeta, ToolState};
use dshr_tool_runtime::{
    ApprovalBroker, ApprovalOutcome, ApprovalPolicy, RegistryError, ToolDefinition, ToolOrigin,
};
use dshr_ui_gpui::{markdown_view, TextInput, Theme, ThemeKind};

const SESSION_ID: &str = "s-chat";

fn now_ts() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("t{nanos}")
}

/// 侧栏的相对时间（不引入日期库）。
fn relative_time(updated_at: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let diff = now.saturating_sub(updated_at);
    if diff < 60 {
        String::from("刚刚")
    } else if diff < 3_600 {
        format!("{} 分钟前", diff / 60)
    } else if diff < 86_400 {
        format!("{} 小时前", diff / 3_600)
    } else {
        format!("{} 天前", diff / 86_400)
    }
}

const SUMMARY_PROMPT: &str = "你是对话压缩器。把下面的对话压缩成 200 字以内的中文摘要，\
保留：任务目标、已确认的事实、关键决定、未完成的步骤、工具调用的结论。只输出摘要，不要客套。";

/// 把被丢弃的历史压缩成一段摘要；provider 失败时退回"截取开头"，不让压缩阻塞对话。
fn summarize_dropped(provider: &mut dyn ModelProvider, dropped: &[ChatMessage]) -> String {
    let transcript = transcript_text(dropped, 8_000);
    if transcript.trim().is_empty() {
        return String::from("（没有可压缩的历史）");
    }
    let request = CompletionRequest {
        messages: vec![
            ChatMessage::System(SUMMARY_PROMPT.to_owned()),
            ChatMessage::user(transcript.clone()),
        ],
        tool_specs: Vec::new(),
    };
    let reply = provider
        .complete_streaming(&request, &mut |_| {})
        .map(|reply| reply.text)
        .unwrap_or_default();
    let summary = reply.trim();
    if summary.is_empty() {
        let head: String = transcript.chars().take(600).collect();
        format!("（摘要生成失败，保留历史开头）\n{head}")
    } else {
        summary.chars().take(1_200).collect()
    }
}

/// 把消息拼成摘要输入；总长受 `max_chars` 限制。
fn transcript_text(messages: &[ChatMessage], max_chars: usize) -> String {
    let mut out = String::new();
    for message in messages {
        let (role, text) = match message {
            ChatMessage::System(text) => ("system", text.clone()),
            ChatMessage::User(text) => ("user", text.clone()),
            ChatMessage::Assistant { text, tool_calls } => {
                let mut body = text.clone();
                if !tool_calls.is_empty() {
                    let names: Vec<&str> =
                        tool_calls.iter().map(|call| call.name.as_str()).collect();
                    body.push_str(&format!("（调用工具 {}）", names.join(", ")));
                }
                ("assistant", body)
            }
            ChatMessage::Tool { content, .. } => ("tool", content.clone()),
        };
        out.push_str(&format!("[{role}] {text}\n"));
        if out.chars().count() >= max_chars {
            break;
        }
    }
    out.chars().take(max_chars).collect()
}

struct ChatState {
    log: SessionLog,
    path: PathBuf,
    host: Option<SidecarHost>,
    plugins: PluginHost,
    /// 必须跨轮复用：turn id 单调递增，否则 call_id 重复会被 Session Log 拒收。
    agent: Agent<Box<dyn ModelProvider>>,
    seq: u64,
    /// 审批 broker：工作线程阻塞等待 UI 决定；不属于大锁保护范围。
    approvals: Arc<ApprovalBroker>,
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

/// 原生写入工具：走沙箱 seam（路径根 + 写前快照 + 1 MiB 限制）。
///
/// 工具展示类型是 `diff`，所以默认策略会要求审批；审批通过后才写文件。
/// 这不是官方 `write` 的替代品，而是用来证明"审批 + 沙箱"链路可端到端工作。
struct ProbeWritePlugin {
    workspace: Workspace,
}

impl ProbeWritePlugin {
    fn new() -> Self {
        let root = std::env::var("DSHR_WORKSPACE").unwrap_or_else(|_| String::from("."));
        let workspace = Workspace::new(&root).unwrap_or_else(|err| {
            eprintln!("[chat] 沙箱工作区不可用（回退到当前目录）: {err}");
            Workspace::new(".").expect("cwd workspace")
        });
        Self { workspace }
    }
}

impl Plugin for ProbeWritePlugin {
    fn id(&self) -> &'static str {
        "builtin.probe-write"
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "probe_write",
                "把一段文本写入工作区文件（沙箱 seam：相对路径、写前快照、1 MiB 限制）",
                ToolOrigin::Native {
                    plugin_id: "builtin.probe-write".to_owned(),
                },
            )
            .with_schema(json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                }
            }))
            .with_presentation("diff"),
        )?;
        Ok(())
    }

    fn call(
        &mut self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        if tool != "probe_write" {
            return Err(PluginError::UnknownTool {
                plugin: "builtin.probe-write".to_owned(),
                tool: tool.to_owned(),
            });
        }
        let path = arguments
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| PluginError::Invalid(String::from("missing path")))?;
        let content = arguments
            .get("content")
            .and_then(|value| value.as_str())
            .ok_or_else(|| PluginError::Invalid(String::from("missing content")))?;
        let receipt = self
            .workspace
            .write_text(path, content)
            .map_err(|err| PluginError::Invalid(err.to_string()))?;
        Ok(json!({
            "path": receipt.path.display().to_string(),
            "bytes": receipt.bytes,
            "snapshot": receipt.snapshot.map(|snapshot| snapshot.id),
        }))
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

/// provider 选择：`DSHR_PROVIDER` 显式指定（mock/deepseek/openai），否则按 key 自动。
///
/// 真实 provider 统一包一层 `RetryingProvider`（429/5xx/连接中断指数退避）；
/// 流式请求只在"还没有交付任何 delta"时重试，避免重复显示。
fn select_provider(cancel: &Arc<AtomicBool>) -> Box<dyn ModelProvider> {
    // 自检必须确定性：显式要求 mock 时不看环境里的 key。
    if std::env::var("DSHR_FORCE_MOCK").as_deref() == Ok("1") {
        println!("[chat] provider=mock（DSHR_FORCE_MOCK=1）");
        return Box::new(MockProvider);
    }
    let requested = std::env::var("DSHR_PROVIDER").unwrap_or_default();
    let with_retries = |provider: Box<dyn ModelProvider>, name: &str, model: &str| {
        println!(
            "[chat] provider={name} model={model} retries={}",
            RetryPolicy::from_env().max_attempts
        );
        Box::new(RetryingProvider::new(provider, RetryPolicy::from_env()))
    };

    match requested.as_str() {
        "mock" => {
            println!("[chat] provider=mock（DSHR_PROVIDER=mock）");
            return Box::new(MockProvider);
        }
        "openai" => {
            if let Some(provider) = OpenAiProvider::from_env() {
                let model = provider.model().to_owned();
                return with_retries(
                    Box::new(provider.with_cancel(cancel.clone())),
                    "openai",
                    &model,
                );
            }
            println!("[chat] provider=mock（DSHR_PROVIDER=openai 但缺少 OPENAI_API_KEY）");
            return Box::new(MockProvider);
        }
        "deepseek" => {
            if let Some(provider) = DeepSeekProvider::from_env() {
                let model = provider.model().to_owned();
                return with_retries(
                    Box::new(provider.with_cancel(cancel.clone())),
                    "deepseek",
                    &model,
                );
            }
            println!("[chat] provider=mock（DSHR_PROVIDER=deepseek 但缺少 DEEPSEEK_API_KEY）");
            return Box::new(MockProvider);
        }
        _ => {}
    }

    if let Some(provider) = DeepSeekProvider::from_env() {
        let model = provider.model().to_owned();
        return with_retries(
            Box::new(provider.with_cancel(cancel.clone())),
            "deepseek",
            &model,
        );
    }
    if let Some(provider) = OpenAiProvider::from_env() {
        let model = provider.model().to_owned();
        return with_retries(
            Box::new(provider.with_cancel(cancel.clone())),
            "openai",
            &model,
        );
    }
    println!("[chat] provider=mock（未找到 DEEPSEEK_API_KEY / OPENAI_API_KEY）");
    Box::new(MockProvider)
}

/// Agent 的工具执行器：authorize 之后按来源分发给 PluginHost（native）或 Sidecar。
struct ChatExecutor<'a> {
    plugins: &'a mut PluginHost,
    host: Option<&'a mut SidecarHost>,
    approvals: &'a ApprovalBroker,
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
            Err(RegistryError::NeedsApproval { tool }) => {
                // 策略说"要确认"：阻塞等工作线程外的 UI 决定（或超时按拒绝）。
                let outcome = self.approvals.request(
                    &call.call_id,
                    &tool,
                    &call.arguments,
                    Duration::from_secs(300),
                );
                match outcome {
                    ApprovalOutcome::AllowOnce => {
                        match self.plugins.registry().get(&tool).cloned() {
                            Some(definition) => definition,
                            None => return ToolOutcome::failure(format!("工具 {tool} 已卸载")),
                        }
                    }
                    ApprovalOutcome::Deny => {
                        return ToolOutcome::failure(format!("用户拒绝了工具 `{tool}` 的调用"))
                    }
                }
            }
            Err(err) => return ToolOutcome::failure(err.to_string()),
        };

        match definition.origin {
            ToolOrigin::Native { .. } => {
                match self.plugins.call_approved(&call.tool, &call.arguments) {
                    Ok(value) => {
                        ToolOutcome::success(json!([{ "type": "json", "value": value }]), value)
                    }
                    Err(err) => ToolOutcome::failure(err.to_string()),
                }
            }
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
    fn new(
        path: PathBuf,
        session_id: &str,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
    ) -> Self {
        let log = if path.exists() {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            SessionLog::from_jsonl(session_id, &text).unwrap_or_else(|err| {
                eprintln!("[chat] 无法加载历史会话（忽略并从空会话开始）: {err}");
                SessionLog::new(session_id)
            })
        } else {
            SessionLog::new(session_id)
        };

        let host = spawn_sidecar();
        let policy = if std::env::var("DSHR_TOOL_POLICY").as_deref() == Ok("ask-all") {
            ApprovalPolicy::ask_all()
        } else {
            ApprovalPolicy::desktop()
        };
        let mut plugins = PluginHost::new(policy);
        if let Err(err) = plugins.load(Box::new(ProbeNativePlugin)) {
            eprintln!("[chat] 原生插件加载失败: {err}");
        }
        if let Err(err) = plugins.load(Box::new(ProbeWritePlugin::new())) {
            eprintln!("[chat] 写入插件加载失败: {err}");
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
            approvals,
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

    /// 重启 Sidecar：先撤销旧 generation，再按当前 profile/环境重新装载。
    fn restart_sidecar(&mut self) -> bool {
        if let Some(old) = self.host.take() {
            self.plugins.invalidate_generation(old.generation());
        }
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
        // 上下文预算在这里做：超长工具结果头尾裁剪、超预算丢最旧，且不拆配对。
        let mut plan = plan_context(&self.log, "");
        if let Some((from_seq, to_seq)) = plan.dropped_range {
            // 丢弃了旧轮次：先让模型生成摘要，写 context.compacted，再按压缩后的历史请求。
            let summary = {
                let provider: &mut dyn ModelProvider = self.agent.provider_mut().as_mut();
                summarize_dropped(provider, &plan.dropped)
            };
            let summary_ts = now_ts();
            self.seq += 1;
            let summary_id = format!("m-sum{}", self.seq);
            let tokens_after = plan.tokens_after + estimate_tokens(&summary);
            let _ = self.log.append(EventSpec::durable(
                "message.assistant.settled",
                &summary_ts,
                json!({ "message_id": summary_id, "text": summary }),
            ));
            let _ = self.log.append(EventSpec::durable(
                "context.compacted",
                &summary_ts,
                json!({
                    "from_seq": from_seq,
                    "to_seq": to_seq,
                    "summary_message_id": summary_id,
                    "tokens_before": plan.tokens_before,
                    "tokens_after": tokens_after,
                }),
            ));
            self.save();
            plan = plan_context(&self.log, "");
            eprintln!(
                "[chat] 已压缩 seq {from_seq}..{to_seq}：{} -> {} tokens",
                plan.tokens_before, plan.tokens_after
            );
            emit(rows_snapshot(&self.log, None));
        }
        let _ = self.log.append(EventSpec::durable(
            "context.measured",
            &ts,
            json!({
                "tokens": {
                    "input": plan.tokens_after,
                    "output": 0,
                    "total": plan.tokens_after,
                },
                "window": plan.window,
                "ratio": plan.ratio(),
                "pruned_tool_results": plan.pruned_tool_results,
                "dropped_messages": plan.dropped_messages,
            }),
        ));
        self.save();
        if plan.triggered() {
            eprintln!(
                "[chat] 上下文裁剪: {} -> {} tokens（窗口 {}，裁剪工具结果 {} 条，丢弃消息 {} 条）",
                plan.tokens_before,
                plan.tokens_after,
                plan.window,
                plan.pruned_tool_results,
                plan.dropped_messages
            );
        }
        emit(rows_snapshot(&self.log, None));
        let history = plan.messages;

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
                approvals,
                ..
            } = self;
            let mut executor = ChatExecutor {
                plugins,
                host: host.as_mut(),
                approvals: approvals.as_ref(),
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

        // 审批痕迹落盘（在工具事件之后：审批链用于审计/回放，不影响模型输入）。
        let records = self.approvals.take_records();
        for record in records {
            let approval_ts = now_ts();
            let _ = self.log.append(EventSpec::durable(
                "approval.asked",
                &approval_ts,
                json!({
                    "call_id": record.call_id,
                    "request": { "tool": record.tool, "arguments": record.arguments },
                    "policy": "desktop",
                }),
            ));
            let _ = self.log.append(EventSpec::durable(
                "approval.answered",
                &approval_ts,
                json!({
                    "call_id": record.call_id,
                    "outcome": record.outcome.as_str(),
                    "answerer": record.answerer,
                }),
            ));
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

    /// 直接执行一次工具调用（自检用；走与真实轮次相同的策略/审批/沙箱路径）。
    fn execute_tool(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        let mut executor = ChatExecutor {
            plugins: &mut self.plugins,
            host: self.host.as_mut(),
            approvals: self.approvals.as_ref(),
        };
        executor.execute(call)
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
    /// 会话目录索引与列表。
    store: SessionIndex,
    sessions: Vec<SessionMeta>,
    session_id: String,
    /// 正在重命名的会话与输入框（None = 不在重命名）。
    renaming: Option<String>,
    rename_input: Option<Entity<TextInput>>,
    /// 等待二次确认删除的会话（None = 没有待确认项）。
    pending_delete: Option<String>,
    /// 工具审批 broker（与 ChatState 共享同一实例）。
    approvals: Arc<ApprovalBroker>,
    /// 上一次渲染时的 pending 数，用于只在变化时触发重绘。
    pending_approvals: usize,
    /// 插件 profile（启停状态持久化）。
    profile: Profile,
    /// 右侧插件面板是否打开。
    plugins_open: bool,
    /// 长会话只渲染最近 N 条（点"载入更早"每次 +200），避免每帧构造上万元素。
    visible_limit: usize,
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
                            // 行数变化 = 新消息落盘：顺手刷新侧栏标题/时间。
                            if rows.len() != app.rows.len() {
                                app.reload_sessions();
                            }
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
                // 审批可能出现在两个快照之间：轮询 pending，变化时重绘。
                let _ = this.update(cx, |app, cx| {
                    let pending = app.approvals.pending().len();
                    if pending != app.pending_approvals {
                        app.pending_approvals = pending;
                        cx.notify();
                    }
                });
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

    // ------------------------------------------------------------------
    // 多会话：索引/切换/新建/重命名/删除
    // ------------------------------------------------------------------

    fn reload_sessions(&mut self) {
        match self.store.scan() {
            Ok(sessions) => self.sessions = sessions,
            Err(err) => eprintln!("[chat] 扫描会话目录失败: {err}"),
        }
    }

    fn active_title(&self) -> String {
        self.sessions
            .iter()
            .find(|session| session.id == self.session_id)
            .map(|session| session.title.clone())
            .unwrap_or_else(|| self.session_id.clone())
    }

    /// 切到另一个会话：新建一份 ChatState（含侧车/插件），把 UI 状态整体替换。
    ///
    /// 生成中不允许切换——工作线程还在往旧 state 写，切走会让 UI 与日志分叉。
    fn open_session(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || id == self.session_id {
            return;
        }
        let path = self.store.path_for(&id);
        let cancel = Arc::new(AtomicBool::new(false));
        let state = ChatState::new(path, &id, &cancel, self.approvals.clone());
        let rows = rows_snapshot(&state.log, None);
        self.state = Arc::new(Mutex::new(state));
        self.cancel = cancel;
        self.rows = rows;
        self.session_id = id;
        self.busy = false;
        self.initial_scroll_done = false;
        self.visible_limit = 200;
        self.renaming = None;
        self.rename_input = None;
        self.pending_delete = None;
        self.reload_sessions();
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let id = self.store.new_id();
        if let Err(err) = self.store.create(&id) {
            eprintln!("[chat] 新建会话失败: {err}");
            return;
        }
        if !self.store.path_for(&id).exists() {
            self.reload_sessions();
        }
        self.open_session(id, window, cx);
    }

    fn delete_session(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_delete.as_deref() != Some(id.as_str()) {
            self.pending_delete = Some(id);
            cx.notify();
            return;
        }
        self.pending_delete = None;
        if self.busy && id == self.session_id {
            return;
        }
        if let Err(err) = self.store.delete(&id) {
            eprintln!("[chat] 删除会话 {id} 失败: {err}");
            return;
        }
        self.renaming = None;
        self.rename_input = None;
        self.reload_sessions();
        if id == self.session_id {
            match self.sessions.first().map(|session| session.id.clone()) {
                Some(next) => self.open_session(next, window, cx),
                None => self.new_session(window, cx),
            }
        } else {
            cx.notify();
        }
    }

    fn start_rename(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = self
            .sessions
            .iter()
            .find(|session| session.id == id)
            .map(|session| session.title.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| TextInput::new("会话名称", cx));
        input.update(cx, |input, cx| input.set_content(&title, window, cx));
        let handle = input.read(cx).focus_handle(cx);
        self.renaming = Some(id);
        self.rename_input = Some(input);
        self.pending_delete = None;
        window.focus(&handle, cx);
        cx.notify();
    }

    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let (Some(id), Some(input)) = (self.renaming.clone(), self.rename_input.clone()) else {
            return;
        };
        let title = input.read(cx).content().to_owned();
        if let Err(err) = self.store.rename(&id, &title) {
            eprintln!("[chat] 重命名会话 {id} 失败: {err}");
        }
        self.renaming = None;
        self.rename_input = None;
        self.reload_sessions();
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.renaming = None;
        self.rename_input = None;
        cx.notify();
    }

    /// 启用/禁用插件：写 profile、更新 sidecar 环境、空闲时重启当前会话的 sidecar。
    fn toggle_plugin(&mut self, name: String, cx: &mut Context<Self>) {
        if !self.profile.can_edit() {
            eprintln!("[chat] Loader 模式插件列表只读（改 cordis.yml / patch 后重启）");
            return;
        }
        let Some(entry) = self
            .profile
            .entries()
            .iter()
            .find(|entry| entry.name == name)
            .cloned()
        else {
            return;
        };
        let next = !entry.enabled;
        if !self.profile.set_enabled(&name, next) {
            return;
        }
        if let Err(err) = self.profile.save() {
            eprintln!("[chat] 保存插件 profile 失败: {err}");
        }
        std::env::set_var("DSHR_DSH_PLUGINS", self.profile.sidecar_json());
        if !self.busy {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !guard.restart_sidecar() {
                eprintln!("[chat] 重启 sidecar 失败（插件开关已保存，下次启动生效）");
            }
        }
        cx.notify();
    }

    fn plugins_panel(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.plugins_open {
            return None;
        }
        let p = t.palette;
        let entries = self.profile.entries().to_vec();
        let editable = self.profile.can_edit();
        let enabled = entries.iter().filter(|entry| entry.enabled).count();
        let total = entries.len();
        let footer = if editable {
            String::from("切换会重启当前会话的 sidecar；配置写入 DSHR_PROFILE_DIR/plugins.json")
        } else {
            String::from("来自 Loader YAML（DSHR_LOADER_BUNDLE），只读；修改 bundle/patch 后重启")
        };

        let mut list = div().flex().flex_col().gap(px(6.0)).w_full();
        for (index, entry) in entries.iter().enumerate() {
            let name = entry.name.clone();
            let display = name
                .strip_prefix("@deepseek-ai/")
                .unwrap_or(&name)
                .to_owned();
            let is_enabled = entry.enabled;
            let action: AnyElement = if editable {
                let plugin_name = name.clone();
                div()
                    .id(("plugin-toggle", index))
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(t.radii.sm)
                    .border_1()
                    .border_color(p.color(p.border_l2))
                    .cursor_pointer()
                    .text_size(t.type_scale.small)
                    .text_color(if is_enabled {
                        p.color(p.state_error)
                    } else {
                        p.color(p.state_success)
                    })
                    .child(if is_enabled { "禁用" } else { "启用" })
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.toggle_plugin(plugin_name.clone(), cx);
                    }))
                    .into_any_element()
            } else {
                div()
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(t.radii.sm)
                    .border_1()
                    .border_color(p.color(p.border_l2))
                    .text_size(t.type_scale.small)
                    .text_color(p.color(p.label_caption))
                    .child("只读")
                    .into_any_element()
            };
            list = list.child(
                div()
                    .id(("plugin-row", index))
                    .w_full()
                    .px(px(10.0))
                    .py(px(8.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .rounded(t.radii.sm)
                    .border_1()
                    .border_color(p.color(p.border_l1))
                    .bg(p.color(p.bg_layer_1))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .text_size(t.type_scale.small)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.color(p.label_primary))
                                    .text_overflow(TextOverflow::Truncate("…".into()))
                                    .child(display),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(if is_enabled {
                                        p.color(p.state_success)
                                    } else {
                                        p.color(p.label_caption)
                                    })
                                    .child(if is_enabled { "已启用" } else { "已禁用" }),
                            ),
                    )
                    .child(action),
            );
        }

        Some(
            div()
                .w(px(300.0))
                .h_full()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(p.color(p.border_l3))
                .bg(p.color(p.bg_base))
                .child(
                    div()
                        .min_h(t.layout.header_height)
                        .px(px(16.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(p.color(p.border_l3))
                        .child(
                            div()
                                .text_size(t.type_scale.body)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.color(p.label_primary))
                                .child(format!("插件 · {enabled}/{total} 启用")),
                        )
                        .child(
                            div()
                                .id("plugins-close")
                                .px(px(6.0))
                                .rounded(t.radii.sm)
                                .cursor_pointer()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_secondary))
                                .child("关闭")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.plugins_open = false;
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .id("plugin-scroll")
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_y_scroll()
                        .px(px(10.0))
                        .py(px(8.0))
                        .child(list),
                )
                .child(
                    div()
                        .px(px(16.0))
                        .py(px(8.0))
                        .border_t_1()
                        .border_color(p.color(p.border_l3))
                        .text_size(px(11.0))
                        .line_height(px(16.0))
                        .text_color(p.color(p.label_caption))
                        .child(footer),
                )
                .into_any_element(),
        )
    }

    fn session_list(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let sessions = self.sessions.clone();
        let active_id = self.session_id.clone();
        let mut list = div().flex().flex_col().gap(px(2.0)).w_full();

        for (index, session) in sessions.iter().enumerate() {
            let is_active = session.id == active_id;
            let is_renaming = self.renaming.as_deref() == Some(session.id.as_str());

            if is_renaming {
                let input = self.rename_input.clone();
                let row = div()
                    .id(("rename-row", index))
                    .w_full()
                    .px(px(8.0))
                    .py(px(5.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(t.radii.sm)
                    .bg(p.color(p.tag))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_primary))
                            .children(input),
                    )
                    .child(
                        div()
                            .id(("rename-ok", index))
                            .px(px(4.0))
                            .cursor_pointer()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.state_success))
                            .child("✓")
                            .on_click(cx.listener(|this, _, _window, cx| {
                                cx.stop_propagation();
                                this.commit_rename(cx);
                            })),
                    )
                    .child(
                        div()
                            .id(("rename-cancel", index))
                            .px(px(4.0))
                            .cursor_pointer()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_caption))
                            .child("✕")
                            .on_click(cx.listener(|this, _, _window, cx| {
                                cx.stop_propagation();
                                this.cancel_rename(cx);
                            })),
                    );
                list = list.child(row);
                continue;
            }

            let open_id = session.id.clone();
            let is_pending_delete = self.pending_delete.as_deref() == Some(session.id.as_str());
            let actions: AnyElement = if is_pending_delete {
                let confirm_id = session.id.clone();
                let cancel_id = session.id.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        div()
                            .id(("confirm-delete", index))
                            .px(px(4.0))
                            .rounded(t.radii.xs)
                            .cursor_pointer()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.state_error))
                            .child("删除")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.delete_session(confirm_id.clone(), window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id(("cancel-delete", index))
                            .px(px(4.0))
                            .cursor_pointer()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_caption))
                            .child("取消")
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                cx.stop_propagation();
                                let _ = cancel_id;
                                this.pending_delete = None;
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            } else {
                let rename_id = session.id.clone();
                let delete_id = session.id.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(
                        div()
                            .id(("rename", index))
                            .px(px(3.0))
                            .cursor_pointer()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_tertiary))
                            .child("✎")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.start_rename(rename_id.clone(), window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id(("delete", index))
                            .px(px(3.0))
                            .cursor_pointer()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_tertiary))
                            .child("×")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.delete_session(delete_id.clone(), window, cx);
                            })),
                    )
                    .into_any_element()
            };
            let row = div()
                .id(("session-row", index))
                .w_full()
                .px(px(10.0))
                .py(px(7.0))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(6.0))
                .rounded(t.radii.sm)
                .cursor_pointer()
                .when(is_active, |row| row.bg(p.color(p.interactive_hover)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.0))
                        .flex_1()
                        .child(
                            div()
                                .text_size(t.type_scale.small)
                                .line_height(px(18.0))
                                .font_weight(if is_active {
                                    FontWeight::MEDIUM
                                } else {
                                    FontWeight::NORMAL
                                })
                                .text_color(p.color(p.label_primary))
                                .text_overflow(TextOverflow::Truncate("…".into()))
                                .child(session.title.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .line_height(px(15.0))
                                .text_color(p.color(p.label_caption))
                                .child(format!(
                                    "{} 条 · {}",
                                    session.events,
                                    relative_time(session.updated_at)
                                )),
                        ),
                )
                .child(actions)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_session(open_id.clone(), window, cx);
                }));
            list = list.child(row);
        }

        list.into_any_element()
    }

    fn sidebar(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let plugin_total = self.profile.entries().len();
        let plugin_enabled = self
            .profile
            .entries()
            .iter()
            .filter(|entry| entry.enabled)
            .count();
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
                        this.new_session(window, cx);
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
            .child(
                div()
                    .id("session-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .child(self.session_list(t, cx)),
            )
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
                            .id("plugins-toggle")
                            .px(px(12.0))
                            .py(px(6.0))
                            .rounded(t.radii.sm)
                            .cursor_pointer()
                            .text_size(t.type_scale.small)
                            .text_color(if self.plugins_open {
                                p.color(p.label_primary)
                            } else {
                                p.color(p.label_secondary)
                            })
                            .child(if self.profile.can_edit() {
                                format!("插件 · {plugin_enabled}/{plugin_total} 启用（点击管理）")
                            } else {
                                format!("插件 · {plugin_enabled}/{plugin_total}（Loader 只读）")
                            })
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.plugins_open = !this.plugins_open;
                                cx.notify();
                            })),
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
                    .flex_1()
                    .min_w(px(0.0))
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
                            .min_w(px(0.0))
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
                                    .max_w(px(360.0))
                                    .overflow_hidden()
                                    .text_size(t.type_scale.body)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.color(p.label_primary))
                                    .text_overflow(TextOverflow::Truncate("…".into()))
                                    .child(self.active_title()),
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

    fn render_row(&self, row: &Row, t: &Theme, base_font: &Font, base_color: Rgba) -> AnyElement {
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
                            .child(markdown_view(text, t, base_font, base_color))
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

    fn transcript(&mut self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let text_style = window.text_style();
        let base_font = text_style.font();
        let base_color: Rgba = text_style.color.into();
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
        let total = rows.len();
        let start = total.saturating_sub(self.visible_limit);
        if start > 0 {
            let hidden = start;
            column = column.child(
                div()
                    .id("load-earlier")
                    .w_full()
                    .flex()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(t.type_scale.small)
                    .text_color(p.color(p.label_secondary))
                    .child(format!("载入更早的 {hidden} 条（每次 200 条）"))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.visible_limit = this.visible_limit.saturating_add(200);
                        cx.notify();
                    })),
            );
        }
        for row in &rows[start..] {
            column = column.child(self.render_row(row, t, &base_font, base_color));
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

    /// 工具审批卡片：工作线程阻塞在这里，等待"允许一次 / 拒绝"。
    fn approval_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.approvals.pending();
        let first = pending.first()?;
        let p = t.palette;
        let call_id = first.call_id.clone();
        let preview = serde_json::to_string_pretty(&first.arguments).unwrap_or_default();
        let preview = if preview.chars().count() > 260 {
            let mut text: String = preview.chars().take(260).collect();
            text.push('…');
            text
        } else {
            preview
        };
        let call_id_allow = call_id.clone();
        let call_id_deny = call_id.clone();
        let approvals_allow = self.approvals.clone();
        let approvals_deny = self.approvals.clone();
        Some(
            div()
                .flex()
                .flex_col()
                .items_center()
                .px(px(16.0))
                .pb(px(6.0))
                .child(
                    div()
                        .w_full()
                        .max_w(t.layout.composer_max_width)
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .px(px(14.0))
                        .py(px(10.0))
                        .rounded(t.radii.md)
                        .border_1()
                        .border_color(p.color(p.state_warn))
                        .bg(p.color(p.bg_layer_1))
                        .shadow_sm()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .w(px(6.0))
                                        .h(px(6.0))
                                        .rounded_full()
                                        .bg(p.color(p.state_warn)),
                                )
                                .child(
                                    div()
                                        .text_size(t.type_scale.secondary)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.color(p.label_primary))
                                        .child(format!("工具调用需要确认：{}", first.tool)),
                                )
                                .child(div().flex_1())
                                .when(pending.len() > 1, |row| {
                                    row.child(
                                        div()
                                            .text_size(t.type_scale.small)
                                            .text_color(p.color(p.label_caption))
                                            .child(format!("另有 {} 个待确认", pending.len() - 1)),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .max_h(px(80.0))
                                .overflow_hidden()
                                .text_size(t.type_scale.small)
                                .line_height(t.type_scale.small_line)
                                .text_color(p.color(p.label_tertiary))
                                .child(preview),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .id("approval-deny")
                                        .px(px(12.0))
                                        .py(px(5.0))
                                        .rounded(t.radii.sm)
                                        .border_1()
                                        .border_color(p.color(p.border_l2))
                                        .bg(p.color(p.button_elevated))
                                        .cursor_pointer()
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(p.label_primary))
                                        .child("拒绝")
                                        .on_click(cx.listener(move |this, _, _window, cx| {
                                            approvals_deny
                                                .resolve(&call_id_deny, ApprovalOutcome::Deny);
                                            this.pending_approvals = 0;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    div()
                                        .id("approval-allow")
                                        .px(px(12.0))
                                        .py(px(5.0))
                                        .rounded(t.radii.sm)
                                        .bg(p.color(p.button_info))
                                        .cursor_pointer()
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(p.bg_base))
                                        .child("允许一次")
                                        .on_click(cx.listener(move |this, _, _window, cx| {
                                            approvals_allow.resolve(
                                                &call_id_allow,
                                                ApprovalOutcome::AllowOnce,
                                            );
                                            this.pending_approvals = 0;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
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
        // 窄窗口（远程会话 / 小屏）自动收起侧栏，否则 280 的侧栏会把正文挤到窗口外；
        // 插件面板打开时同理，否则 280+300 会吃掉正文宽度。
        let collapsed =
            self.sidebar_collapsed || self.plugins_open || window.bounds().size.width < px(600.0);
        let mut shell = div().flex().size_full().bg(p.color(p.sidebar_fill));
        if !collapsed {
            shell = shell.child(self.sidebar(&theme, cx));
        }
        shell = shell.child(
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
                .child(self.transcript(&theme, window, cx))
                .when_some(self.approval_bar(&theme, cx), |el, bar| el.child(bar))
                .child(self.composer(&theme, cx)),
        );
        shell.when_some(self.plugins_panel(&theme, cx), |el, panel| el.child(panel))
    }
}

fn selftest() -> i32 {
    // 不用系统 TEMP：本机 TEMP 对部分子进程不可写（见 docs/m0-runbook.md）。
    let path = PathBuf::from(".tmp/chat-selftest.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

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
    let mut state = ChatState::new(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

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
    let mut state = ChatState::new(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

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
    let mut state = ChatState::new(path, SESSION_ID, &cancel, ApprovalBroker::new());

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
    let mut state = ChatState::new(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

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

/// 上下文预算自检：塞一份超预算的历史，断言
/// (1) 写了 `context.measured` 事件；(2) 工具结果被裁剪或旧消息被丢弃；
/// (3) 实际发给模型的历史仍然是合法配对（不会以 Tool 开头）。
fn selftest_context() -> i32 {
    let path = PathBuf::from(".tmp/chat-context.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    std::env::set_var("DSHR_CONTEXT_WINDOW", "8000");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    // 直接写日志模拟长会话：CJK 文本 + 大工具结果，确保超过 8000 的 80%。
    for index in 0..20 {
        let user = format!("历史问题 {index}：{}", "长内容".repeat(60));
        let _ = state.log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": format!("h-u{index}"), "text": user }),
        ));
        let call_id = format!("h-c{index}");
        let _ = state.log.append(EventSpec::durable(
            "tool.call",
            "t0",
            json!({ "call_id": call_id, "tool": "probe_echo", "arguments": {} }),
        ));
        let _ = state.log.append(EventSpec::durable(
            "tool.result",
            "t0",
            json!({
                "call_id": call_id, "is_error": false,
                "content": [{ "type": "text", "text": "输出".repeat(800) }]
            }),
        ));
        let _ = state.log.append(EventSpec::durable(
            "message.assistant.settled",
            "t0",
            json!({ "message_id": format!("h-a{index}"), "text": "收到" }),
        ));
    }
    state.save();

    let reply = state.stream_turn("继续", &mut |_| {});
    let measured = state
        .log
        .events()
        .iter()
        .rev()
        .find(|event| event.event_type == "context.measured")
        .cloned();
    let Some(measured) = measured else {
        println!("[selftest-context] FAIL: 没有 context.measured 事件");
        return 1;
    };
    let ratio = measured
        .payload
        .get("ratio")
        .and_then(|value| value.as_f64())
        .unwrap_or(1.0);
    let pruned = measured
        .payload
        .get("pruned_tool_results")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let dropped = measured
        .payload
        .get("dropped_messages")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);

    // 压缩事件 + 摘要消息 + 重建出的 System 摘要都应存在。
    let compacted = state
        .log
        .events()
        .iter()
        .rev()
        .find(|event| event.event_type == "context.compacted")
        .cloned();
    let summary_id = compacted
        .as_ref()
        .and_then(|event| event.payload.get("summary_message_id"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let summary_event = state.log.events().iter().find(|event| {
        event.event_type == "message.assistant.settled"
            && event.payload.get("message_id").and_then(|v| v.as_str()) == Some(&summary_id)
    });
    let history = history_from_log(&state.log);
    let has_system_summary = history
        .iter()
        .any(|message| matches!(message, ChatMessage::System(text) if text.contains("摘要")));
    println!(
        "[context] ratio={ratio:.3} pruned={pruned} dropped={dropped} compacted={} summary_chars={:?} window={:?}",
        compacted.is_some(),
        summary_event
            .and_then(|event| event.payload.get("text"))
            .and_then(|value| value.as_str())
            .map(str::chars)
            .map(Iterator::count),
        measured.payload.get("window")
    );

    // 用同一份日志重建计划，断言配对合法且压缩后不再触发丢弃。
    let plan = dshr_agent_context::plan_context(&state.log, "");
    let legal = !matches!(
        plan.messages.first(),
        Some(dshr_agent_core::ChatMessage::Tool { .. })
    );

    let ok = reply.is_some()
        && ratio <= 0.8
        && compacted.is_some()
        && !summary_id.is_empty()
        && summary_event.is_some()
        && has_system_summary
        && legal
        && plan.dropped_messages == 0
        && plan.tokens_after <= plan.window;
    if ok {
        println!("[selftest-context] PASS");
        0
    } else {
        println!("[selftest-context] FAIL");
        1
    }
}

/// 审批自检：策略对全部工具 Ask，
/// (1) `DSHR_APPROVAL_MODE=deny`：工具不执行、落 approval 事件、工具结果为空错误；
/// (2) `DSHR_APPROVAL_MODE=allow`：审批通过、工具真正执行。
fn selftest_approval() -> i32 {
    let _ = std::fs::create_dir_all(".tmp");
    std::env::set_var("DSHR_TOOL_POLICY", "ask-all");

    let denied_path = PathBuf::from(".tmp/chat-approval-deny.jsonl");
    let _ = std::fs::remove_file(&denied_path);
    std::env::set_var("DSHR_APPROVAL_MODE", "deny");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut denied_state = ChatState::new(denied_path, SESSION_ID, &cancel, ApprovalBroker::new());
    let denied_reply = denied_state.stream_turn("用工具试一下", &mut |_| {});

    let event_count = |state: &ChatState, kind: &str| {
        state
            .log
            .events()
            .iter()
            .filter(|event| event.event_type == kind)
            .count()
    };
    let denied_tool = denied_state
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "tool.aborted");
    let asked = event_count(&denied_state, "approval.asked");
    let answered = event_count(&denied_state, "approval.answered");
    let deny_outcome = denied_state.log.events().iter().any(|event| {
        event.event_type == "approval.answered"
            && event.payload.get("outcome").and_then(|v| v.as_str()) == Some("deny")
    });

    let allowed_path = PathBuf::from(".tmp/chat-approval-allow.jsonl");
    let _ = std::fs::remove_file(&allowed_path);
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let mut allowed_state =
        ChatState::new(allowed_path, SESSION_ID, &cancel, ApprovalBroker::new());
    let allowed_reply = allowed_state.stream_turn("用工具试一下", &mut |_| {});
    let allowed_tool = allowed_state
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "tool.result");
    let allow_outcome = allowed_state.log.events().iter().any(|event| {
        event.event_type == "approval.answered"
            && event.payload.get("outcome").and_then(|v| v.as_str()) == Some("allow_once")
    });

    std::env::remove_var("DSHR_APPROVAL_MODE");
    std::env::remove_var("DSHR_TOOL_POLICY");

    println!(
        "[approval] asked={asked} answered={answered} denied_tool={denied_tool} \
         deny_outcome={deny_outcome} allowed_tool={allowed_tool} allow_outcome={allow_outcome}"
    );
    let ok = denied_reply.is_some()
        && allowed_reply.is_some()
        && denied_tool
        && asked == 1
        && answered == 1
        && deny_outcome
        && allowed_tool
        && allow_outcome;
    if ok {
        println!("[selftest-approval] PASS");
        0
    } else {
        println!("[selftest-approval] FAIL");
        1
    }
}

/// 沙箱 seam 自检：原型写工具走"策略 Ask → 自动批准 → 沙箱写入/快照/路径拒绝"。
fn selftest_sandbox() -> i32 {
    let root =
        std::path::absolute(".tmp/sandbox-ws").unwrap_or_else(|_| PathBuf::from(".tmp/sandbox-ws"));
    let _ = std::fs::remove_dir_all(&root);
    std::env::set_var("DSHR_WORKSPACE", &root);
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let path = PathBuf::from(".tmp/chat-sandbox.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path, SESSION_ID, &cancel, ApprovalBroker::new());

    let write = |call_id: &str, relative: &str, content: &str| ToolCallRequest {
        call_id: call_id.to_owned(),
        tool: "probe_write".to_owned(),
        arguments: json!({ "path": relative, "content": content }),
    };
    let first = state.execute_tool(&write("c-w1", "demo.txt", "hello sandbox"));
    let second = state.execute_tool(&write("c-w2", "demo.txt", "v2"));
    let escaped = state.execute_tool(&write("c-w3", "../escape.txt", "nope"));

    let file = root.join("demo.txt");
    let content = std::fs::read_to_string(&file).unwrap_or_default();
    let snapshots = state
        .approvals
        .take_records()
        .into_iter()
        .filter(|record| record.tool == "probe_write")
        .count();
    let snapshot_files = std::fs::read_dir(root.join(".dshr").join("snapshots"))
        .map(|entries| entries.count())
        .unwrap_or(0);

    std::env::remove_var("DSHR_APPROVAL_MODE");
    std::env::remove_var("DSHR_WORKSPACE");

    println!(
        "[sandbox] write_ok={} overwrite_ok={} denied={} content={content:?} approval_records={snapshots} snapshot_files={snapshot_files} first_err={:?} second_err={:?}",
        !first.is_error,
        !second.is_error,
        escaped.is_error,
        first.content,
        second.content
    );
    let ok = !first.is_error
        && !second.is_error
        && escaped.is_error
        && content == "v2"
        && snapshots >= 2
        && snapshot_files >= 1;
    if ok {
        println!("[selftest-sandbox] PASS");
        0
    } else {
        println!("[selftest-sandbox] FAIL");
        1
    }
}

/// 插件 profile 自检：默认值、启停持久化、sidecar JSON、以及（有 vendored 依赖时）
/// 禁用 fs 后 read 工具消失、重新启用后回来。
fn selftest_profile() -> i32 {
    let _ = std::fs::create_dir_all(".tmp");
    let dir = PathBuf::from(".tmp/profile-selftest");
    let _ = std::fs::remove_dir_all(&dir);
    let mut profile = Profile::load(&dir).expect("load profile");
    let defaults_ok = profile.entries().len() == 2;
    let all_json = profile.sidecar_json();
    let disable_ok = profile.set_enabled("@deepseek-ai/dsh-tool-fs", false);
    if let Err(err) = profile.save() {
        println!("[selftest-profile] 保存失败: {err}");
        return 1;
    }
    let disabled_json = profile.sidecar_json();
    let reloaded = Profile::load(&dir).expect("reload profile");
    let persisted = reloaded
        .entries()
        .iter()
        .any(|entry| entry.name == "@deepseek-ai/dsh-tool-fs" && !entry.enabled);
    let json_ok = all_json.contains("dsh-tool-fs") && !disabled_json.contains("dsh-tool-fs");

    let vendored = PathBuf::from("sidecar/dsh-official/node_modules").exists();
    let mut sidecar_ok = true;
    if vendored {
        std::env::set_var("DSHR_SIDECAR", "dsh");
        std::env::set_var("DSHR_DSH_PLUGINS", disabled_json.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        let without_fs = ChatState::new(
            PathBuf::from(".tmp/chat-profile-without-fs.jsonl"),
            SESSION_ID,
            &cancel,
            ApprovalBroker::new(),
        );
        let read_absent = without_fs.plugins.registry().get("read").is_none();
        let workspace_present = without_fs
            .plugins
            .registry()
            .get("load_workspace_dependencies")
            .is_some();

        let mut reopened = Profile::load(&dir).expect("reload");
        reopened.set_enabled("@deepseek-ai/dsh-tool-fs", true);
        let _ = reopened.save();
        std::env::set_var("DSHR_DSH_PLUGINS", reopened.sidecar_json());
        let with_fs = ChatState::new(
            PathBuf::from(".tmp/chat-profile-with-fs.jsonl"),
            SESSION_ID,
            &cancel,
            ApprovalBroker::new(),
        );
        let read_present = with_fs.plugins.registry().get("read").is_some();
        sidecar_ok = read_absent && workspace_present && read_present;
        println!(
            "[profile] vendored=true read_absent={read_absent} workspace_present={workspace_present} read_present={read_present}"
        );
    } else {
        println!("[profile] vendored=false（跳过 sidecar 端到端）");
    }

    let ok = defaults_ok && disable_ok && persisted && json_ok && sidecar_ok;
    println!(
        "[profile] defaults_ok={defaults_ok} disable_ok={disable_ok} persisted={persisted} json_ok={json_ok}"
    );
    if ok {
        println!("[selftest-profile] PASS");
        0
    } else {
        println!("[selftest-profile] FAIL");
        1
    }
}

/// Loader YAML 自检：bundle 条目 + patch 覆盖/插入 → 只读 profile。
fn selftest_loader() -> i32 {
    const BUNDLE: &str = r#"
- name: "@deepseek-ai/dsh-tool-workspace-dependencies"
  config:
    source: "C:/dsh/primary-runtime"
- id: fs-tools
  name: "@deepseek-ai/dsh-tool-fs"
  inject: [tools, fs, systemPrompt]
  config:
    read: true
    write: false
"#;
    const PATCH: &str = r#"
- id: fs-tools
  disabled: true
  config:
    read: true
    write: true
- insert:
    - name: "@deepseek-ai/dsh-tool-extra"
      config:
        limit: 3
"#;

    let dir = PathBuf::from(".tmp/loader-selftest");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let bundle = dir.join("cordis.yml");
    let patch = dir.join("cordis.patch.yml");
    if let Err(err) = std::fs::write(&bundle, BUNDLE) {
        println!("[selftest-loader] 写 bundle 失败: {err}");
        return 1;
    }
    if let Err(err) = std::fs::write(&patch, PATCH) {
        println!("[selftest-loader] 写 patch 失败: {err}");
        return 1;
    }

    let profile = match Profile::from_loader(&dir, &bundle, &[patch]) {
        Ok(profile) => profile,
        Err(err) => {
            println!("[selftest-loader] 加载失败: {err}");
            return 1;
        }
    };
    let entries = profile.entries();
    let fs = entries
        .iter()
        .find(|entry| entry.name == "@deepseek-ai/dsh-tool-fs");
    let extra = entries
        .iter()
        .find(|entry| entry.name == "@deepseek-ai/dsh-tool-extra");
    let json = profile.sidecar_json();

    let ok = profile.source() == ProfileSource::Loader
        && !profile.can_edit()
        && profile.save().is_err()
        && fs.is_some_and(|entry| !entry.enabled && entry.config["write"] == true)
        && extra.is_some()
        && !json.contains("dsh-tool-fs")
        && json.contains("dsh-tool-extra");
    println!(
        "[loader] entries={} fs_disabled={:?} extra={} json_has_fs={} read_only={}",
        entries.len(),
        fs.map(|entry| !entry.enabled),
        extra.is_some(),
        json.contains("dsh-tool-fs"),
        !profile.can_edit()
    );
    if ok {
        println!("[selftest-loader] PASS");
        0
    } else {
        println!("[selftest-loader] FAIL");
        1
    }
}

/// 离线安装插件：`chat --install-plugin <本地包目录>`。
///
/// 复制到 `sidecar/dsh-official/node_modules/<name>` 并追加 profile 条目；
/// 不解析 npm 依赖（包需要在本地已完整）。
fn install_plugin_cli(package: PathBuf) -> i32 {
    let root = std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .unwrap_or_else(|_| PathBuf::from("."));
    let sidecar_dir = std::env::var("DSHR_SIDECAR_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("sidecar/dsh-official"));
    if let Err(err) = std::fs::create_dir_all(&sidecar_dir) {
        eprintln!("[install] 创建 sidecar 目录失败: {err}");
        return 1;
    }
    let profile_dir = std::env::var("DSHR_PROFILE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".tmp/profile"));
    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[install] 加载 profile 失败: {err}");
            return 1;
        }
    };
    match install_package(&sidecar_dir, &package, &mut profile) {
        Ok(report) => {
            println!(
                "[install] {} -> {}（profile_added={}）",
                report.name,
                report.destination.display(),
                report.added_to_profile
            );
            println!(
                "[install] profile={}；重启 chat 后加载",
                profile.path().display()
            );
            0
        }
        Err(err) => {
            eprintln!("[install] 安装失败: {err}");
            1
        }
    }
}

/// 安装流程自检：本地包复制 + profile 追加 + 重启后可读。
fn selftest_install() -> i32 {
    let root = PathBuf::from(".tmp/install-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(root.join("sidecar"));
    let package = root.join("pkg");
    let _ = std::fs::create_dir_all(package.join("lib"));
    let _ = std::fs::write(
        package.join("package.json"),
        json!({ "name": "@deepseek-ai/dsh-tool-selftest", "version": "1.0.0" }).to_string(),
    );
    let _ = std::fs::write(package.join("lib/index.js"), "export const ok = true\n");

    let mut profile = match Profile::load(root.join("profile")) {
        Ok(profile) => profile,
        Err(err) => {
            println!("[selftest-install] 加载 profile 失败: {err}");
            return 1;
        }
    };
    let report = match install_package(&root.join("sidecar"), &package, &mut profile) {
        Ok(report) => report,
        Err(err) => {
            println!("[selftest-install] 安装失败: {err}");
            return 1;
        }
    };
    let copied = report.destination.join("lib/index.js").exists();
    let reloaded = Profile::load(root.join("profile")).ok();
    let in_profile = reloaded.is_some_and(|profile| {
        profile
            .entries()
            .iter()
            .any(|entry| entry.name == "@deepseek-ai/dsh-tool-selftest" && entry.enabled)
    });

    println!(
        "[install] name={} copied={copied} profile_added={} in_profile={in_profile}",
        report.name, report.added_to_profile
    );
    let ok = copied && report.added_to_profile && in_profile;
    if ok {
        println!("[selftest-install] PASS");
        0
    } else {
        println!("[selftest-install] FAIL");
        1
    }
}
/// 重试/故障注入自检：每第 3 次请求返回 503，20 轮全部应成功。
struct FlakyProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    fail_every: usize,
}

impl ModelProvider for FlakyProvider {
    fn complete(
        &mut self,
        _request: &CompletionRequest,
    ) -> Result<dshr_agent_core::ModelReply, String> {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if self.fail_every > 0 && call.is_multiple_of(self.fail_every) {
            return Err(String::from("HTTP 503 temporarily unavailable"));
        }
        Ok(dshr_agent_core::ModelReply {
            text: format!("ok-{call}"),
            tool_calls: Vec::new(),
        })
    }
}

struct NoopExecutor;

impl ToolExecutor for NoopExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        Vec::new()
    }

    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        ToolOutcome::failure(format!("unexpected tool {}", call.tool))
    }
}

fn selftest_retry() -> i32 {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let flaky = FlakyProvider {
        calls: calls.clone(),
        fail_every: 3,
    };
    let mut agent = Agent::new(RetryingProvider::new(
        Box::new(flaky),
        RetryPolicy::immediate(4),
    ));
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    for _ in 0..20 {
        let events = agent.run_turn("继续", &mut NoopExecutor);
        if events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnFailed(_)))
        {
            failed += 1;
        } else {
            succeeded += 1;
        }
    }
    let total_calls = calls.load(std::sync::atomic::Ordering::Relaxed);
    println!(
        "[retry] turns=20 succeeded={succeeded} failed={failed} provider_calls={total_calls} \
         (fault every 3rd call, max attempts 4)"
    );
    let ok = failed == 0 && succeeded == 20 && total_calls > 20;
    if ok {
        println!("[selftest-retry] PASS");
        0
    } else {
        println!("[selftest-retry] FAIL");
        1
    }
}

/// 10k 事件性能基线：冷加载 + 回放 + 快照 + 历史重建（M2 gate 2 的数据层）。
fn selftest_perf() -> i32 {
    let mut log = SessionLog::new("s-perf");
    for index in 0..2_500 {
        let user = format!("第 {index} 轮问题：帮我检查一下这个方案的风险点与回滚方式。");
        let assistant = format!("第 {index} 轮回答：已完成检查，主要风险在并发写入与持久化顺序。");
        log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": format!("u{index}"), "text": user }),
        ))
        .expect("append user");
        log.append(EventSpec::durable(
            "message.assistant.settled",
            "t0",
            json!({ "message_id": format!("a{index}"), "text": assistant }),
        ))
        .expect("append assistant");
        log.append(EventSpec::durable(
            "tool.call",
            "t0",
            json!({
                "call_id": format!("c{index}"),
                "tool": "read",
                "arguments": { "path": "README.md" }
            }),
        ))
        .expect("append tool.call");
        log.append(EventSpec::durable(
            "tool.result",
            "t0",
            json!({
                "call_id": format!("c{index}"),
                "is_error": false,
                "content": [{ "type": "text", "text": "ok" }]
            }),
        ))
        .expect("append tool.result");
    }
    assert_eq!(log.len(), 10_000);

    let jsonl = log.to_jsonl();
    let bytes = jsonl.len();
    let started = std::time::Instant::now();
    let reloaded = SessionLog::from_jsonl("s-perf", &jsonl).expect("reload");
    let load_ms = started.elapsed().as_millis();

    let started = std::time::Instant::now();
    let items = build_from_log(&reloaded);
    let build_ms = started.elapsed().as_millis();

    let started = std::time::Instant::now();
    let rows = rows_snapshot(&reloaded, None);
    let snapshot_ms = started.elapsed().as_millis();

    let started = std::time::Instant::now();
    let history = history_for_model(&reloaded);
    let history_ms = started.elapsed().as_millis();

    let cold_ms = load_ms + build_ms;
    println!(
        "[perf] events=10000 jsonl_bytes={bytes} load_ms={load_ms} build_ms={build_ms} \
         snapshot_ms={snapshot_ms} history_ms={history_ms} cold_ms={cold_ms} \
         items={} rows={} history_messages={}",
        items.len(),
        rows.len(),
        history.len()
    );
    let ok = reloaded.len() == 10_000
        && items.len() == 7_500
        && rows.len() == items.len()
        && history.len() <= 60
        && cold_ms < 1_500;
    if ok {
        println!("[selftest-perf] PASS");
        0
    } else {
        println!("[selftest-perf] FAIL");
        1
    }
}

/// 多轮真实对话（需要 API key）：验证"多轮记忆"而不是单轮应答。
/// 用法：`chat --live-chat "第一句" "第二句" ...`（每个参数是一轮用户消息）
fn live_chat(script: &[String]) -> i32 {
    let path = PathBuf::from(".tmp/chat-live-chat.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

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
    if std::env::args().any(|arg| arg == "--selftest-context") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_context());
    }
    if std::env::args().any(|arg| arg == "--selftest-approval") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_approval());
    }
    if std::env::args().any(|arg| arg == "--selftest-sandbox") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_sandbox());
    }
    if std::env::args().any(|arg| arg == "--selftest-profile") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_profile());
    }
    if std::env::args().any(|arg| arg == "--selftest-loader") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_loader());
    }
    if std::env::args().any(|arg| arg == "--selftest-install") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_install());
    }
    if std::env::args().any(|arg| arg == "--selftest-retry") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_retry());
    }
    if std::env::args().any(|arg| arg == "--selftest-perf") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_perf());
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--install-plugin") {
        let Some(path) = std::env::args().nth(index + 1) else {
            eprintln!("用法: chat --install-plugin <本地包目录>");
            std::process::exit(2);
        };
        std::process::exit(install_plugin_cli(PathBuf::from(path)));
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

    // 会话目录：默认 `.tmp/sessions`，可用 DSHR_SESSIONS_DIR 覆盖（测试/打包用）。
    let dir = std::env::var("DSHR_SESSIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".tmp/sessions"));
    let store = SessionIndex::new(dir);
    if let Err(err) = store.ensure_dir() {
        eprintln!("[chat] 无法创建会话目录: {err}");
        std::process::exit(1);
    }

    // 迁移旧版单会话文件，避免升级后历史看起来“消失”。
    let legacy = PathBuf::from(".tmp/chat-session.jsonl");
    if store
        .scan()
        .map(|sessions| sessions.is_empty())
        .unwrap_or(false)
        && legacy.exists()
    {
        let target = store.path_for(SESSION_ID);
        if !target.exists() {
            let _ = std::fs::copy(&legacy, &target);
        }
    }

    let mut sessions = store.scan().unwrap_or_default();
    if sessions.is_empty() {
        let id = store.new_id();
        if let Err(err) = store.create(&id) {
            eprintln!("[chat] 无法创建初始会话: {err}");
            std::process::exit(1);
        }
        sessions = store.scan().unwrap_or_default();
    }
    let active = sessions
        .first()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| SESSION_ID.to_owned());
    // 插件 profile：启停状态持久化到 DSHR_PROFILE_DIR/plugins.json，并注入 sidecar。
    let profile_dir = std::env::var("DSHR_PROFILE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".tmp/profile"));
    // 两种来源：Loader bundle+patch（只读派生视图），或 plugins.json（可编辑）。
    let profile = if let Ok(bundle) = std::env::var("DSHR_LOADER_BUNDLE") {
        let patches: Vec<PathBuf> = std::env::var("DSHR_LOADER_PATCH")
            .ok()
            .filter(|path| !path.is_empty())
            .map(|path| vec![PathBuf::from(path)])
            .unwrap_or_default();
        Profile::from_loader(&profile_dir, &PathBuf::from(bundle), &patches).unwrap_or_else(|err| {
            eprintln!("[chat] 加载 Loader bundle 失败（使用默认值）: {err}");
            Profile::defaults(&profile_dir)
        })
    } else {
        Profile::load(&profile_dir).unwrap_or_else(|err| {
            eprintln!("[chat] 加载插件 profile 失败（使用默认值）: {err}");
            Profile::defaults(&profile_dir)
        })
    };
    std::env::set_var("DSHR_DSH_PLUGINS", profile.sidecar_json());

    let path = store.path_for(&active);
    let cancel = Arc::new(AtomicBool::new(false));
    let approvals = ApprovalBroker::new();
    let state = Arc::new(Mutex::new(ChatState::new(
        path,
        &active,
        &cancel,
        approvals.clone(),
    )));
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
        let store = store;
        let sessions = sessions;
        let active = active;
        let approvals = approvals;
        let profile = profile;
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
                    store,
                    sessions,
                    session_id: active,
                    renaming: None,
                    rename_input: None,
                    pending_delete: None,
                    approvals,
                    pending_approvals: 0,
                    profile,
                    plugins_open: false,
                    visible_limit: 200,
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
                // 调试钩子：绕过输入法/鼠标，直接发一条消息（验证审批卡片等）。
                if let Ok(text) = std::env::var("DSHR_UI_AUTOSEND") {
                    app.update(cx, |chat, cx| {
                        chat.on_submit(&text, cx);
                    });
                }
                app
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
