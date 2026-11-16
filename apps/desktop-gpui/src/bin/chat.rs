// release 构建是 Windows GUI 子系统：双击/快捷方式启动不弹命令行窗口。
// debug 构建保留控制台子系统，方便 `cargo run` 直接看日志。
// CLI 用法（--selftest 等）在 release 下通过 AttachConsole 接回父控制台（见 win_console）。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

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
//!   cargo run ... --bin chat -- --selftest-adc-panel         # ADC 插件面板链路自检
//!   cargo run ... --bin chat -- --selftest-wasm              # WASM 插件链路自检（安装→装载→调用→trap→卸载）
//!   cargo run ... --bin chat -- --selftest-defaults          # M3 默认差异接线自检
//!   cargo run ... --bin chat -- --selftest-plugin-lifecycle  # M4：安装→卸载→密钥轮换自检
//!   cargo run ... --bin chat -- --uninstall-plugin <name>    # 卸载插件（目录/profile/审计）
//!   cargo run ... --bin chat -- --trust-key <action> ...     # 信任密钥库管理
//!   cargo run ... --bin chat -- --live "一句话"               # 真实 LLM 单轮（需 API key）

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::profiler::{set_trace_enabled, FrameEvent, FrameTimingCollector};
use gpui::{
    actions, div, point, px, size, AnyElement, App, Bounds, ClipboardItem, Context, Entity,
    Focusable, Font, FontWeight, KeyBinding, Rgba, Role, ScrollHandle, TextOverflow, Window,
    WindowBounds, WindowOptions,
};
use gpui_platform::application;
use serde_json::json;

use dshr_agent_context::{
    estimate_tokens, history_for_model, history_from_log, plan_context, plan_context_with_spill,
    ContextPlan, SpillPolicy, SpillStore,
};
use dshr_agent_core::{
    spawn_child_spec, Agent, AgentEvent, ChatMessage, CompletionRequest, MockProvider,
    ModelProvider, ModelReply, ModelToolCall, ReasoningEffort, SessionTreeRuntime, SpawnChildArgs,
    SpawnContext, ToolCallRequest, ToolExecutor, ToolOutcome, ToolSpec, DEFAULT_MAX_DEPTH,
    SPAWN_CHILD_TOOL,
};
use dshr_compat_dsh_fs::FsWriteService;
use dshr_compat_dsh_host::{
    parse_tool_registration, Envelope, EventPage, HostOptions, HostServiceRouter, ServiceError,
    SessionAccess, SessionEventView, SessionHeaderView, SessionLogService, SidecarHost,
    StorageKvService, WorkingDirectoryService, CAP_SERVICE_PROXY, PROTOCOL_MAX, SESSION_LOG_V1,
    STORAGE_KV_V1, WORKING_DIRECTORY_V1,
};
use dshr_conversation_model::{build_from_log, CardKind, Item, ToolCard};
use dshr_llm_deepseek::DeepSeekProvider;
use dshr_llm_openai::OpenAiProvider;
use dshr_llm_retry::{RetryPolicy, RetryingProvider};
use dshr_plugin_api::manifest::RuntimeKind;
use dshr_plugin_api::{HostVersion, Manifest, Plugin, PluginContext, PluginError};
use dshr_plugin_host::{ExternalPlugin, ExternalPluginSpec, PluginHost};
use dshr_profile::{
    add_trust_key, install_package, install_package_trusted, package_digest, read_installed_meta,
    remove_trust_key, revoke_trust_key, rotate_trust_key, uninstall_package, Profile,
    ProfileSource, TrustKey, TrustPolicy, TrustStore,
};
use dshr_sandbox::Workspace;
use dshr_session_export::{export_session, verify_export, ExportOptions, SpillStoreSource};
use dshr_session_store::{
    quarantine_corrupt, recover_backup, save_atomic, EventSpec, SessionCreated, SessionIndex,
    SessionLog, SessionMeta, ToolState,
};
use dshr_sidecar_guard::GuardLimits;
use dshr_tool_runtime::{
    AllowAll, ApprovalBroker, ApprovalOutcome, ApprovalPolicy, PermissionMode, PolicyDecision,
    RegistryError, ToolDefinition, ToolOrigin, ToolPolicy,
};
use dshr_ui_contributions::parse_action;
use dshr_ui_gpui::{
    adc_panel_body, adc_panel_body_with_handler, diff_view_from_text, markdown_view,
    terminal_view_from_text, Action as AdcAction, AdcActionHandler, DiagnosticCode, TextInput,
    Theme, ThemeKind,
};

const SESSION_ID: &str = "s-chat";

// M4 W2：键盘优先的操作。键位在 `chat_key_bindings()` 注册，语义在 ChatApp 的
// `shortcut_*` 方法里；不使用鼠标也能覆盖"新建→输入→发送→审批→面板→关闭"。
actions!(
    dshr_chat,
    [
        NewSessionShortcut,
        FocusComposerShortcut,
        ToggleSidebarShortcut,
        TogglePluginsShortcut,
        ApproveFirstShortcut,
        DenyFirstShortcut,
        EscapeShortcut,
        QuitShortcut,
    ]
);

/// `DSHR_UI_TRACE=1` 时把快捷键触发写到 stderr，供真实窗口人工/自动化复验。
fn trace_shortcut(action: &str) {
    if std::env::var("DSHR_UI_TRACE").as_deref() == Ok("1") {
        eprintln!("[keyboard] {action}");
    }
}

/// 应用里展示/自检用的快捷键说明（键位字符串与 `chat_key_bindings()` 保持一致）。
const CHAT_SHORTCUTS: [(&str, &str); 8] = [
    ("cmd-n", "新建会话"),
    ("cmd-k", "聚焦输入框"),
    ("cmd-b", "展开/收起侧栏"),
    ("cmd-j", "打开/关闭插件面板"),
    ("cmd-shift-a", "允许第一个待审批工具"),
    ("cmd-shift-d", "拒绝第一个待审批工具"),
    ("escape", "关闭面板/取消重命名/回到输入框"),
    ("cmd-w", "关闭应用"),
];

fn chat_key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-n", NewSessionShortcut, None),
        KeyBinding::new("cmd-k", FocusComposerShortcut, None),
        KeyBinding::new("cmd-b", ToggleSidebarShortcut, None),
        KeyBinding::new("cmd-j", TogglePluginsShortcut, None),
        KeyBinding::new("cmd-shift-a", ApproveFirstShortcut, None),
        KeyBinding::new("cmd-shift-d", DenyFirstShortcut, None),
        KeyBinding::new("escape", EscapeShortcut, None),
        KeyBinding::new("cmd-w", QuitShortcut, None),
    ]
}

/// 把 keymap 语法（`cmd-shift-a`）转成 UI 文案（`Ctrl+Shift+A`）。
fn format_shortcut(key: &str, primary: &str) -> String {
    key.split('-')
        .map(|part| match part {
            "cmd" => primary.to_owned(),
            "ctrl" => String::from("Ctrl"),
            "shift" => String::from("Shift"),
            "alt" => String::from("Alt"),
            "escape" => String::from("Esc"),
            other if other.chars().count() == 1 => other.to_uppercase(),
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// 输入框下方的一行快捷键提示（与 `CHAT_SHORTCUTS` 同源，避免文档漂移）。
fn chat_shortcut_hint() -> String {
    let primary = if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    };
    let label = |index: usize| format_shortcut(CHAT_SHORTCUTS[index].0, primary);
    format!(
        "{} 新建 · {} 插件 · {}/{} 审批",
        label(0),
        label(3),
        label(4),
        label(5)
    )
}

/// 非空环境变量 → 路径。空字符串按"未设置"处理（Windows 上 `set VAR=` 是常见写法）。
fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// 只在用户没显式设置时写入默认值；自检/测试里显式设置的 env 永远优先。
fn set_env_default(key: &str, value: impl Into<std::ffi::OsString>) {
    let missing = std::env::var_os(key)
        .map(|existing| existing.is_empty())
        .unwrap_or(true);
    if missing {
        std::env::set_var(key, value.into());
    }
}

/// 目录可写探测：创建一个短命探针文件。Program Files 等位置会在这里被排除。
fn is_writable_dir(dir: &std::path::Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".dshr-write-probe-{}", std::process::id()));
    let writable = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
        .is_ok();
    if writable {
        let _ = std::fs::remove_file(&probe);
    }
    writable
}

/// 数据根来源（诊断/单测用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataRootSource {
    /// 用户显式 `DSHR_HOME`。
    Home,
    /// 打包布局：exe 旁边有 `sidecar/` 且目录可写。
    ExeDir,
    /// 默认 `%LOCALAPPDATA%\dshr`。
    LocalAppData,
}

/// 纯选择逻辑：`DSHR_HOME` 优先；打包布局（exe 目录含 `sidecar/` 且可写）其次；
/// 否则 `%LOCALAPPDATA%\dshr`。创建/回退不在这个函数里做，便于单测。
fn pick_data_root(
    dshr_home: Option<&std::path::Path>,
    exe_dir: Option<&std::path::Path>,
    local_app_data: Option<&std::path::Path>,
    exe_dir_usable: bool,
) -> Option<(DataRootSource, PathBuf)> {
    if let Some(home) = dshr_home {
        return Some((DataRootSource::Home, home.to_path_buf()));
    }
    if exe_dir_usable {
        if let Some(dir) = exe_dir {
            return Some((DataRootSource::ExeDir, dir.to_path_buf()));
        }
    }
    local_app_data.map(|base| (DataRootSource::LocalAppData, base.join("dshr")))
}

/// 兜底数据根：exe 目录（可写）→ 当前目录（可写）→ 系统临时目录。
fn fallback_data_root(exe_dir: Option<&std::path::Path>) -> PathBuf {
    if let Some(dir) = exe_dir.filter(|dir| is_writable_dir(dir)) {
        return dir.to_path_buf();
    }
    if let Some(cwd) = std::env::current_dir()
        .ok()
        .filter(|dir| is_writable_dir(dir))
    {
        return cwd;
    }
    std::env::temp_dir()
}

/// 解析可写数据根：与 CWD 完全解耦。
///
/// 旧实现把状态目录建在 CWD 下（`.tmp/sessions`）。桌面快捷方式"起始位置"为空时
/// CWD 可能落到 `C:\Windows\System32`，或 exe 被放进 Program Files 等只读位置，
/// 于是建目录直接 `拒绝访问 (os error 5)` 退出——这就是"被 Windows 权限拦住"的可复现根因。
fn resolve_data_root() -> PathBuf {
    let home = non_empty_env_path("DSHR_HOME");
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
    let exe_dir_usable = exe_dir
        .as_deref()
        .is_some_and(|dir| dir.join("sidecar").is_dir() && is_writable_dir(dir));
    let local_app_data = non_empty_env_path("LOCALAPPDATA");

    if let Some((source, path)) = pick_data_root(
        home.as_deref(),
        exe_dir.as_deref(),
        local_app_data.as_deref(),
        exe_dir_usable,
    ) {
        match source {
            DataRootSource::Home => {
                // 显式覆盖优先：创建失败也尊重用户选择，让后面的错误指向用户指定路径。
                if let Err(err) = std::fs::create_dir_all(&path) {
                    eprintln!(
                        "[chat] DSHR_HOME 数据目录不可用: {}（{err}）",
                        path.display()
                    );
                }
                return path;
            }
            _ => {
                // 不只是"能创建"：已存在但当前完整性级别/ACL 不可写的目录也要换。
                let unavailable = match std::fs::create_dir_all(&path) {
                    Ok(()) if is_writable_dir(&path) => return path,
                    Ok(()) => "目录存在但不可写".to_owned(),
                    Err(err) => format!("无法创建: {err}"),
                };
                eprintln!("[chat] 数据目录 {} 不可用（{unavailable}）", path.display());
            }
        }
    }

    let fallback = fallback_data_root(exe_dir.as_deref());
    eprintln!(
        "[chat] 数据根回退到 {}（不依赖当前工作目录；可用 DSHR_HOME 显式指定）",
        fallback.display()
    );
    fallback
}

/// 进程级数据根缓存：`prepare_data_env` 解析一次，其它地方（工具工作区兜底）复用。
static DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// 已解析的数据根；`prepare_data_env` 之前调用时现场解析（单测/非 main 入口）。
fn data_root() -> PathBuf {
    DATA_ROOT.get().cloned().unwrap_or_else(resolve_data_root)
}

/// 把默认状态目录挂到数据根上；必须在 `install_crash_hook()` 之前调用（hook 读 env）。
fn prepare_data_env() {
    let root = resolve_data_root();
    let _ = DATA_ROOT.set(root.clone());
    set_env_default("DSHR_SESSIONS_DIR", root.join("sessions"));
    set_env_default("DSHR_PROFILE_DIR", root.join("profile"));
    set_env_default("DSHR_SPILL_DIR", root.join("spill"));
    set_env_default("DSHR_CRASH_LOG", root.join("crash.log"));
}

/// 自检从只读 CWD（`C:\Windows\System32`、受限打包目录等）启动时，`--selftest*`
/// 使用的 `.tmp/...` 相对路径会失败。自检不依赖用户 CWD，统一切到数据根（可写、与
/// 用户数据同源）；GUI 与 `--install-plugin`/`--export-session` 等命令不切。
fn prepare_selftest_cwd(args: &[String]) {
    if !args.iter().skip(1).any(|arg| arg.starts_with("--selftest")) {
        return;
    }
    // 自检里的工具工作区必须仍是“用户当前目录/仓库根”（dsh 自检会 read README.md），
    // 只把 `.tmp/...` 这类相对路径切到数据根；两者不能一起切。
    if std::env::var_os("DSHR_WORKSPACE").is_none() {
        let workspace = std::env::current_dir()
            .ok()
            .filter(|dir| is_writable_dir(dir))
            .unwrap_or_else(app_root);
        std::env::set_var("DSHR_WORKSPACE", workspace);
    }
    let root = data_root();
    if std::fs::create_dir_all(&root).is_ok() {
        let _ = std::env::set_current_dir(&root);
    }
}

/// 判断本次是命令行用法（自检/导出/安装/显式 --console）还是纯 GUI 双击。
///
/// GUI 启动永远是零参数；任何 `--` 开关都意味着用户从终端跑：需要把输出接回控制台。
fn is_cli_invocation(args: &[String]) -> bool {
    args.iter().skip(1).any(|arg| arg.starts_with("--"))
}

/// 应用资源根：`DSHR_APP_ROOT` → exe 旁含 `sidecar/` 的目录 → 编译期仓库根（开发机）。
/// 打包后整目录拷走即可运行，不再依赖构建机上的源码树。
fn app_root() -> PathBuf {
    if let Some(root) = non_empty_env_path("DSHR_APP_ROOT") {
        return root;
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    {
        if dir.join("sidecar").is_dir() {
            return dir;
        }
    }
    std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Windows release 构建的控制台重建（GUI 子系统没有控制台，CLI 仍要能打印）。
#[cfg(windows)]
#[cfg_attr(debug_assertions, allow(dead_code))]
mod win_console {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, FreeConsole, GetConsoleWindow, GetStdHandle, SetConsoleOutputCP,
        SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// 打开当前控制台的屏幕缓冲区/输入缓冲区设备（`CONOUT$` / `CONIN$`）。
    fn open_console_device(name: &str) -> Option<HANDLE> {
        let handle = unsafe {
            CreateFileW(
                wide(name).as_ptr(),
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        // windows-sys 的 HANDLE 是 *mut c_void，INVALID_HANDLE_VALUE 就是 -1。
        (!handle.is_null() && handle != INVALID_HANDLE_VALUE).then_some(handle)
    }

    fn handle_is_valid(handle: HANDLE) -> bool {
        !handle.is_null() && handle != INVALID_HANDLE_VALUE
    }

    /// 挂到父进程（PowerShell/cmd）的控制台，并把标准句柄装回去。
    ///
    /// - 从终端运行：挂上父控制台 → stdout/stderr 可打印（UTF-8 代码页修中文乱码）；
    /// - 双击启动：父进程是 explorer，挂不上 → 不显示控制台、继续开窗；
    /// - 已被重定向（`Start-Process -RedirectStandardOutput`）：保留继承的管道句柄；
    /// - 启动时已经带控制台（少见）：不再 `FreeConsole`，只补装标准句柄——释放后再
    ///   `AttachConsole(ATTACH_PARENT_PROCESS)` 会被 Windows 拒绝（ERROR_ACCESS_DENIED）。
    ///
    /// 返回是否成功得到可用的控制台（失败不影响 GUI 启动）。
    pub fn attach_parent_console() -> bool {
        if unsafe { GetConsoleWindow() }.is_null() {
            unsafe {
                FreeConsole();
            }
            if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
                return false;
            }
        }

        // 关键：Rust std 每次写 stdio 都会 GetStdHandle，所以 SetStdHandle 立即生效。
        // 已重定向（继承到有效管道）的句柄保持不动。
        for (std_handle, device) in [
            (STD_OUTPUT_HANDLE, "CONOUT$"),
            (STD_ERROR_HANDLE, "CONOUT$"),
            (STD_INPUT_HANDLE, "CONIN$"),
        ] {
            let current = unsafe { GetStdHandle(std_handle) };
            if handle_is_valid(current) {
                continue;
            }
            if let Some(handle) = open_console_device(device) {
                unsafe { SetStdHandle(std_handle, handle) };
            }
        }
        // 默认代码页（中文系统是 936）会把 Rust 写出的 UTF-8 字节显示成乱码。
        unsafe { SetConsoleOutputCP(65001) };
        true
    }
}

/// release GUI 构建里，命令行用法先接回父控制台；debug/非 Windows 保持原样。
#[cfg(all(windows, not(debug_assertions)))]
fn prepare_cli_console(is_cli: bool) {
    if is_cli {
        win_console::attach_parent_console();
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn prepare_cli_console(_is_cli: bool) {}

/// 崩溃诊断：panic 时把位置/消息/工作目录追加到 `DSHR_CRASH_LOG`
/// （默认落在数据根下，见 `prepare_data_env`），再交给默认 hook（stderr 照常输出）。
/// GUI 模式下用户能看到"出过什么错"。
fn install_crash_hook() {
    use std::io::Write;
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut report = format!(
            "=== panic {} ===\nlocation: {:?}\n",
            now_ts(),
            info.location()
        );
        if let Some(message) = info.payload().downcast_ref::<&str>() {
            report.push_str(message);
            report.push('\n');
        } else if let Some(message) = info.payload().downcast_ref::<String>() {
            report.push_str(message);
            report.push('\n');
        }
        if let Ok(cwd) = std::env::current_dir() {
            report.push_str(&format!("cwd: {}\n", cwd.display()));
        }
        let path = std::env::var("DSHR_CRASH_LOG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(".tmp/crash.log"));
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = file.write_all(report.as_bytes());
        }
        eprintln!("[chat] 已记录崩溃诊断到 {}", path.display());
        default(info);
    }));
}

fn now_ts() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("t{nanos}")
}

/// 上下文外部化对象目录：默认在会话目录旁的 `spill/`，可用 `DSHR_SPILL_DIR` 覆盖。
fn spill_dir_for(session_path: &std::path::Path) -> PathBuf {
    if let Ok(dir) = std::env::var("DSHR_SPILL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    session_path
        .parent()
        .map(|parent| parent.join("spill"))
        .unwrap_or_else(|| PathBuf::from(".tmp/spill"))
}

/// 自检重置会话夹具：主文件 + `.bak` + `.corrupt-*` 全部清掉。
///
/// 只删主文件的话，`recover_backup()` 会在下一次自检启动时把上次的备份恢复回来，
/// 造成“新会话里混入旧事件”的测试污染（P1-1 恢复机制的副作用）。
fn reset_session_file(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}.bak", path.display()));
    if let (Some(parent), Some(name)) = (
        path.parent(),
        path.file_name().and_then(|name| name.to_str()),
    ) {
        let prefix = format!("{name}.corrupt-");
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
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

/// 一个已取到的插件 ADC 面板（宿主侧缓存；渲染只读，避免每帧走插件 IPC）。
#[derive(Clone)]
struct AdcPanelView {
    plugin_id: String,
    title: String,
    /// 面板声明的 view 名（`ui.action` 回传时用于定位插件状态）。
    view: String,
    /// `render_panel` 成功时的元素树；失败为 `None`，错误在 `error`。
    tree: Option<serde_json::Value>,
    /// 渲染失败的可读错误（未知 view / 插件不实现 view / 进程错误）。
    error: Option<String>,
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
    /// 上下文外部化存储：超阈值工具结果落盘为内容寻址对象（内容只在磁盘，模型见引用）。
    spill: SpillStore,
    /// 当前会话 id（会话树事件/子会话父链接使用）。
    session_id: String,
    /// 会话目录（子会话文件与导出都基于它）。
    sessions_dir: PathBuf,
    /// L2 Host Service 的会话读写面（None = 该会话不接 sidecar 服务）。
    l2_access: Option<Arc<ChatSessionAccess>>,
    /// 与 provider 共享的停止句柄（子会话继承，用户停止会传播）。
    cancel: Arc<AtomicBool>,
    /// 是否允许派生子会话（子会话本身为 false，保证默认单层）。
    allow_spawn: bool,
    /// sidecar 启动/插件装载/会话加载阶段上报的错误（启动错误提示用）。
    plugin_errors: Vec<String>,
    /// 原会话文件存在但读不出来时置位：禁止 save 覆盖（否则会把读失败当成空会话写回）。
    save_blocked: Option<String>,
    /// 插件 ADC 面板缓存（`workspace.panel` 的 view → 元素树 JSON）。
    adc_panels: Vec<AdcPanelView>,
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
        ctx.register_panel(
            dshr_plugin_api::PanelContribution::new(
                "probe.panel",
                "Probe Panel",
                "workspace.panel",
            )
            // 插件面板的 ADC 视图名（`render_view` 的入参；与注册 id 分开）。
            .with_view("probe.panel"),
        );
        Ok(())
    }

    /// ADC-v1 面板 view：证明"原生插件 → 宿主 → adc_panel_body"的完整产品链路。
    fn render_view(&mut self, view: &str) -> Result<serde_json::Value, PluginError> {
        if view != "probe.panel" {
            return Err(PluginError::Invalid(format!("unknown view `{view}`")));
        }
        Ok(json!({
            "type": "Card",
            "props": {"title": "Probe Panel"},
            "children": [
                {"type": "Text", "props": {"value": "原生插件 ADC 面板（probe_native）"}},
                {"type": "Row", "props": {"gap": 8, "align": "center"}, "children": [
                    {"type": "Badge", "props": {"label": "native", "tone": "success"}},
                    {"type": "Text", "props": {"value": "只读渲染 · 事件留 M4", "style": "caption"}}
                ]}
            ]
        }))
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
        // 工作区根与状态目录同源：CWD 不可写时回退到数据根，避免沙箱初始化 panic。
        let root = workspace_root();
        let workspace = Workspace::new(&root).unwrap_or_else(|err| {
            eprintln!("[chat] 沙箱工作区不可用（回退到数据根）: {err}");
            Workspace::new(data_root().join("workspace")).expect("data root workspace")
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

/// 工具/写入的工作区根：`DSHR_WORKSPACE` → **可写的**当前目录 → 数据根下的 `workspace/`。
///
/// 和状态目录一样必须与 CWD 解耦：桌面快捷方式"起始位置"为空时 CWD 可能是
/// `C:\Windows\System32`，或 exe 被放进 Program Files。旧实现把工具工作区钉在 CWD，
/// 沙箱初始化在不可写的 CWD 上直接失败（`os error 5`），用户看到的就是"启动即退出"。
fn workspace_root() -> PathBuf {
    if let Some(dir) = non_empty_env_path("DSHR_WORKSPACE") {
        return dir;
    }
    if let Some(cwd) = std::env::current_dir()
        .ok()
        .filter(|dir| is_writable_dir(dir))
    {
        return cwd;
    }
    let fallback = data_root().join("workspace");
    eprintln!(
        "[chat] 当前工作目录不可写，工具工作区改用 {}（可用 DSHR_WORKSPACE 指定）",
        fallback.display()
    );
    fallback
}

/// 启动 Sidecar；失败时返回 None（Native 工具不受影响）。
///
/// 三种运行时（`DSHR_SIDECAR`）：
/// - 缺省：probe 侧车（自研最小协议探针）；
/// - `cordis`：真实 Cordis 4.0.5 + 只注册 `ctx.bridge.tool` 的探针插件；
/// - `dsh`：真实 Cordis 4.1.0 + **未修改的官方 DSH 插件**（依赖由 `tools/dsh-vendor.mjs`
///   从本机 DSH 安装中抽取，见 `docs/m2-dsh-official.md`）。
///
/// 启动路径统一走：
/// 1. Node 解析 `DSHR_NODE` → 捆绑 `runtime/node` → PATH（`dshr-sidecar-guard`）；
/// 2. Job Object 包裹（进程数/内存上限 + 随宿主退出，Windows）；
/// 3. 官方 DSH 侧车额外协商 `fs.write`：官方 write/edit/apply_patch 的写盘请求
///    回到 Rust 走审批 + 沙箱（见 `docs/m2-official-write.md`）。
fn spawn_sidecar(
    approvals: Option<Arc<ApprovalBroker>>,
    l2_access: Option<Arc<ChatSessionAccess>>,
) -> Option<SidecarHost> {
    // 资源根按 exe 相对解析：`dist/dshr-dev` 整目录拷走后仍能找到 sidecar/（见 `app_root`）。
    let root = app_root();
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

    let mut options = HostOptions::default();
    match dshr_sidecar_guard::resolve_node() {
        Some(node) => {
            eprintln!(
                "[chat] sidecar node={}（{:?}）",
                node.program.display(),
                node.source
            );
            options = options.with_program(node.program);
        }
        None => eprintln!("[chat] 未找到捆绑 Node，回退到 PATH 上的 node"),
    }
    // 只有官方 DSH 侧车实现了 fs.write.request；其它侧车保持只读能力集。
    // `DSHR_FS_WRITE=0` 可显式关闭官方写工具（只读自检/受限环境），默认开启。
    let write_enabled = std::env::var("DSHR_FS_WRITE")
        .map(|value| value != "0")
        .unwrap_or(true);
    if mode == "dsh" && write_enabled {
        if let Some(approvals) = approvals {
            match FsWriteService::open(workspace_root(), approvals) {
                Ok(service) => options = options.with_fs_write(Arc::new(service)),
                Err(err) => eprintln!("[chat] 官方写工具不可用（沙箱初始化失败）: {err}"),
            }
        }
    }
    // L2 v2 Host Service：只有官方 sidecar 实现了 host.service.*；默认开启协商，
    // `DSHR_HOST_SERVICES=0` 显式关闭（受限环境/回退 M2 行为）。握手协商到 1 时
    // SidecarHost 自动回退 v1，router 完全不会被使用（安全失败）。
    let l2_enabled = std::env::var("DSHR_HOST_SERVICES")
        .map(|value| value != "0")
        .unwrap_or(true);
    if mode == "dsh" && l2_enabled {
        if let Some(access) = l2_access {
            match build_l2_router(access) {
                Ok(router) => options = options.with_host_services(router),
                Err(err) => eprintln!("[chat] L2 host-service 不可用（回退 v1）: {err}"),
            }
        }
    }
    options = options.with_guard(GuardLimits::sidecar_default());
    SidecarHost::spawn_with(script, plugin, options)
        .map_err(|err| eprintln!("[chat] sidecar 不可用（工具调用会显示为 aborted）: {err}"))
        .ok()
}

/// 构造 v2 Host Service Router：`storage-kv/v1` + `working-directory/v1` + `session-log/v1`。
///
/// generation 先写占位值，`SidecarHost` 在 `runtime.ready` 协商到 2 后会写入真实
/// generation（`HostServiceRouter::set_generation`）。
fn build_l2_router(
    access: Arc<ChatSessionAccess>,
) -> Result<Arc<Mutex<HostServiceRouter>>, String> {
    build_l2_router_at(
        access,
        data_root().join("storage"),
        workspace_root(),
        "dsh-official",
    )
}

/// [`build_l2_router`] 的显式路径版本（`--selftest-l2` 用临时目录与 fixture 插件 id，
/// 不污染用户 profile）。
fn build_l2_router_at(
    access: Arc<ChatSessionAccess>,
    storage_root: PathBuf,
    workspace: PathBuf,
    plugin_id: &str,
) -> Result<Arc<Mutex<HostServiceRouter>>, String> {
    let mut router = HostServiceRouter::new(
        "node-pending",
        PROTOCOL_MAX,
        vec![CAP_SERVICE_PROXY.to_owned()],
    )
    .map_err(|error| format!("router: {} ({})", error.message, error.code))?;
    let storage = StorageKvService::open(&storage_root)
        .map_err(|error| format!("storage-kv: {} ({})", error.message, error.code))?;
    router
        .register(Box::new(storage))
        .map_err(|error| format!("register storage-kv: {} ({})", error.message, error.code))?;
    router
        .register(Box::new(WorkingDirectoryService::new(
            access.clone(),
            workspace,
        )))
        .map_err(|error| {
            format!(
                "register working-directory: {} ({})",
                error.message, error.code
            )
        })?;
    router
        .register(Box::new(SessionLogService::new(access)))
        .map_err(|error| format!("register session-log: {} ({})", error.message, error.code))?;
    router.register_plugin_inject(
        plugin_id,
        vec![
            STORAGE_KV_V1.to_owned(),
            WORKING_DIRECTORY_V1.to_owned(),
            SESSION_LOG_V1.to_owned(),
        ],
    );
    Ok(Arc::new(Mutex::new(router)))
}

/// 给没有取消概念的 provider（mock）补上取消 token。
///
/// 无 key/自检场景仍要验证“停止”：Agent 通过 `provider.cancel_token()` 检查取消，
/// MockProvider 自己不带 token，所以在这里包一层（真实 provider 使用各自的 token）。
struct CancelAwareProvider<P> {
    inner: P,
    cancel: Arc<AtomicBool>,
}

impl<P: ModelProvider> ModelProvider for CancelAwareProvider<P> {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        self.inner.complete(request)
    }

    fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
        Some(self.cancel.clone())
    }

    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        self.inner.complete_streaming(request, on_delta)
    }
}

fn cancel_aware(
    provider: impl ModelProvider + 'static,
    cancel: &Arc<AtomicBool>,
) -> Box<dyn ModelProvider> {
    Box::new(CancelAwareProvider {
        inner: provider,
        cancel: cancel.clone(),
    })
}

/// provider 选择：`DSHR_PROVIDER` 显式指定（mock/deepseek/openai），否则按 key 自动。
///
/// 真实 provider 统一包一层 `RetryingProvider`（429/5xx/连接中断指数退避）；
/// 流式请求只在"还没有交付任何 delta"时重试，避免重复显示。
fn select_provider(cancel: &Arc<AtomicBool>) -> Box<dyn ModelProvider> {
    // reasoning effort 由 provider 自己在 `from_env()` 里读取（`DSHR_REASONING_EFFORT`）；
    // chat 不做覆盖，只在配置了时打印一行便于诊断。DeepSeek 当前不发该字段（厂商差异）。
    if let Some(effort) = ReasoningEffort::from_env() {
        println!(
            "[chat] reasoning_effort={}（由 provider 按 env 应用）",
            effort.as_str()
        );
    }
    // 自检必须确定性：显式要求 mock 时不看环境里的 key。
    if std::env::var("DSHR_FORCE_MOCK").as_deref() == Ok("1") {
        println!("[chat] provider=mock（DSHR_FORCE_MOCK=1）");
        return cancel_aware(MockProvider, cancel);
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
            return cancel_aware(MockProvider, cancel);
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
            return cancel_aware(MockProvider, cancel);
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
            return cancel_aware(MockProvider, cancel);
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
    cancel_aware(MockProvider, cancel)
}

/// sidecar 工具调用超时：按工具声明（`ToolDefinition.timeout_ms`，对齐 DSH 的 `timeoutMs`）
/// 而不是写死 5s。`DSHR_TOOL_TIMEOUT_MS` 可整体覆盖（诊断/慢机器），统一 clamp。
fn sidecar_tool_timeout(definition: &ToolDefinition) -> Duration {
    let override_ms = std::env::var("DSHR_TOOL_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    clamp_tool_timeout_ms(override_ms.unwrap_or(definition.timeout_ms))
}

/// 超时夹到 1s..=600s：太小会误伤正常工具，太大（或 0）会让界面像卡死。
fn clamp_tool_timeout_ms(millis: u64) -> Duration {
    Duration::from_millis(millis.clamp(1_000, 600_000))
}

/// 把 agent 本轮触发的重复工具软提醒落成 durable `tool.repeat` 事件。
///
/// 模型侧的提醒由 `agent-core` 在每步工具执行完后以 system 消息注入（软提醒不失败）；
/// 这里只负责**宿主侧留痕**：回放/审计可见，UI 不需要专门渲染。返回写入条数。
///
/// payload 形状 `{tool, count, threshold, message}`（`message` 即注入模型的提醒文本）。
fn append_repeat_reminders<P: ModelProvider>(
    agent: &mut Agent<P>,
    log: &mut SessionLog,
    ts: &str,
) -> usize {
    let reminders = agent.take_repeat_reminders();
    let written = reminders.len();
    for reminder in reminders {
        let _ = log.append(EventSpec::durable(
            "tool.repeat",
            ts,
            json!({
                "tool": reminder.tool,
                "count": reminder.count,
                "threshold": reminder.threshold,
                "message": reminder.text,
            }),
        ));
    }
    written
}

// ---------------------------------------------------------------------------
// L2 Host Service 的会话读写面（spec/sidecar-rpc.md §13，M3 G3）
// ---------------------------------------------------------------------------

/// `chat` 对 `SessionAccess` 的实现。
///
/// 与 service 层是**同一线程**的同步调用：`ChatExecutor` 在调 sidecar 工具前
/// `publish` 当前内存 `SessionLog` 快照；`working-directory`/`session-log` 的读走快照，
/// 服务的 `append_event` 进队列，工具返回后由 `ChatExecutor` 补写回内存日志。
/// 其它会话的读面落到磁盘（`sessions_dir/<id>.jsonl`），因为插件只在当前会话里执行。
///
/// seq 约定：快照尾部 + 1 递增；publish 与 drain 之间没有其它日志写入（同步单线程），
/// 所以队列事件的 seq 与后续真实日志 seq 一致。
pub struct ChatSessionAccess {
    inner: Mutex<ChatSessionAccessInner>,
}

struct ChatSessionAccessInner {
    sessions_dir: PathBuf,
    published: Option<PublishedSession>,
    pending: Vec<PendingSessionEvent>,
}

#[derive(Clone)]
struct PublishedSession {
    header: SessionHeaderView,
    events: Vec<SessionEventView>,
    next_seq: u64,
}

struct PendingSessionEvent {
    session_id: String,
    event_type: String,
    data: serde_json::Value,
}

impl ChatSessionAccess {
    pub fn new(sessions_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(ChatSessionAccessInner {
                sessions_dir,
                published: None,
                pending: Vec::new(),
            }),
        })
    }

    /// 用当前内存日志刷新服务层可见的快照（在每次 sidecar 工具调用前调用）。
    pub fn publish(&self, log: &SessionLog) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let header = chat_header_from_log(log, &inner.sessions_dir);
        let events: Vec<SessionEventView> = log
            .events()
            .iter()
            .map(|event| SessionEventView {
                seq: event.seq,
                event_type: event.event_type.clone(),
                data: event.payload.clone(),
            })
            .collect();
        let next_seq = events.last().map(|event| event.seq + 1).unwrap_or(1);
        inner.published = Some(PublishedSession {
            header,
            events,
            next_seq,
        });
    }

    /// 取走服务层为 `session_id` 追加的事件（按序写回内存日志）。
    pub fn take_appends(&self, session_id: &str) -> Vec<(String, serde_json::Value)> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut taken = Vec::new();
        let mut keep = Vec::with_capacity(inner.pending.len());
        for pending in inner.pending.drain(..) {
            if pending.session_id == session_id {
                taken.push((pending.event_type, pending.data));
            } else {
                keep.push(pending);
            }
        }
        inner.pending = keep;
        taken
    }

    // ServiceError 携带结构化 detail（与协议层一致的大 Err），这里按 host crate 的
    // 同样取舍保留原类型，不为了 clippy 的尺寸阈值再包一层 Box。
    #[allow(clippy::result_large_err)]
    fn disk_read(
        &self,
        session_id: &str,
    ) -> Result<(SessionHeaderView, Vec<SessionEventView>), ServiceError> {
        let sessions_dir = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.sessions_dir.clone()
        };
        let log = load_session_log(&sessions_dir, session_id)?;
        let header = chat_header_from_log(&log, &sessions_dir);
        let events = log
            .events()
            .iter()
            .map(|event| SessionEventView {
                seq: event.seq,
                event_type: event.event_type.clone(),
                data: event.payload.clone(),
            })
            .collect();
        Ok((header, events))
    }
}

impl SessionAccess for ChatSessionAccess {
    fn header(&self, session_id: &str) -> Result<SessionHeaderView, ServiceError> {
        {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(published) = &inner.published {
                if published.header.id == session_id {
                    return Ok(published.header.clone());
                }
            }
        }
        self.disk_read(session_id).map(|(header, _)| header)
    }

    fn read_events(
        &self,
        session_id: &str,
        from_seq: u64,
        max_events: usize,
    ) -> Result<EventPage, ServiceError> {
        let events = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(published) = &inner.published {
                if published.header.id == session_id {
                    Some(published.events.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };
        let events = match events {
            Some(events) => events,
            None => self.disk_read(session_id)?.1,
        };
        let start = events
            .iter()
            .position(|event| event.seq >= from_seq)
            .unwrap_or(events.len());
        let page: Vec<SessionEventView> =
            events[start..].iter().take(max_events).cloned().collect();
        let eof = start + page.len() >= events.len();
        let next_seq = page.last().map(|event| event.seq + 1).unwrap_or(from_seq);
        Ok(EventPage {
            events: page,
            next_seq,
            eof,
        })
    }

    fn append_event(
        &self,
        session_id: &str,
        event_type: &str,
        data: serde_json::Value,
    ) -> Result<u64, ServiceError> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(published) = inner.published.as_mut() else {
            return Err(ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                "no active session snapshot for host service writes",
            ));
        };
        if published.header.id != session_id {
            return Err(ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                format!("session `{session_id}` is not the active session"),
            ));
        }
        let seq = published.next_seq;
        published.events.push(SessionEventView {
            seq,
            event_type: event_type.to_owned(),
            data: data.clone(),
        });
        published.next_seq += 1;
        inner.pending.push(PendingSessionEvent {
            session_id: session_id.to_owned(),
            event_type: event_type.to_owned(),
            data,
        });
        Ok(seq)
    }

    fn latest_working_directory(&self, session_id: &str) -> Option<String> {
        let events = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.published.as_ref().and_then(|published| {
                (published.header.id == session_id).then(|| published.events.clone())
            })
        };
        let events = match events {
            Some(events) => events,
            None => self.disk_read(session_id).ok()?.1,
        };
        events
            .iter()
            .rev()
            .find(|event| event.event_type == "working-directory/change")
            .and_then(|event| event.data.get("cwd"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }
}

#[allow(clippy::result_large_err)]
fn load_session_log(
    sessions_dir: &std::path::Path,
    session_id: &str,
) -> Result<SessionLog, ServiceError> {
    let path = sessions_dir.join(format!("{session_id}.jsonl"));
    if !path.exists() {
        return Err(ServiceError::new(
            "E_SERVICE_UNAVAILABLE",
            format!("unknown session `{session_id}`"),
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|error| {
        ServiceError::new(
            "E_INTERNAL",
            format!("read session `{session_id}`: {error}"),
        )
    })?;
    SessionLog::from_jsonl(session_id, &text).map_err(|error| {
        ServiceError::new(
            "E_INTERNAL",
            format!("decode session `{session_id}`: {error}"),
        )
    })
}

fn chat_header_from_log(log: &SessionLog, sessions_dir: &std::path::Path) -> SessionHeaderView {
    let created = log.created();
    SessionHeaderView {
        id: log.session_id().to_owned(),
        cwd: created
            .and_then(|created| created.payload().get("workspace_id").cloned())
            .and_then(|workspace| workspace.as_str().map(str::to_owned)),
        created_at: log
            .events()
            .first()
            .map(|event| event.ts.clone())
            .unwrap_or_default(),
        parent_session_id: created.and_then(|created| created.parent_session_id.clone()),
        delegation_depth: session_depth(log, sessions_dir),
    }
}

/// 从 `session.created.parent_session_id` 链推导深度（根 0）；环形/断链时停止。
fn session_depth(log: &SessionLog, sessions_dir: &std::path::Path) -> u32 {
    let mut depth = 0u32;
    let mut current = log.parent_session_id().map(str::to_owned);
    let mut seen = std::collections::BTreeSet::new();
    while let Some(parent) = current {
        if depth >= DEFAULT_MAX_DEPTH || !seen.insert(parent.clone()) {
            break;
        }
        depth += 1;
        current = load_session_log(sessions_dir, &parent)
            .ok()
            .and_then(|parent_log| parent_log.parent_session_id().map(str::to_owned));
    }
    depth
}

/// Agent 的工具执行器：authorize 之后按来源分发给 PluginHost（native）或 Sidecar。
struct ChatExecutor<'a> {
    plugins: &'a mut PluginHost,
    host: Option<&'a mut SidecarHost>,
    approvals: &'a Arc<ApprovalBroker>,
    /// 父会话日志（与流式回调共享同一个 RefCell：`spawn_child` 要向它追加
    /// child.spawned/finished，回调同时在追加 tool.call/tool.result）。
    log: std::rc::Rc<std::cell::RefCell<SessionLog>>,
    /// 会话目录（子会话文件落在这里）。
    sessions_dir: &'a std::path::Path,
    /// 父会话 id。
    parent_id: &'a str,
    /// 父会话取消句柄：子轮次共享，用户停止会同时停掉子会话。
    cancel: &'a Arc<AtomicBool>,
    /// 是否允许派生子会话（子会话本身不再派发，默认单层）。
    allow_spawn: bool,
    /// L2 Host Service 的会话读写面（None = 该轮不接 sidecar 服务）。
    l2_access: Option<Arc<ChatSessionAccess>>,
}

impl ToolExecutor for ChatExecutor<'_> {
    fn specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .plugins
            .registry()
            .list()
            .iter()
            .map(|definition| ToolSpec {
                name: definition.name.clone(),
                description: definition.description.clone(),
                parameters: definition.input_schema.clone(),
            })
            .collect();
        // `spawn_child` 是核心能力，不经过插件注册表；子会话不暴露该工具。
        if self.allow_spawn {
            specs.push(spawn_child_spec());
        }
        specs
    }

    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        if self.allow_spawn && call.tool == SPAWN_CHILD_TOOL {
            return self.execute_spawn_child(call);
        }
        match self.authorize_call(call) {
            Ok(definition) => self.dispatch_one(call, &definition),
            Err(outcome) => outcome,
        }
    }

    /// 同一 step 的批量执行（M3 并行工具，DSH `maxParallelToolCalls=10`）：
    /// - sidecar 工具走 [`SidecarHost::invoke_batch`]（≤ `max_parallel` 并发）；
    /// - native/MCP/`spawn_child` 逐个顺序执行（native 的插件调用是同步的，不做并发）；
    /// - 合并后按**原顺序**返回；单个失败/超时/拒绝在各自槽位降级，不拖垮整批。
    fn execute_batch(
        &mut self,
        calls: &[ToolCallRequest],
        max_parallel: usize,
    ) -> Vec<ToolOutcome> {
        let mut outcomes: Vec<Option<ToolOutcome>> = (0..calls.len()).map(|_| None).collect();
        let mut sidecar_indices: Vec<usize> = Vec::new();
        let mut sidecar_calls: Vec<dshr_compat_dsh_host::BatchInvokeCall> = Vec::new();
        let mut native_indices: Vec<usize> = Vec::new();
        let mut native_calls: Vec<(String, serde_json::Value)> = Vec::new();

        for (index, call) in calls.iter().enumerate() {
            // 子会话派发会阻塞跑一整轮，必须保持串行（且不改事件顺序）。
            if self.allow_spawn && call.tool == SPAWN_CHILD_TOOL {
                outcomes[index] = Some(self.execute_spawn_child(call));
                continue;
            }
            let definition = match self.authorize_call(call) {
                Ok(definition) => definition,
                Err(outcome) => {
                    outcomes[index] = Some(outcome);
                    continue;
                }
            };
            match definition.origin {
                ToolOrigin::Sidecar { .. } => {
                    if self.host.is_some() {
                        sidecar_indices.push(index);
                        sidecar_calls.push(dshr_compat_dsh_host::BatchInvokeCall::new(
                            call.call_id.clone(),
                            call.tool.clone(),
                            call.arguments.clone(),
                            sidecar_tool_timeout(&definition),
                        ));
                    } else {
                        outcomes[index] = Some(ToolOutcome::failure("sidecar unavailable"));
                    }
                }
                ToolOrigin::Native { .. } => {
                    // M4 W4：native 调用先收集；批末尾按插件并行（不同插件才会重叠）。
                    native_indices.push(index);
                    native_calls.push((call.tool.clone(), call.arguments.clone()));
                }
                ToolOrigin::Mcp { .. } => {
                    outcomes[index] = Some(ToolOutcome::failure("mcp not wired"));
                }
            }
        }

        if !sidecar_calls.is_empty() {
            // 服务读面快照一次；批结束后统一把 L2 追加事件写回内存日志。
            self.publish_l2_snapshot();
            let results = match self.host.as_mut() {
                Some(host) => host.invoke_batch(
                    &sidecar_calls,
                    max_parallel.max(1),
                    Some(self.cancel.as_ref()),
                ),
                None => (0..sidecar_calls.len())
                    .map(|_| {
                        Err(dshr_compat_dsh_host::HostError::Protocol(
                            "sidecar disappeared before batch".to_owned(),
                        ))
                    })
                    .collect(),
            };
            self.drain_l2_appends();
            for (slot, index) in sidecar_indices.iter().enumerate() {
                let outcome = match results.get(slot) {
                    Some(Ok(outcome)) if outcome.terminal.kind == "tool.result" => {
                        outcome_from_terminal(&outcome.terminal)
                    }
                    Some(Ok(outcome)) => ToolOutcome::failure(outcome.terminal.payload.to_string()),
                    Some(Err(error)) => ToolOutcome::failure(error.to_string()),
                    None => ToolOutcome::failure("batch outcome missing"),
                };
                outcomes[*index] = Some(outcome);
            }
        }

        if !native_calls.is_empty() {
            let results = self.plugins.call_approved_batch_cancellable(
                &native_calls,
                max_parallel.max(1),
                Some(self.cancel.as_ref()),
            );
            for (slot, index) in native_indices.iter().enumerate() {
                let outcome = match results.get(slot) {
                    Some(Ok(value)) => ToolOutcome::success(
                        json!([{ "type": "json", "value": value.clone() }]),
                        value.clone(),
                    ),
                    Some(Err(error)) => ToolOutcome::failure(error.clone()),
                    None => ToolOutcome::failure("native batch outcome missing"),
                };
                outcomes[*index] = Some(outcome);
            }
        }

        outcomes
            .into_iter()
            .map(|outcome| {
                outcome
                    .unwrap_or_else(|| ToolOutcome::failure("batch outcome missing for this call"))
            })
            .collect()
    }
}

impl ChatExecutor<'_> {
    /// `spawn_child`：阻塞跑一整轮子会话（必须串行）。
    fn execute_spawn_child(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        let mut log = self.log.borrow_mut();
        TreeCoordinator::spawn_child(
            &mut log,
            self.parent_id,
            self.sessions_dir,
            call,
            self.approvals,
            self.cancel,
        )
    }

    /// 策略 + schema 授权（Ask 时阻塞等审批）；失败返回可直接使用的降级结果。
    ///
    /// `ToolOutcome` 携带 content/value/meta（与协议 payload 同量级），这里按错误值直接
    /// 复用而不是再包一层 enum，与 chat 现有错误路径一致。
    #[allow(clippy::result_large_err)]
    fn authorize_call(&mut self, call: &ToolCallRequest) -> Result<ToolDefinition, ToolOutcome> {
        match self
            .plugins
            .registry()
            .authorize(&call.tool, &call.arguments)
        {
            Ok(definition) => Ok(definition.clone()),
            Err(RegistryError::NeedsApproval { tool }) => {
                // 策略说"要确认"：阻塞等工作线程外的 UI 决定（或超时按拒绝）。
                // 停止生成必须能打断审批等待：request_cancellable 每 100ms 检查一次取消标志。
                let outcome = self.approvals.request_cancellable(
                    &call.call_id,
                    &tool,
                    &call.arguments,
                    Duration::from_secs(300),
                    self.cancel.as_ref(),
                );
                match outcome {
                    ApprovalOutcome::AllowOnce => self
                        .plugins
                        .registry()
                        .get(&tool)
                        .cloned()
                        .ok_or_else(|| ToolOutcome::failure(format!("工具 {tool} 已卸载"))),
                    ApprovalOutcome::Deny => Err(ToolOutcome::failure(format!(
                        "用户拒绝了工具 `{tool}` 的调用"
                    ))),
                }
            }
            Err(err) => Err(ToolOutcome::failure(err.to_string())),
        }
    }

    /// 单调用分发（sidecar 走单调用路径，保持既有语义）。
    fn dispatch_one(&mut self, call: &ToolCallRequest, definition: &ToolDefinition) -> ToolOutcome {
        match definition.origin {
            ToolOrigin::Sidecar { .. } => {
                // 发布当前会话快照：L2 服务（working-directory/session-log）读面
                // 看到的是本轮最新日志；工具返回后再把服务追加的事件补写回来。
                self.publish_l2_snapshot();
                let Some(host) = self.host.as_mut() else {
                    return ToolOutcome::failure("sidecar unavailable");
                };
                // 超时用工具声明的 `timeout_ms`（对齐 DSH 的 per-tool timeoutMs），
                // 而不是写死 5s：带 bash/pwsh 的工具固定 5s 必超时。
                // 可取消版本在用户停止时发 tool.cancel 给 sidecar（慢工具不再等待完整超时）。
                let timeout = sidecar_tool_timeout(definition);
                let result = host.invoke_as_cancellable(
                    &call.call_id,
                    &call.tool,
                    call.arguments.clone(),
                    timeout,
                    Some(self.cancel.as_ref()),
                );
                self.drain_l2_appends();
                match result {
                    Ok(outcome) if outcome.terminal.kind == "tool.result" => {
                        outcome_from_terminal(&outcome.terminal)
                    }
                    Ok(outcome) => ToolOutcome::failure(outcome.terminal.payload.to_string()),
                    Err(err) => ToolOutcome::failure(err.to_string()),
                }
            }
            _ => self.dispatch_non_sidecar(call, definition),
        }
    }

    /// native / MCP 分发（同步；批量里也保持串行）。
    fn dispatch_non_sidecar(
        &mut self,
        call: &ToolCallRequest,
        definition: &ToolDefinition,
    ) -> ToolOutcome {
        match definition.origin {
            ToolOrigin::Native { .. } => {
                match self.plugins.call_approved(&call.tool, &call.arguments) {
                    Ok(value) => {
                        ToolOutcome::success(json!([{ "type": "json", "value": value }]), value)
                    }
                    Err(err) => ToolOutcome::failure(err.to_string()),
                }
            }
            ToolOrigin::Mcp { .. } => ToolOutcome::failure("mcp not wired"),
            ToolOrigin::Sidecar { .. } => ToolOutcome::failure("sidecar must use invoke_batch"),
        }
    }

    fn publish_l2_snapshot(&self) {
        if let Some(access) = &self.l2_access {
            access.publish(&self.log.borrow());
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
        parent_id: &str,
        sessions_dir: &std::path::Path,
        call: &ToolCallRequest,
        approvals: &Arc<ApprovalBroker>,
        cancel: &Arc<AtomicBool>,
    ) -> ToolOutcome {
        let args = match SpawnChildArgs::from_call(call) {
            Ok(args) => args,
            Err(err) => return ToolOutcome::failure(format!("spawn_child 参数非法: {err}")),
        };
        let store = SessionIndex::new(sessions_dir);
        let child_id = store.new_id();
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
        let child_path = store.path_for(&child_id);
        let mut child = ChatState::open(
            child_path,
            &child_id,
            cancel,
            approvals.clone(),
            false,
            false,
        );
        let reply = child.stream_turn(&args.prompt, &mut |_| {});
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
        Self::open(path, session_id, cancel, approvals, true, true)
    }

    /// 通用构造：
    /// - `allow_spawn = false`：子会话不再派发（默认单层，深度上限 2）；
    /// - `with_sidecar = false`：子会话只用原生工具，避免每次派发再起一个 Node 进程。
    fn open(
        path: PathBuf,
        session_id: &str,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
        allow_spawn: bool,
        with_sidecar: bool,
    ) -> Self {
        let mut boot_errors: Vec<String> = Vec::new();
        // 上次保存若在 Windows remove→rename 窗口崩溃，主文件会缺失而 .bak 完整：
        // 先尝试恢复，避免把可恢复的历史当成“新会话”重写。
        if !path.exists() {
            match recover_backup(&path) {
                Ok(true) => {
                    eprintln!("[chat] 已从备份恢复会话日志 {}", path.display());
                    boot_errors.push(format!(
                        "已恢复：会话从备份恢复（{}），可继续使用",
                        path.display()
                    ));
                }
                Ok(false) => {}
                Err(err) => boot_errors.push(format!("会话备份恢复失败：{err}")),
            }
        }
        let mut save_blocked: Option<String> = None;
        let mut log = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(text) => match SessionLog::from_jsonl(session_id, &text) {
                    Ok(log) => log,
                    // 损坏：隔离原文件（字节保留供导出排查），绝不覆盖。
                    Err(err) => {
                        match quarantine_corrupt(&path) {
                            Ok(quarantined) => {
                                let message = format!(
                                    "会话日志损坏，已隔离为 {}（原文件保留，可导出排查）: {err}",
                                    quarantined.display()
                                );
                                eprintln!("[chat] {message}");
                                boot_errors.push(message);
                            }
                            Err(quarantine_err) => {
                                let message = format!(
                                    "会话日志损坏且隔离失败（为避免覆盖，已停止写入）: {err}; {quarantine_err}"
                                );
                                eprintln!("[chat] {message}");
                                boot_errors.push(message);
                                save_blocked = Some(String::from("会话日志损坏且无法隔离"));
                            }
                        }
                        SessionLog::new(session_id)
                    }
                },
                Err(err) => {
                    // 文件存在但读失败（锁定/权限）：绝不能当空会话重写。
                    let message = format!("读取会话日志失败（为避免覆盖，已停止写入）: {err}");
                    eprintln!("[chat] {message}");
                    boot_errors.push(message);
                    save_blocked = Some(format!("读取会话日志失败: {err}"));
                    SessionLog::new(session_id)
                }
            }
        } else {
            SessionLog::new(session_id)
        };
        // 新会话：把 `session.created` 作为第一条事件（父链/来源/工作区的权威来源）。
        let mut wrote_created = false;
        if log.is_empty() {
            let workspace = workspace_root().display().to_string();
            match log.append_created(now_ts(), &SessionCreated::root(workspace)) {
                Ok(_) => wrote_created = true,
                Err(err) => eprintln!("[chat] 写入 session.created 失败: {err}"),
            }
        }

        let sessions_dir = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        // L2 会话读写面在 spawn 之前准备好：Router 注册时需要它（chat.rs 只负责构造/注入）。
        let l2_access = if with_sidecar {
            Some(ChatSessionAccess::new(sessions_dir.clone()))
        } else {
            None
        };
        let host = if with_sidecar {
            spawn_sidecar(Some(approvals.clone()), l2_access.clone())
        } else {
            None
        };
        // 审批策略：`DSHR_TOOL_POLICY=ask-all` 保持最高优先级（自检/演示要更严）；
        // 否则按 `DSHR_PERMISSION_MODE`（read-only / workspace-write / danger-full-access），
        // 缺省 workspace-write = 原有 desktop 行为，不回归。
        let policy = if std::env::var("DSHR_TOOL_POLICY").as_deref() == Ok("ask-all") {
            ApprovalPolicy::ask_all()
        } else {
            let mode = PermissionMode::from_env();
            eprintln!(
                "[chat] approval mode={}（DSHR_PERMISSION_MODE）",
                mode.as_str()
            );
            ApprovalPolicy::from_env()
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
        let mut plugin_errors = host
            .as_ref()
            .map(|sidecar| sidecar.boot_errors().to_vec())
            .unwrap_or_default();
        plugin_errors.extend(boot_errors);

        let mut state = Self {
            log,
            path: path.clone(),
            host,
            plugins,
            agent: Agent::new(select_provider(cancel)),
            seq: 0,
            approvals,
            spill: SpillStore::new(spill_dir_for(&path)),
            session_id: session_id.to_owned(),
            sessions_dir,
            l2_access,
            cancel: cancel.clone(),
            allow_spawn,
            plugin_errors,
            save_blocked,
            adc_panels: Vec::new(),
        };
        // 原生面板在构造期就取一次树；external-process 面板由启动流程装载后再刷新。
        state.refresh_adc_panels();
        // 新建的会话立刻落盘：索引/侧栏/树查询都从磁盘扫描，不能等第一轮才可见。
        if wrote_created {
            state.save();
        }
        state
    }

    /// 刷新 `workspace.panel` 的 ADC 视图缓存（装载/重载插件后调用一次）。
    ///
    /// 渲染路径（`plugins_panel`）只读缓存：插件 IPC 不进入每帧渲染，避免 UI 线程
    /// 等外部进程（`ui.render` 超时默认 10s）。未知 view / 不实现 view 的插件只留下
    /// 可读错误，不影响其它面板。
    fn refresh_adc_panels(&mut self) {
        let registrations: Vec<(String, String, String)> = self
            .plugins
            .panels()
            .iter()
            .filter(|registration| registration.panel.slot == "workspace.panel")
            .map(|registration| {
                (
                    registration.plugin_id.clone(),
                    registration.panel.title.clone(),
                    registration.panel.view_name().to_owned(),
                )
            })
            .collect();
        let mut panels = Vec::with_capacity(registrations.len());
        for (plugin_id, title, view) in registrations {
            let mut panel = AdcPanelView {
                plugin_id: plugin_id.clone(),
                title,
                view: view.clone(),
                tree: None,
                error: None,
            };
            match self.plugins.render_panel(&plugin_id, &view) {
                Ok(tree) => panel.tree = Some(tree),
                Err(error) => panel.error = Some(format!("view `{view}`: {error}")),
            }
            panels.push(panel);
        }
        self.adc_panels = panels;
    }

    /// 组装上下文计划：先按阈值把超大工具结果外部化（写对象 + 事件留痕），再重新计划。
    /// 外部化后模型只看到"引用 + 预览"，原始内容可从内容寻址对象无损取回。
    fn plan_with_spill(&mut self) -> ContextPlan {
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
        let Some(host) = spawn_sidecar(Some(self.approvals.clone()), self.l2_access.clone()) else {
            return false;
        };
        register_sidecar_tools(&mut self.plugins, &host);
        self.plugin_errors = host.boot_errors().to_vec();
        self.host = Some(host);
        true
    }

    fn save(&self) {
        if let Some(reason) = &self.save_blocked {
            eprintln!("[chat] 跳过保存（{reason}）: {}", self.path.display());
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // 临时文件 + 替换 + 上一版 .bak：崩溃/断电不会留下半截文件；
        // Windows 上 remove→rename 的极短窗口由 recover_backup 兜底。
        if let Err(err) = save_atomic(&self.path, self.log.to_jsonl().as_bytes()) {
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
        // 上下文预算在这里做：超大结果先外部化、超长工具结果头尾裁剪、超预算丢最旧，且不拆配对。
        let mut plan = self.plan_with_spill();
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
            plan = self.plan_with_spill();
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
                "spilled_tool_results": plan.spilled_tool_results(),
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
        // 非失败结束原因（用户取消）：决定 TurnEnded 后写 session.interrupted 而不是“处理失败”。
        let mut cancelled: Option<String> = None;
        // 父日志临时放进 Rc<RefCell>：流式回调（tool.* 事件）与 spawn_child 执行器
        // （child.spawned/finished）都要写同一份父日志，单借用无法同时满足。
        let log_cell = std::rc::Rc::new(std::cell::RefCell::new(std::mem::replace(
            &mut self.log,
            SessionLog::new(&self.session_id),
        )));
        {
            let mut executor = ChatExecutor {
                plugins: &mut self.plugins,
                host: self.host.as_mut(),
                approvals: &self.approvals,
                log: log_cell.clone(),
                sessions_dir: &self.sessions_dir,
                parent_id: &self.session_id,
                cancel: &self.cancel,
                allow_spawn: self.allow_spawn,
                l2_access: self.l2_access.clone(),
            };
            let log_for_events = log_cell.clone();
            self.agent.run_turn_streaming(
                &history,
                text,
                &mut executor,
                &mut |event| match event {
                    AgentEvent::TextDelta(delta) => {
                        reply.push_str(delta);
                        emit(rows_snapshot(&log_for_events.borrow(), Some(&reply)));
                    }
                    AgentEvent::ToolCall(call) => {
                        let (origin, presentation) = presentations
                            .get(&call.tool)
                            .cloned()
                            .unwrap_or_else(|| ("unknown".to_owned(), "generic".to_owned()));
                        {
                            let mut log = log_for_events.borrow_mut();
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
                        }
                        emit(rows_snapshot(&log_for_events.borrow(), Some(&reply)));
                    }
                    AgentEvent::ToolResult { call_id, outcome } => {
                        {
                            let mut log = log_for_events.borrow_mut();
                            // P1-3：普通工具失败（拒绝/报错/预算耗尽）必须写 tool.result(is_error)，
                            // 只有“真正被取消”的调用才写 tool.aborted（reason 用 spec 枚举）。
                            if outcome.is_error && self.cancel.load(Ordering::Relaxed) {
                                let _ = log.append(EventSpec::durable(
                                    "tool.aborted",
                                    &ts,
                                    json!({
                                        "call_id": call_id,
                                        "reason": "cancel"
                                    }),
                                ));
                            } else {
                                let _ = log.append(EventSpec::durable(
                                    "tool.result",
                                    &ts,
                                    json!({
                                        "call_id": call_id,
                                        "is_error": outcome.is_error,
                                        // 对齐 DSH：content 给模型，value/meta 给展示与回放。
                                        "content": outcome.content,
                                        "value": outcome.value,
                                        "meta": outcome.meta,
                                    }),
                                ));
                            }
                        }
                        emit(rows_snapshot(&log_for_events.borrow(), Some(&reply)));
                    }
                    AgentEvent::ToolCancelled { call_id, reason } => {
                        {
                            let mut log = log_for_events.borrow_mut();
                            let _ = log.append(EventSpec::durable(
                                "tool.aborted",
                                &ts,
                                json!({ "call_id": call_id, "reason": reason }),
                            ));
                        }
                        emit(rows_snapshot(&log_for_events.borrow(), Some(&reply)));
                    }
                    AgentEvent::TurnCancelled { reason } => {
                        cancelled = Some(reason.clone());
                        emit(rows_snapshot(&log_for_events.borrow(), Some(&reply)));
                    }
                    AgentEvent::TurnFailed(err) => {
                        reply.push_str(&format!("处理失败：{err}"));
                        emit(rows_snapshot(&log_for_events.borrow(), Some(&reply)));
                    }
                    AgentEvent::TurnStarted { .. }
                    | AgentEvent::StepStarted { .. }
                    | AgentEvent::TurnEnded { .. } => {}
                },
            );
        }
        // 重复工具软提醒：本轮结束后一次性落 durable 事件（模型侧注入由 agent-core 完成）。
        {
            let repeat_ts = now_ts();
            let mut log = log_cell.borrow_mut();
            let written = append_repeat_reminders(&mut self.agent, &mut log, &repeat_ts);
            if written > 0 {
                eprintln!("[chat] tool.repeat: {written} 条（重复工具调用软提醒已落日志）");
            }
        }
        // 还原父日志：spawn_child 可能已往里追加 child.spawned / child.finished。
        self.log = std::rc::Rc::try_unwrap(log_cell)
            .map(|cell| cell.into_inner())
            .unwrap_or_else(|cell| cell.borrow().clone());
        // 用户中断留痕（回放/审计可见；子会话的中断由 TreeCoordinator 传播）。
        // 取消可能由 Agent 的 TurnCancelled 上报，也可能发生在工具返回后，两个来源都覆盖。
        let cancelled_by_flag = self.cancel.load(Ordering::Relaxed);
        if cancelled.is_some() || cancelled_by_flag {
            let reason = cancelled
                .clone()
                .unwrap_or_else(|| String::from("user_stop"));
            let _ = self.log.append(EventSpec::durable(
                "session.interrupted",
                now_ts(),
                json!({ "reason": reason, "by_parent": false }),
            ));
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
        // 取消且没有任何产出时不写空的 assistant 消息（避免 UI 出现空白气泡）。
        let cancelled_turn = cancelled_by_flag || cancelled.is_some();
        if !(cancelled_turn && reply.trim().is_empty()) {
            let _ = self.log.append(EventSpec::durable(
                "message.assistant.settled",
                &ts,
                json!({ "message_id": format!("m-a{}", self.seq), "text": reply }),
            ));
        }
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
        let log_cell = std::rc::Rc::new(std::cell::RefCell::new(std::mem::replace(
            &mut self.log,
            SessionLog::new(&self.session_id),
        )));
        let mut executor = ChatExecutor {
            plugins: &mut self.plugins,
            host: self.host.as_mut(),
            approvals: &self.approvals,
            log: log_cell.clone(),
            sessions_dir: &self.sessions_dir,
            parent_id: &self.session_id,
            cancel: &self.cancel,
            allow_spawn: self.allow_spawn,
            l2_access: self.l2_access.clone(),
        };
        let outcome = executor.execute(call);
        drop(executor);
        self.log = std::rc::Rc::try_unwrap(log_cell)
            .map(|cell| cell.into_inner())
            .unwrap_or_else(|cell| cell.borrow().clone());
        outcome
    }
}

/// 滚动帧探针：用 GPUI profiler 的 `FrameTiming` 记录真实帧耗时与呈现间隔。
///
/// 由 `DSHR_FRAME_PROBE=<帧数>` 开启：每帧推进滚动位置、请求下一帧，达到目标帧数后
/// 把统计写到 `DSHR_FRAME_LOG`（默认 `.tmp/frame-probe.txt`）并按 `DSHR_FRAME_QUIT`
/// （默认 1）退出。这是 Gate 2 的测量手段：时间戳来自渲染管线，不是伪造的 sleep。
struct FrameProbe {
    target: usize,
    dir: f32,
    scroll_pos: f32,
    step: f32,
    collector: FrameTimingCollector,
    draw_work_ns: Vec<u64>,
    layout_ns: Vec<u64>,
    prepaint_ns: Vec<u64>,
    paint_ns: Vec<u64>,
    present_interval_ns: Vec<u64>,
    refresh_ns: Option<u64>,
    viewport: Option<(f32, f32)>,
    scale: f32,
    scroll_max: f32,
    rows: usize,
    started: std::time::Instant,
    log_path: PathBuf,
    quit: bool,
    ticks: usize,
    debug: bool,
}

impl FrameProbe {
    fn from_env() -> Option<Self> {
        let target = std::env::var("DSHR_FRAME_PROBE")
            .ok()?
            .parse::<usize>()
            .ok()?;
        if target == 0 {
            return None;
        }
        let step = std::env::var("DSHR_FRAME_STEP")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(0.01);
        let log_path = std::env::var("DSHR_FRAME_LOG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(".tmp/frame-probe.txt"));
        let quit = std::env::var("DSHR_FRAME_QUIT")
            .map(|value| value != "0")
            .unwrap_or(true);
        Some(Self {
            target,
            dir: 1.0,
            scroll_pos: 0.0,
            step,
            collector: FrameTimingCollector::new(),
            draw_work_ns: Vec::new(),
            layout_ns: Vec::new(),
            prepaint_ns: Vec::new(),
            paint_ns: Vec::new(),
            present_interval_ns: Vec::new(),
            refresh_ns: None,
            viewport: None,
            scale: 1.0,
            scroll_max: 0.0,
            rows: 0,
            started: std::time::Instant::now(),
            log_path,
            quit,
            ticks: 0,
            debug: std::env::var("DSHR_FRAME_DEBUG").is_ok(),
        })
    }

    /// 收集上一帧的 profiler 事件并推进滚动；返回 true 表示达到目标帧数。
    fn tick(
        &mut self,
        scroll: &ScrollHandle,
        viewport: gpui::Size<gpui::Pixels>,
        scale: f32,
        rows: usize,
    ) -> bool {
        for event in self.collector.collect_unseen() {
            match event {
                FrameEvent::Draw(timing) => {
                    self.draw_work_ns.push(
                        timing
                            .draw_end
                            .saturating_duration_since(timing.draw_start)
                            .as_nanos() as u64,
                    );
                    self.layout_ns
                        .push(timing.phases.request_layout.as_nanos() as u64);
                    self.prepaint_ns
                        .push(timing.phases.prepaint.as_nanos() as u64);
                    self.paint_ns.push(timing.phases.paint.as_nanos() as u64);
                    if self.refresh_ns.is_none() {
                        self.refresh_ns = timing.refresh_interval.map(|d| d.as_nanos() as u64);
                    }
                }
                FrameEvent::Present(present) => {
                    if let Some(interval) = present.animation_interval {
                        self.present_interval_ns.push(interval.as_nanos() as u64);
                    }
                }
            }
        }
        // 滚动范围按当前窗口（逻辑像素）计算；看不到内容时仍以 1px 兜底，避免除零。
        let max = scroll.max_offset().y.as_f32().max(1.0);
        self.scroll_pos = (self.scroll_pos + self.dir * self.step).clamp(0.0, 1.0);
        if self.scroll_pos >= 1.0 {
            self.dir = -1.0;
        }
        if self.scroll_pos <= 0.0 {
            self.dir = 1.0;
        }
        scroll.set_offset(point(px(0.0), px(-self.scroll_pos * max)));
        self.viewport = Some((viewport.width.as_f32(), viewport.height.as_f32()));
        self.scale = scale;
        self.scroll_max = max;
        self.rows = rows;
        self.ticks += 1;
        if self.debug && self.ticks.is_multiple_of(60) {
            eprintln!(
                "[probe] ticks={} draws={} trace={}",
                self.ticks,
                self.draw_work_ns.len(),
                gpui::profiler::trace_enabled()
            );
        }
        // 安全阀：120s 内一帧都没完成说明内容量让首帧无法收敛（例如一次性展开上万行），
        // 记录空报告并退出，避免探针把窗口挂死。
        if self.draw_work_ns.is_empty() && self.started.elapsed() > Duration::from_secs(120) {
            return true;
        }
        self.draw_work_ns.len() >= self.target
    }

    fn report(&self) -> String {
        let draws = self.draw_work_ns.len();
        let elapsed = self.started.elapsed();
        let fps = if elapsed.as_secs_f64() > 0.0 {
            draws as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };
        let mut out = String::new();
        out.push_str(&format!(
            "[frame-probe] draws={} elapsed_ms={} avg_fps={:.1} rows={} viewport={:?} scale={} scroll_max={:.0}\n",
            draws,
            elapsed.as_millis(),
            fps,
            self.rows,
            self.viewport,
            self.scale,
            self.scroll_max
        ));
        for (name, values) in [
            ("draw_work", &self.draw_work_ns),
            ("request_layout", &self.layout_ns),
            ("prepaint", &self.prepaint_ns),
            ("paint", &self.paint_ns),
            ("present_interval", &self.present_interval_ns),
        ] {
            if values.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "[frame-probe] {} p50_us={:.2} p95_us={:.2} max_us={:.2} samples={}\n",
                name,
                percentile_ns(values, 0.50) as f64 / 1_000.0,
                percentile_ns(values, 0.95) as f64 / 1_000.0,
                values.iter().copied().max().unwrap_or(0) as f64 / 1_000.0,
                values.len()
            ));
        }
        let budget = self.refresh_ns.unwrap_or(16_666_667);
        let missed = self
            .draw_work_ns
            .iter()
            .filter(|&&work| work > budget)
            .count();
        out.push_str(&format!(
            "[frame-probe] refresh_ns={:?} budget_us={:.2} missed_refresh={} missed_ratio={:.4}\n",
            self.refresh_ns,
            budget as f64 / 1_000.0,
            missed,
            if draws == 0 {
                0.0
            } else {
                missed as f64 / draws as f64
            }
        ));
        out
    }
}

fn percentile_ns(values: &[u64], pct: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 - 1.0) * pct).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// ADC Input/Textarea 的编辑会话（M4 W3）：用真实 `TextInput`，提交时把值注入 onChange。
struct AdcEditor {
    plugin_id: String,
    view: String,
    path: String,
    on_change: AdcAction,
    input: Entity<TextInput>,
    multiline: bool,
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
    /// 等待用户确认的 `openExternal` URL（M4 W3；确认前不打开）。
    pending_external: Option<String>,
    /// ADC Input/Textarea 的编辑模态（None = 未编辑）。
    adc_editor: Option<AdcEditor>,
    /// 长会话只渲染最近 N 条（点"载入更早"每次 +200），避免每帧构造上万元素。
    visible_limit: usize,
    /// 帧探针（DSHR_FRAME_PROBE）：滚动时测量真实帧耗时与呈现间隔。
    frame_probe: Option<FrameProbe>,
    /// 会话树深度（session id → depth）；侧栏按树顺序展示并缩进子会话。
    session_depths: std::collections::BTreeMap<String, u32>,
    /// sidecar 启动/插件装载/插件面板错误：插件面板顶部提示（避免"插件失败但界面无感"）。
    plugin_errors: Vec<String>,
    /// 插件 ADC 面板快照（渲染路径只读；刷新见加载插件/打开面板处，不持锁渲染）。
    adc_panels: Vec<AdcPanelView>,
}

impl ChatApp {
    /// 请求停止生成：provider 在下一个 SSE 行边界返回已生成的部分。
    fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            return;
        }
        self.cancel.store(true, Ordering::Relaxed);
        // 审批等待中的调用立即结束（否则 UI 的审批条会一直挂到 300s 超时）。
        self.approvals.cancel_pending();
        self.pending_approvals = 0;
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
        // 上一轮的停止信号不能污染这一轮（provider 与 ChatState 共享同一个 Arc）。
        self.cancel.store(false, Ordering::Relaxed);
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

    /// 打开/关闭右侧插件面板；打开时补一次 ADC 面板快照。
    fn toggle_plugins_panel(&mut self, cx: &mut Context<Self>) {
        self.plugins_open = !self.plugins_open;
        if self.plugins_open {
            // 与点击入口一致：用 try_lock，工作线程持锁时不阻塞 UI 渲染。
            if let Ok(guard) = self.state.try_lock() {
                self.adc_panels = guard.adc_panels.clone();
            }
        }
        cx.notify();
    }

    /// ADC Input/Textarea 点击：打开真实 TextInput 模态；提交时把值注入 onChange。
    fn open_adc_editor(
        &mut self,
        plugin_id: String,
        view: String,
        args: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(on_change) = args
            .get("on_change")
            .and_then(|value| parse_action(value).ok())
        else {
            eprintln!("[adc] 编辑请求缺少可解析的 on_change");
            return;
        };
        let path = args["path"].as_str().unwrap_or_default().to_owned();
        let value = args["value"].as_str().unwrap_or_default().to_owned();
        let multiline = args["multiline"].as_bool().unwrap_or(false);
        let placeholder = if multiline {
            "编辑多行值"
        } else {
            "编辑值"
        };
        let input = cx.new(|cx| TextInput::new(placeholder, cx));
        input.update(cx, |input, cx| input.set_content(value, window, cx));
        let handle = input.read(cx).focus_handle(cx);
        self.adc_editor = Some(AdcEditor {
            plugin_id,
            view,
            path,
            on_change,
            input,
            multiline,
        });
        window.focus(&handle, cx);
        cx.notify();
    }

    fn commit_adc_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.adc_editor.take() else {
            return;
        };
        let value = editor.input.read(cx).content().to_owned();
        match editor.on_change {
            AdcAction::Call { method, mut args } => {
                if let Some(object) = args.as_object_mut() {
                    object.insert("value".to_owned(), json!(value));
                }
                self.dispatch_adc_action(
                    &editor.plugin_id,
                    &editor.view,
                    "call",
                    json!({ "method": method, "args": args }),
                    cx,
                );
            }
            AdcAction::Command { id } => {
                self.dispatch_adc_action(
                    &editor.plugin_id,
                    &editor.view,
                    "command",
                    json!({ "id": id }),
                    cx,
                );
            }
            other => {
                eprintln!(
                    "[adc] 编辑提交只支持 call/command（path={}）: {other:?}",
                    editor.path
                );
            }
        }
        cx.notify();
    }

    fn cancel_adc_edit(&mut self, cx: &mut Context<Self>) {
        self.adc_editor = None;
        cx.notify();
    }

    fn adc_editor_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let editor = self.adc_editor.as_ref()?;
        let p = t.palette;
        let input = editor.input.clone();
        let save = cx.listener(|this, _, _window, cx| this.commit_adc_edit(cx));
        let cancel = cx.listener(|this, _, _window, cx| this.cancel_adc_edit(cx));
        let label = if editor.multiline {
            format!("编辑多行值 · {}", editor.path)
        } else {
            format!("编辑值 · {}", editor.path)
        };
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
                        .items_center()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(t.radii.md)
                        .border_1()
                        .border_color(p.color(p.state_warn))
                        .bg(p.color(p.bg_layer_1))
                        .shadow_sm()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(p.label_caption))
                                        .child(label),
                                )
                                .child(div().w_full().child(input)),
                        )
                        .child(
                            div()
                                .id("adc-edit-cancel")
                                .role(Role::Button)
                                .aria_label("取消编辑")
                                .px(px(10.0))
                                .py(px(5.0))
                                .rounded(t.radii.sm)
                                .border_1()
                                .border_color(p.color(p.border_l2))
                                .bg(p.color(p.button_elevated))
                                .cursor_pointer()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_primary))
                                .child("取消")
                                .on_click(cancel),
                        )
                        .child(
                            div()
                                .id("adc-edit-save")
                                .role(Role::Button)
                                .aria_label("保存编辑")
                                .px(px(10.0))
                                .py(px(5.0))
                                .rounded(t.radii.sm)
                                .bg(p.color(p.button_info))
                                .cursor_pointer()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.bg_base))
                                .child("保存")
                                .on_click(save),
                        ),
                )
                .into_any_element(),
        )
    }

    /// ADC `openExternal` 必须先确认（M4 W3）；确认前只保存 URL，不打开。
    fn request_open_external(&mut self, url: String, cx: &mut Context<Self>) {
        self.pending_external = Some(url);
        cx.notify();
    }

    fn external_confirm_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let url = self.pending_external.as_ref()?;
        let p = t.palette;
        let mut display: String = url.chars().take(160).collect();
        if url.chars().count() > 160 {
            display.push('…');
        }
        let cancel = cx.listener(|this, _, _window, cx| {
            this.pending_external = None;
            cx.notify();
        });
        let open = cx.listener(|this, _, _window, cx| {
            if let Some(url) = this.pending_external.take() {
                cx.open_url(&url);
            }
            cx.notify();
        });
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
                        .items_center()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(t.radii.md)
                        .border_1()
                        .border_color(p.color(p.state_warn))
                        .bg(p.color(p.bg_layer_1))
                        .shadow_sm()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(t.type_scale.secondary)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.color(p.label_primary))
                                        .child("打开外部链接？"),
                                )
                                .child(
                                    div()
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(p.label_caption))
                                        .text_overflow(TextOverflow::Truncate("…".into()))
                                        .child(display),
                                ),
                        )
                        .child(
                            div()
                                .id("external-deny")
                                .role(Role::Button)
                                .aria_label("取消打开外部链接")
                                .px(px(10.0))
                                .py(px(5.0))
                                .rounded(t.radii.sm)
                                .border_1()
                                .border_color(p.color(p.border_l2))
                                .bg(p.color(p.button_elevated))
                                .cursor_pointer()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_primary))
                                .child("取消")
                                .on_click(cancel),
                        )
                        .child(
                            div()
                                .id("external-open")
                                .role(Role::Button)
                                .aria_label("确认打开外部链接")
                                .px(px(10.0))
                                .py(px(5.0))
                                .rounded(t.radii.sm)
                                .bg(p.color(p.button_info))
                                .cursor_pointer()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.bg_base))
                                .child("打开")
                                .on_click(open),
                        ),
                )
                .into_any_element(),
        )
    }

    /// 键盘审批：处理当前第一个待确认调用；没有 pending 时返回 false。
    fn resolve_first_approval(&mut self, outcome: ApprovalOutcome, cx: &mut Context<Self>) -> bool {
        let Some(first) = self.approvals.pending().first().cloned() else {
            return false;
        };
        self.approvals.resolve(&first.call_id, outcome);
        self.pending_approvals = self.approvals.pending().len();
        cx.notify();
        true
    }

    // ------------------------------------------------------------------
    // 键盘动作（M4 W2）：处理函数只做状态变更，键位映射见 `chat_key_bindings()`。
    // ------------------------------------------------------------------

    fn shortcut_new_session(
        &mut self,
        _: &NewSessionShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("new-session");
        self.new_session(window, cx);
    }

    fn shortcut_focus_composer(
        &mut self,
        _: &FocusComposerShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("focus-composer");
        self.focus_composer(window, cx);
    }

    fn shortcut_toggle_sidebar(
        &mut self,
        _: &ToggleSidebarShortcut,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("toggle-sidebar");
        self.toggle_sidebar(cx);
    }

    fn shortcut_toggle_plugins(
        &mut self,
        _: &TogglePluginsShortcut,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("toggle-plugins");
        self.toggle_plugins_panel(cx);
    }

    fn shortcut_approve_first(
        &mut self,
        _: &ApproveFirstShortcut,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("approve-first");
        self.resolve_first_approval(ApprovalOutcome::AllowOnce, cx);
    }

    fn shortcut_deny_first(
        &mut self,
        _: &DenyFirstShortcut,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("deny-first");
        self.resolve_first_approval(ApprovalOutcome::Deny, cx);
    }

    fn shortcut_escape(&mut self, _: &EscapeShortcut, window: &mut Window, cx: &mut Context<Self>) {
        trace_shortcut("escape");
        if self.adc_editor.is_some() {
            self.cancel_adc_edit(cx);
        } else if self.pending_external.take().is_some() {
            cx.notify();
        } else if self.plugins_open {
            self.plugins_open = false;
            cx.notify();
        } else if self.renaming.is_some() {
            self.cancel_rename(cx);
        } else if self.pending_delete.take().is_some() {
            cx.notify();
        } else {
            self.focus_composer(window, cx);
        }
    }

    fn shortcut_quit(&mut self, _: &QuitShortcut, _window: &mut Window, cx: &mut Context<Self>) {
        trace_shortcut("quit");
        cx.quit();
    }

    fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    // ------------------------------------------------------------------
    // 多会话：索引/切换/新建/重命名/删除
    // ------------------------------------------------------------------

    fn reload_sessions(&mut self) {
        let (sessions, depths) = ordered_sessions(&self.store);
        self.sessions = sessions;
        self.session_depths = depths;
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
        self.plugin_errors = state.plugin_errors.clone();
        self.state = Arc::new(Mutex::new(state));
        self.cancel = cancel;
        self.rows = rows;
        self.session_id = id;
        self.busy = false;
        self.initial_scroll_done = false;
        self.visible_limit = usize::MAX;
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

    /// 把插件面板的 `call`/`command` 动作派发给插件，然后刷新面板树（M4 W3）。
    ///
    /// 动作是 fire-and-forget；插件更新自身状态后，`refresh_adc_panels` 再发
    /// `ui.render` 取新树。失败只进日志，不阻塞 UI 其它部分。
    fn dispatch_adc_action(
        &mut self,
        plugin_id: &str,
        view: &str,
        action: &str,
        payload: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let result = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let result = guard
                .plugins
                .dispatch_action(plugin_id, view, action, &payload);
            if result.is_ok() {
                guard.refresh_adc_panels();
                self.adc_panels = guard.adc_panels.clone();
            }
            result
        };
        if let Err(error) = result {
            eprintln!("[adc] action `{action}` on {plugin_id}/{view} 失败: {error}");
        }
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
            self.plugin_errors = guard.plugin_errors.clone();
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
        // 插件错误提示：sidecar 在装载阶段上报的 log.emit(error) 直接显示在面板顶部。
        if !self.plugin_errors.is_empty() {
            let message = self
                .plugin_errors
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            list = list.child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(t.radii.sm)
                    .border_1()
                    .border_color(p.color(p.state_error))
                    .bg(p.color(p.bg_layer_1))
                    .child(
                        div()
                            .text_size(t.type_scale.small)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.color(p.state_error))
                            .child("启动/插件错误"),
                    )
                    .child(
                        div()
                            .text_size(t.type_scale.small)
                            .line_height(t.type_scale.small_line)
                            .text_color(p.color(p.label_tertiary))
                            .child(message),
                    ),
            );
        }
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

        // ADC-v1 插件面板：对 `workspace.panel` 的每条注册渲染它的 view。
        // 树由装载/打开面板时取好并快照在 `ChatApp` 里（**渲染路径不碰 state 锁**，
        // 否则工作线程跑一轮时会冻住 UI）；事件轮留 M4（ADR-0011 降级清单）。
        let adc_panels = self.adc_panels.clone();
        if !adc_panels.is_empty() {
            list = list.child(
                div()
                    .w_full()
                    .pt(px(6.0))
                    .text_size(t.type_scale.small)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.color(p.label_secondary))
                    .child("插件面板 · ADC-v1"),
            );
            for (index, panel) in adc_panels.iter().enumerate() {
                let mut card = div()
                    .id(("adc-panel", index))
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(t.radii.sm)
                    .border_1()
                    .border_color(p.color(p.border_l2))
                    .bg(p.color(p.bg_layer_1))
                    .child(
                        div()
                            .w_full()
                            .text_size(t.type_scale.small)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.color(p.label_secondary))
                            .text_overflow(TextOverflow::Truncate("…".into()))
                            .child(format!("{} · {}", panel.plugin_id, panel.title)),
                    );
                match &panel.tree {
                    Some(tree) => {
                        let weak = cx.entity().downgrade();
                        let plugin_id = panel.plugin_id.clone();
                        let view = panel.view.clone();
                        let handler: AdcActionHandler =
                            std::rc::Rc::new(move |action, window, app| match action {
                                AdcAction::Copy { text } => {
                                    app.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                    eprintln!("[adc] copy {} bytes", text.len());
                                }
                                AdcAction::Call { method, args } => {
                                    if method == "__adc_edit" {
                                        let plugin_id = plugin_id.clone();
                                        let view = view.clone();
                                        let args = args.clone();
                                        let _ = weak.update(app, |chat, cx| {
                                            chat.open_adc_editor(plugin_id, view, args, window, cx);
                                        });
                                    } else {
                                        let payload = json!({ "method": method, "args": args });
                                        let _ = weak.update(app, |chat, cx| {
                                            chat.dispatch_adc_action(
                                                &plugin_id, &view, "call", payload, cx,
                                            );
                                        });
                                    }
                                }
                                AdcAction::Command { id } => {
                                    let payload = json!({ "id": id });
                                    let _ = weak.update(app, |chat, cx| {
                                        chat.dispatch_adc_action(
                                            &plugin_id, &view, "command", payload, cx,
                                        );
                                    });
                                }
                                AdcAction::OpenExternal { url } => {
                                    let url = url.clone();
                                    let _ = weak.update(app, |chat, cx| {
                                        chat.request_open_external(url, cx);
                                    });
                                }
                                _ => {
                                    eprintln!("[adc] 未知动作（ADC-v1 新增类型），已忽略");
                                }
                            });
                        let (body, diagnostics) = adc_panel_body_with_handler(t, tree, handler);
                        card = card.child(body);
                        if !diagnostics.is_empty() {
                            // 未知元素 Placeholder / 配额裁剪仍然可见，诊断只作提示。
                            let errors = diagnostics
                                .iter()
                                .filter(|diagnostic| diagnostic.is_error())
                                .count();
                            card = card.child(
                                div()
                                    .text_size(t.type_scale.small)
                                    .line_height(t.type_scale.small_line)
                                    .text_color(p.color(p.state_warn))
                                    .child(format!(
                                        "ADC 诊断 {} 条（error {errors}）",
                                        diagnostics.len()
                                    )),
                            );
                        }
                    }
                    None => {
                        let message = panel
                            .error
                            .clone()
                            .unwrap_or_else(|| "（没有可渲染的 view）".to_owned());
                        card = card.child(
                            div()
                                .text_size(t.type_scale.small)
                                .line_height(t.type_scale.small_line)
                                .text_color(p.color(p.state_error))
                                .child(format!("面板渲染失败：{message}")),
                        );
                    }
                }
                list = list.child(card);
            }
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
                                .role(Role::Button)
                                .aria_label("关闭插件面板")
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
            // 子会话按树深度缩进，父会话保持 0 级。
            let depth = self.session_depths.get(&session.id).copied().unwrap_or(0);

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
                .pl(px(10.0 + depth as f32 * 14.0))
                .pr(px(10.0))
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
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .when(depth > 0, |title| {
                                    title.child(
                                        div()
                                            .text_size(t.type_scale.small)
                                            .text_color(p.color(p.label_caption))
                                            .child("↳"),
                                    )
                                })
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
                                ),
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
                    .role(Role::Button)
                    .aria_label("新建对话")
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
                            .role(Role::Button)
                            .aria_label(format!("切换到{}主题", self.theme_kind.toggled().label()))
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
                            .role(Role::Button)
                            .aria_label(if self.plugins_open {
                                "关闭插件面板"
                            } else {
                                "打开插件面板"
                            })
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
                                // 打开时补一次面板快照；用 try_lock：工作线程持有 state 时
                                // 保持旧快照，绝不阻塞 UI 渲染路径。
                                if this.plugins_open {
                                    if let Ok(guard) = this.state.try_lock() {
                                        this.adc_panels = guard.adc_panels.clone();
                                    }
                                }
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
                            .role(Role::Button)
                            .aria_label(if collapsed {
                                "展开侧栏"
                            } else {
                                "收起侧栏"
                            })
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
                            .role(Role::Button)
                            .aria_label(format!("切换到{}主题", self.theme_kind.toggled().label()))
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
                card,
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
                    // 有专用视图的工具结果：diff / terminal 用原生组件渲染正文，
                    // 不再只有一行摘要（正文已按卡片上限截断）。
                    .when_some(
                        card.as_ref().filter(|card| {
                            matches!(card.kind, CardKind::Diff | CardKind::Terminal)
                                && !card.body.is_empty()
                        }),
                        |el, card| {
                            let view = match card.kind {
                                CardKind::Diff => diff_view_from_text(t, &card.body),
                                _ => terminal_view_from_text(t, &card.body),
                            };
                            el.child(view)
                        },
                    )
                    // M4 W3：工具结果 `meta.ui` 的受限 ADC 附加区（tool card extra）。
                    .when_some(
                        card.as_ref().and_then(|card| card.extra.as_ref()),
                        |el, extra| {
                            let (body, _diagnostics) = adc_panel_body(t, extra);
                            el.child(body)
                        },
                    )
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
        // 不 clone 整份 rows：10k 会话下每帧克隆 7500 个 String 是纯浪费。
        let rows = &self.rows;
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
        // 变高行虚拟化：只构造/排版视口附近的几十行，用上下占位块维持滚动高度。
        // 实测 400 行全量渲染时 prepaint 要 125ms（debug），滚动只有 ~6fps；虚拟化后
        // 帧成本与历史长度基本无关，这是 Gate 2 "滚动帧率不低于基线" 的实现基础。
        let visible = &rows[start..];
        let mut cumulative = Vec::with_capacity(visible.len() + 1);
        cumulative.push(0.0f32);
        for (index, row) in visible.iter().enumerate() {
            let gap = if index == 0 { 0.0 } else { ROW_GAP };
            let next = cumulative[index] + estimated_row_height(row) + gap;
            cumulative.push(next);
        }
        let total_height = cumulative[visible.len()];
        let viewport_height = self.scroll.bounds().size.height.as_f32().max(240.0);
        let offset = (-self.scroll.offset().y.as_f32()).clamp(0.0, total_height);
        const OVERSCAN: f32 = 240.0;
        let top_edge = (offset - OVERSCAN).max(0.0);
        let bottom_edge = (offset + viewport_height + OVERSCAN).min(total_height);
        let first = cumulative
            .partition_point(|edge| *edge < top_edge)
            .saturating_sub(1)
            .min(visible.len().saturating_sub(1));
        let mut last = cumulative.partition_point(|edge| *edge < bottom_edge);
        if last <= first {
            last = (first + 1).min(visible.len());
        }
        let top_spacer = (cumulative[first] - ROW_GAP).max(0.0);
        let bottom_spacer = (total_height - cumulative[last] - ROW_GAP).max(0.0);
        if top_spacer > 0.5 {
            column = column.child(div().w_full().h(px(top_spacer)));
        }
        for row in &visible[first..last] {
            column = column.child(self.render_row(row, t, &base_font, base_color));
        }
        if bottom_spacer > 0.5 {
            column = column.child(div().w_full().h(px(bottom_spacer)));
        }
        let _ = cx;
        div()
            .id("transcript-scroll")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(px(24.0))
            // 滚动只改变窗口位置：重绘一次（只排版可见行）而不是重新构造整段历史。
            .on_scroll(
                cx.listener(|_this, _event: &gpui::ScrollEvent, _window, cx| {
                    cx.notify();
                }),
            )
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
                                        .role(Role::Button)
                                        .aria_label("拒绝这次工具调用")
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
                                        .role(Role::Button)
                                        .aria_label("允许一次工具调用")
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
                                    .role(Role::Button)
                                    .aria_label(if self.busy {
                                        "停止生成"
                                    } else {
                                        "发送消息"
                                    })
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
                    .child("回车发送 · 生成中可停止 · 输入法组合输入可用")
                    .child(chat_shortcut_hint()),
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
        /// 结构化卡片（Diff/Terminal 的原生视图从这里取正文）。
        card: Option<ToolCard>,
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

/// 虚拟化列表的行间距（必须与 transcript 列容器的 `gap` 一致）。
const ROW_GAP: f32 = 16.0;

/// 估计一行的高度（逻辑像素）。用于虚拟化的滚动占位；估算偏差只影响滚动条比例，
/// 不影响可见行之间的真实排版。按 CJK 宽度保守估计每行字符数。
fn estimated_row_height(row: &Row) -> f32 {
    const LINE: f32 = 23.0;
    match row {
        Row::User(text) => {
            let chars = text.chars().count() as f32;
            (chars / 28.0).ceil().max(1.0) * LINE + 20.0
        }
        Row::Assistant(text) => {
            let chars = text.chars().count() as f32;
            (chars / 36.0).ceil().max(1.0) * LINE + 12.0
        }
        Row::Tool { detail, .. } => {
            let chars = detail.chars().count() as f32;
            46.0 + (chars / 60.0).ceil() * 18.0
        }
        Row::Meta(_) => 30.0,
    }
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
                card: tool.card.clone(),
            }
        }
        Item::Compaction { from_seq, to_seq } => {
            Row::Meta(format!("上下文已压缩 · seq {from_seq}–{to_seq}"))
        }
    }
}

impl ChatApp {
    /// 启动/加载错误横幅：GUI 双击没有控制台，eprintln 用户看不到，必须画在界面上。
    /// 覆盖 sidecar 装载错误、会话日志损坏/隔离、备份恢复失败等。
    fn error_banner(&self, theme: &Theme) -> AnyElement {
        let t = theme;
        let p = theme.palette;
        let message = self
            .plugin_errors
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let recovered_only = self
            .plugin_errors
            .iter()
            .all(|message| message.starts_with("已恢复："));
        let tone = if recovered_only {
            p.color(p.state_success)
        } else {
            p.color(p.state_error)
        };
        let title = if recovered_only {
            "恢复提示"
        } else {
            "启动/加载错误"
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(12.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(tone)
            .bg(p.color(p.bg_layer_1))
            .child(
                div()
                    .text_size(t.type_scale.small)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tone)
                    .child(title),
            )
            .child(
                div()
                    .text_size(t.type_scale.small)
                    .line_height(t.type_scale.small_line)
                    .text_color(p.color(p.label_tertiary))
                    .child(message),
            )
            .into_any_element()
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
        // 帧探针：每帧推进滚动位置并请求下一帧；达到目标帧数后写报告（可选退出）。
        // 时间戳全部来自 GPUI 渲染管线，测量的是真实滚动帧率而不是估算。
        let mut probe_report = None;
        if let Some(probe) = self.frame_probe.as_mut() {
            self.initial_scroll_done = true;
            let finished = probe.tick(
                &self.scroll,
                window.viewport_size(),
                window.scale_factor(),
                self.visible_limit.min(self.rows.len()),
            );
            if finished {
                probe_report = Some(probe.report());
            } else {
                let entity = cx.entity();
                window.on_next_frame(move |_, cx| {
                    entity.update(cx, |_this, cx| cx.notify());
                });
            }
        }
        if let Some(report) = probe_report {
            let quit = self
                .frame_probe
                .as_ref()
                .map(|probe| probe.quit)
                .unwrap_or(false);
            let log_path = self
                .frame_probe
                .as_ref()
                .map(|probe| probe.log_path.clone())
                .unwrap_or_else(|| PathBuf::from(".tmp/frame-probe.txt"));
            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&log_path, &report);
            print!("{report}");
            self.frame_probe = None;
            if quit {
                cx.quit();
            }
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
                // 启动/加载错误在 GUI 主界面可见（双击启动没有控制台可看 stderr）。
                .when(!self.plugin_errors.is_empty(), |el| {
                    el.child(self.error_banner(&theme))
                })
                .child(self.transcript(&theme, window, cx))
                .when_some(self.approval_bar(&theme, cx), |el, bar| el.child(bar))
                .when_some(self.external_confirm_bar(&theme, cx), |el, bar| {
                    el.child(bar)
                })
                .when_some(self.adc_editor_bar(&theme, cx), |el, bar| el.child(bar))
                .child(self.composer(&theme, cx)),
        );
        shell
            .on_action(cx.listener(Self::shortcut_new_session))
            .on_action(cx.listener(Self::shortcut_focus_composer))
            .on_action(cx.listener(Self::shortcut_toggle_sidebar))
            .on_action(cx.listener(Self::shortcut_toggle_plugins))
            .on_action(cx.listener(Self::shortcut_approve_first))
            .on_action(cx.listener(Self::shortcut_deny_first))
            .on_action(cx.listener(Self::shortcut_escape))
            .on_action(cx.listener(Self::shortcut_quit))
            .when_some(self.plugins_panel(&theme, cx), |el, panel| el.child(panel))
    }
}

fn selftest() -> i32 {
    // 不用系统 TEMP：本机 TEMP 对部分子进程不可写（见 docs/m0-runbook.md）。
    let path = PathBuf::from(".tmp/chat-selftest.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
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
    reset_session_file(&path);
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
    // 资源根按 exe 相对解析（打包后不依赖源码树）。
    let root = app_root();
    if !root.join("sidecar/dsh-official/node_modules").exists() {
        println!("[selftest-dsh] SKIP: 先运行 `node tools/dsh-vendor.mjs` 准备官方插件依赖");
        return 0;
    }

    let path = PathBuf::from(".tmp/chat-dsh.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    // 本自检只验证"只读官方插件 + 回放"；写工具由 --selftest-dsh-write 覆盖。
    std::env::set_var("DSHR_FS_WRITE", "0");
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
    reset_session_file(&path);
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
    reset_session_file(&path);
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
    reset_session_file(&path);
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

/// 长会话压缩自检（Gate 3）：连续多轮注入超预算历史，验证
/// (1) 每次超预算都累积一条 `context.compacted`（可叠加多次压缩）；
/// (2) 每轮压缩后 `context.measured.ratio ≤ 0.8`、不再丢消息、配对合法；
/// (3) 重新读 JSONL 的 UI 行与模型历史与在线日志逐行一致（回放一致）。
fn selftest_long_context() -> i32 {
    let path = PathBuf::from(".tmp/chat-long-context.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    std::env::set_var("DSHR_CONTEXT_WINDOW", "8000");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    let mut failures = 0usize;
    let mut compactions = 0usize;
    // 每轮写入的历史量是窗口的 2 倍以上，保证"每轮都触发一次压缩"。
    for round in 0..8 {
        for index in 0..20 {
            let user = format!("长会话第 {round}-{index} 轮：{}", "长内容".repeat(60));
            let _ = state.log.append(EventSpec::durable(
                "message.user",
                "t0",
                json!({ "message_id": format!("lc-u{round}-{index}"), "text": user }),
            ));
            let call_id = format!("lc-c{round}-{index}");
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
                json!({ "message_id": format!("lc-a{round}-{index}"), "text": "收到" }),
            ));
        }
        let reply = state.stream_turn("继续", &mut |_| {});
        if reply.is_none() {
            failures += 1;
        }

        let latest_compacted = state
            .log
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == "context.compacted")
            .cloned();
        let Some(compacted) = latest_compacted else {
            println!("[long-context] round={round} FAIL: 没有 context.compacted");
            return 1;
        };
        compactions = state
            .log
            .events()
            .iter()
            .filter(|event| event.event_type == "context.compacted")
            .count();

        let measured_ratio = state
            .log
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == "context.measured")
            .and_then(|event| event.payload.get("ratio"))
            .and_then(|value| value.as_f64())
            .unwrap_or(1.0);
        let measured_window = state
            .log
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == "context.measured")
            .and_then(|event| event.payload.get("window"))
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let plan = plan_context(&state.log, "");
        let legal = !matches!(plan.messages.first(), Some(ChatMessage::Tool { .. }));

        // 回放一致性：JSONL → 事件 → 行/历史，必须与在线状态逐项一致。
        let rebuilt =
            SessionLog::from_jsonl(SESSION_ID, &state.log.to_jsonl()).expect("replay log");
        let live_rows = rows_snapshot(&state.log, None);
        let replay_rows = rows_snapshot(&rebuilt, None);
        let rows_equal = live_rows.len() == replay_rows.len()
            && live_rows
                .iter()
                .zip(replay_rows.iter())
                .all(|(live, replay)| live.main_text() == replay.main_text());
        let live_history = history_from_log(&state.log);
        let replay_history = history_from_log(&rebuilt);
        let history_equal = live_history.len() == replay_history.len()
            && live_history
                .iter()
                .zip(replay_history.iter())
                .all(|(live, replay)| format!("{live:?}") == format!("{replay:?}"));
        let has_summary = live_history
            .iter()
            .any(|message| matches!(message, ChatMessage::System(text) if text.contains("摘要")));
        let compaction_rows = live_rows
            .iter()
            .filter(|row| row.main_text().contains("上下文已压缩"))
            .count();
        let ok = measured_ratio <= 0.8
            && measured_window > 0
            && plan.tokens_after <= plan.window
            && plan.dropped_messages == 0
            && legal
            && rows_equal
            && history_equal
            && has_summary
            // UI 只显示"最近一次压缩"标记（更早的压缩事件被后续压缩区间覆盖，属预期），
            // 一致性由 rows_equal/history_equal 保证。
            && compaction_rows >= 1
            && compacted.payload.get("tokens_after").is_some();
        if !ok {
            failures += 1;
        }
        println!(
            "[long-context] round={round} compactions={compactions} ratio={measured_ratio:.3} \
             tokens={}/{} dropped={} rows={} history={} rows_equal={rows_equal} \
             history_equal={history_equal} summary={has_summary} ok={ok}",
            plan.tokens_after,
            plan.window,
            plan.dropped_messages,
            live_rows.len(),
            live_history.len(),
        );
    }

    let ok = failures == 0 && compactions >= 5;
    if ok {
        println!("[selftest-long-context] PASS compactions={compactions}");
        0
    } else {
        println!("[selftest-long-context] FAIL failures={failures} compactions={compactions}");
        1
    }
}

/// 上下文外部化自检（PLAN §6.4）：超大工具结果 → 内容寻址对象 + 两条事件留痕；
/// 模型历史只剩引用 + 预览；对象可无损取回；重复计划不重复外部化；回放一致。
fn selftest_spill() -> i32 {
    let path = PathBuf::from(".tmp/chat-spill.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let spill_dir = PathBuf::from(".tmp/chat-spill-objects");
    let _ = std::fs::remove_dir_all(&spill_dir);
    std::env::set_var("DSHR_SPILL_DIR", &spill_dir);
    // 窗口放大，避免压缩/丢弃与外部化互相干扰；阈值用默认 16 KiB。
    std::env::set_var("DSHR_CONTEXT_WINDOW", "200000");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    let big = format!("{}结尾唯一标记XYZ", "外部化内容".repeat(6_000));
    let _ = state.log.append(EventSpec::durable(
        "tool.call",
        "t0",
        json!({ "call_id": "sp-c1", "tool": "probe_echo", "arguments": {} }),
    ));
    let _ = state.log.append(EventSpec::durable(
        "tool.result",
        "t0",
        json!({
            "call_id": "sp-c1", "is_error": false,
            "content": [{ "type": "text", "text": big }]
        }),
    ));
    state.save();

    let plan = state.plan_with_spill();
    let created = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "spill.created")
        .count();
    let pruned = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "tool.result.pruned")
        .count();
    let refs = dshr_agent_context::spill::spill_refs_in_log(&state.log);
    let object = refs
        .first()
        .and_then(|spill| state.spill.get_verified(spill).ok());
    let lossless = object
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).contains("外部化内容"))
        .unwrap_or(false);
    // 模型看到的是 plan.messages（由 plan_context_with_spill 生成）；history_from_log
    // 不含外部化替换，不能用来断言"模型看到什么"。
    let history_text = format!("{:?}", plan.messages);
    let model_sees_reference = history_text.contains("工具结果已外部化");
    // 预览允许出现开头 512 字符；结尾标记只能由"完整内容泄漏"带进模型历史。
    let model_body_hidden = !history_text.contains("结尾唯一标记XYZ");

    // 重复计划：事件不重复，且第二次不再产生待外部化项。
    let plan_again = state.plan_with_spill();
    let created_again = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "spill.created")
        .count();

    // 回放一致：从 JSONL 重建后外部化已生效、模型历史条数一致。
    let rebuilt =
        SessionLog::from_jsonl(SESSION_ID, &state.log.to_jsonl()).expect("replay spill log");
    let policy = SpillPolicy::from_env();
    let replay_plan = plan_context_with_spill(&rebuilt, "", &policy);
    let live_plan = plan_context_with_spill(&state.log, "", &policy);
    let replay_consistent = replay_plan.spilled.is_empty()
        && replay_plan.messages.len() == live_plan.messages.len()
        && replay_plan.spilled_tool_results() == 0;

    let ok = created == 1
        && pruned == 1
        && refs.len() == 1
        && lossless
        && model_sees_reference
        && model_body_hidden
        && plan.spilled.is_empty()
        && plan_again.spilled.is_empty()
        && created_again == created
        && replay_consistent
        && plan.tokens_after < plan.tokens_before;
    println!(
        "[spill] created={created} pruned={pruned} objects={} lossless={lossless} \
         reference={model_sees_reference} hidden={model_body_hidden} \
         tokens={}->{} replay={replay_consistent} history_chars={}",
        refs.len(),
        plan.tokens_before,
        plan.tokens_after,
        history_text.chars().count(),
    );
    std::env::remove_var("DSHR_SPILL_DIR");
    std::env::remove_var("DSHR_CONTEXT_WINDOW");
    if ok {
        println!("[selftest-spill] PASS");
        0
    } else {
        println!("[selftest-spill] FAIL");
        1
    }
}

/// Gate 4 证据（最关键分界线）：官方 `dsh-tool-fs` 的 write/edit/apply_patch 在
/// "Rust 审批 + 沙箱"约束下真正可用，官方插件源码一行未改。
///
/// 校验：能力协商后四个工具都注册 → write/edit/apply_patch 真实落盘 → 越界被拒 →
/// 审批 deny 时工具报错且文件不变 → 写前快照存在。
fn selftest_dsh_write() -> i32 {
    // 资源根按 exe 相对解析（打包后不依赖源码树）。
    let root = app_root();
    if !root.join("sidecar/dsh-official/node_modules").exists() {
        println!("[selftest-dsh-write] SKIP: 先运行 `node tools/dsh-vendor.mjs`");
        return 2;
    }
    let ws = std::path::absolute(".tmp/dsh-write-ws")
        .unwrap_or_else(|_| PathBuf::from(".tmp/dsh-write-ws"));
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::create_dir_all(&ws);
    std::env::set_var("DSHR_WORKSPACE", &ws);
    std::env::set_var("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    // 用全新 profile：旧 profile 文件可能还留着 write:false 的历史默认值。
    let profile_dir = PathBuf::from(".tmp/profile-dsh-write");
    let _ = std::fs::remove_dir_all(&profile_dir);
    let profile = Profile::defaults(&profile_dir);
    std::env::set_var("DSHR_DSH_PLUGINS", profile.sidecar_json());

    let path = PathBuf::from(".tmp/chat-dsh-write.jsonl");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::new(path, SESSION_ID, &cancel, ApprovalBroker::new());

    let registered: Vec<String> = state
        .plugins
        .registry()
        .list()
        .iter()
        .map(|definition| definition.name.clone())
        .collect();
    let has = |name: &str| registered.iter().any(|candidate| candidate == name);
    if !(has("write") && has("edit") && has("apply_patch") && has("read")) {
        println!("[dsh-write] registered={registered:?}");
        println!("[selftest-dsh-write] FAIL: 写工具未注册（host_caps/能力协商问题）");
        return 1;
    }

    let call = |tool: &str, arguments: serde_json::Value| ToolCallRequest {
        call_id: format!("dw-{tool}-{}", now_ts()),
        tool: tool.to_owned(),
        arguments,
    };
    let read_file = |name: &str| std::fs::read_to_string(ws.join(name)).unwrap_or_default();

    let write = state.execute_tool(&call(
        "write",
        json!({ "file_path": "hello.txt", "content": "hello dsh write" }),
    ));
    let wrote = !write.is_error && read_file("hello.txt") == "hello dsh write";

    let edit = state.execute_tool(&call(
        "edit",
        json!({ "file_path": "hello.txt", "old_string": "hello", "new_string": "hi" }),
    ));
    let edited = !edit.is_error && read_file("hello.txt") == "hi dsh write";

    let patch = state.execute_tool(&call(
        "apply_patch",
        json!({ "input": "*** Begin Patch\n*** Add File: notes.txt\n+patch line\n*** End Patch\n" }),
    ));
    let patched = !patch.is_error && read_file("notes.txt").contains("patch line");

    // 越界写：Rust 沙箱必须拒绝，且不落盘。
    let escaped = state.execute_tool(&call(
        "write",
        json!({ "file_path": "../escape.txt", "content": "nope" }),
    ));
    let escaped_rejected = escaped.is_error && !ws.join("../escape.txt").exists();

    // 审批拒绝：工具报错、文件内容不变。
    std::env::set_var("DSHR_APPROVAL_MODE", "deny");
    let denied = state.execute_tool(&call(
        "write",
        json!({ "file_path": "hello.txt", "content": "should not land" }),
    ));
    let denied_kept = denied.is_error && read_file("hello.txt") == "hi dsh write";

    let snapshots = std::fs::read_dir(ws.join(".dshr").join("snapshots"))
        .map(|dir| dir.count())
        .unwrap_or(0);
    let ok = wrote && edited && patched && escaped_rejected && denied_kept && snapshots > 0;
    println!(
        "[dsh-write] registered={registered:?} write={wrote} edit={edited} patch={patched} \
         escape_rejected={escaped_rejected} deny_kept={denied_kept} snapshots={snapshots}"
    );
    std::env::remove_var("DSHR_APPROVAL_MODE");
    if ok {
        println!("[selftest-dsh-write] PASS");
        0
    } else {
        println!("[selftest-dsh-write] FAIL");
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
    reset_session_file(&denied_path);
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
    // P1-3：审批 Deny 属于“工具执行失败”，必须写 tool.result(is_error=true)，
    // 而不是 tool.aborted（aborted 留给真正的取消/崩溃语义）。
    let denied_tool_result = denied_state.log.events().iter().any(|event| {
        event.event_type == "tool.result"
            && event
                .payload
                .get("is_error")
                .and_then(|value| value.as_bool())
                == Some(true)
    });
    let denied_tool_aborted = event_count(&denied_state, "tool.aborted") > 0;
    let asked = event_count(&denied_state, "approval.asked");
    let answered = event_count(&denied_state, "approval.answered");
    let deny_outcome = denied_state.log.events().iter().any(|event| {
        event.event_type == "approval.answered"
            && event.payload.get("outcome").and_then(|v| v.as_str()) == Some("deny")
    });

    let allowed_path = PathBuf::from(".tmp/chat-approval-allow.jsonl");
    reset_session_file(&allowed_path);
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let mut allowed_state =
        ChatState::new(allowed_path, SESSION_ID, &cancel, ApprovalBroker::new());
    let allowed_reply = allowed_state.stream_turn("用工具试一下", &mut |_| {});
    let allowed_tool = allowed_state.log.events().iter().any(|event| {
        event.event_type == "tool.result"
            && event
                .payload
                .get("is_error")
                .and_then(|value| value.as_bool())
                == Some(false)
    });
    let allow_outcome = allowed_state.log.events().iter().any(|event| {
        event.event_type == "approval.answered"
            && event.payload.get("outcome").and_then(|v| v.as_str()) == Some("allow_once")
    });

    std::env::remove_var("DSHR_APPROVAL_MODE");
    std::env::remove_var("DSHR_TOOL_POLICY");

    println!(
        "[approval] asked={asked} answered={answered} denied_tool_result={denied_tool_result} \
         denied_tool_aborted={denied_tool_aborted} deny_outcome={deny_outcome} \
         allowed_tool={allowed_tool} allow_outcome={allow_outcome}"
    );
    let ok = denied_reply.is_some()
        && allowed_reply.is_some()
        && denied_tool_result
        && !denied_tool_aborted
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

/// 损坏会话日志自检：加载失败必须**隔离原文件**并新建空会话，绝不能把原文件覆盖成
/// 1 行 `session.created`（review P1-1）。
fn selftest_corrupt_session() -> i32 {
    let root = PathBuf::from(".tmp/chat-corrupt-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);

    // 先造一份合法日志，再追加半截记录模拟中断写入。
    let valid = {
        let mut log = SessionLog::new("s-corrupt");
        let _ = log.append_created(now_ts(), &SessionCreated::root(String::from(".")));
        log.to_jsonl()
    };
    let corrupt_bytes = format!("{valid}{{\"session_id\":\"s-corrupt\",\"seq\":2");
    let path = root.join("s-corrupt.jsonl");
    std::fs::write(&path, &corrupt_bytes).expect("write corrupt fixture");

    let cancel = Arc::new(AtomicBool::new(false));
    let state = ChatState::open(
        path.clone(),
        "s-corrupt",
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );

    // 原文件被隔离，字节完整保留。
    let quarantined: Vec<PathBuf> = std::fs::read_dir(&root)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|candidate| {
                    candidate
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name.contains(".corrupt-"))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    let quarantined_bytes = quarantined
        .first()
        .and_then(|candidate| std::fs::read_to_string(candidate).ok())
        .unwrap_or_default();

    // 新会话文件存在且只含一条 session.created（未被损坏内容污染）。
    let fresh_text = std::fs::read_to_string(&path).unwrap_or_default();
    let fresh_ok = SessionLog::from_jsonl("s-corrupt", &fresh_text)
        .map(|log| log.events().len() == 1 && log.events()[0].event_type == "session.created")
        .unwrap_or(false);

    let reported = state
        .plugin_errors
        .iter()
        .any(|message| message.contains("会话日志损坏") && message.contains("隔离"));
    let preserved = quarantined_bytes == corrupt_bytes;

    println!(
        "[corrupt-session] quarantined={} preserved={preserved} fresh_ok={fresh_ok} reported={reported}",
        quarantined.len()
    );
    let ok = quarantined.len() == 1 && preserved && fresh_ok && reported;
    if ok {
        println!("[selftest-corrupt-session] PASS");
        0
    } else {
        println!("[selftest-corrupt-session] FAIL");
        1
    }
}

/// 故意在注册后失败的插件：宿主必须原子回滚，且仍能装载后续健康插件。
struct RecoveryFailingPlugin;

impl Plugin for RecoveryFailingPlugin {
    fn id(&self) -> &'static str {
        "recovery.failing"
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "recovery_fail_tool",
                "should be rolled back",
                ToolOrigin::Native {
                    plugin_id: "recovery.failing".to_owned(),
                },
            )
            .with_schema(json!({ "type": "object" })),
        )?;
        Err(PluginError::Invalid("injected apply failure".to_owned()))
    }

    fn call(
        &mut self,
        _tool: &str,
        _arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        unreachable!("failing plugin is rolled back")
    }
}

/// 失败插件之后装载的健康插件。
struct RecoveryHealthyPlugin;

impl Plugin for RecoveryHealthyPlugin {
    fn id(&self) -> &'static str {
        "recovery.healthy"
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "recovery_healthy_echo",
                "healthy plugin tool",
                ToolOrigin::Native {
                    plugin_id: "recovery.healthy".to_owned(),
                },
            )
            .with_schema(json!({ "type": "object" })),
        )?;
        Ok(())
    }

    fn call(
        &mut self,
        _tool: &str,
        _arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        Ok(json!({ "ok": true }))
    }
}

/// `--selftest-recovery`：G2 数据/插件失败恢复（无窗口）。
///
/// 1. 损坏会话：隔离原字节、生成干净 `session.created`、错误可见（复用
///    `--selftest-corrupt-session`）；
/// 2. 备份恢复：删掉主文件后 `.bak` 原子恢复，重放成功并可继续追加；
/// 3. 插件失败：`apply` 注册后失败被回滚，随后健康插件可装载/调用。
fn selftest_recovery() -> i32 {
    let corrupt_ok = selftest_corrupt_session() == 0;

    let root = PathBuf::from(".tmp/m4-recovery");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let path = root.join("s-recovery.jsonl");
    let mut log = SessionLog::new("s-recovery");
    let _ = log.append_created(now_ts(), &SessionCreated::root(String::from(".")));
    let first_len = log.events().len();
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());
    let _ = log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "before crash" }),
    ));
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());

    // 模拟 Windows 替换窗口：主文件缺失、`.bak` 是上一次成功版本。
    std::fs::remove_file(&path).expect("remove main log");
    let recovered = recover_backup(&path).unwrap_or(false);
    let replayed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-recovery", &text).ok());
    let backup_ok = recovered
        && replayed
            .as_ref()
            .map(|log| log.events().len() == first_len)
            .unwrap_or(false);
    let continue_ok = replayed.map_or(false, |mut log| {
        let _ = log.append(EventSpec::durable(
            "message.user",
            now_ts(),
            json!({ "text": "after recovery" }),
        ));
        save_atomic(&path, log.to_jsonl().as_bytes()).is_ok()
            && std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| SessionLog::from_jsonl("s-recovery", &text).ok())
                .map(|log| log.events().len() == first_len + 1)
                .unwrap_or(false)
    });

    let mut host = PluginHost::new(AllowAll);
    let rollback_ok =
        host.load(Box::new(RecoveryFailingPlugin)).is_err() && host.registry().is_empty();
    let healthy_ok = host.load(Box::new(RecoveryHealthyPlugin)).is_ok();
    let continue_plugin_ok = host.call("recovery_healthy_echo", &json!({})).is_ok();

    let ok =
        corrupt_ok && backup_ok && continue_ok && rollback_ok && healthy_ok && continue_plugin_ok;
    println!(
        "[selftest-recovery] corrupt={corrupt_ok} backup={backup_ok} continue={continue_ok} plugin_rollback={rollback_ok} healthy={healthy_ok} healthy_call={continue_plugin_ok}"
    );
    if ok {
        println!("[selftest-recovery] PASS");
        0
    } else {
        println!("[selftest-recovery] FAIL");
        1
    }
}

/// `--selftest-crash-child <path>`：模拟进程在保存后崩溃（主文件缺失、`.bak` 完整），
/// 由父进程 `--selftest-crash-recovery` 验证恢复与继续追加。
fn selftest_crash_child(path: &str) -> i32 {
    let path = PathBuf::from(path);
    let _ = std::fs::remove_file(&path);
    let mut log = SessionLog::new("s-crash");
    let _ = log.append_created(now_ts(), &SessionCreated::root(String::from(".")));
    let _ = log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "before crash" }),
    ));
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());
    // 第二次保存产生 `.bak`（上一版含 before crash），再删主文件模拟替换窗口崩溃。
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());
    let _ = std::fs::remove_file(&path);
    std::process::abort();
}

/// `--selftest-crash-recovery`：真实子进程崩溃 → `.bak` 恢复 → 继续追加。
fn selftest_crash_recovery() -> i32 {
    let root = PathBuf::from(".tmp/m4-crash-recovery");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let path = root.join("s-crash.jsonl");
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            println!("[selftest-crash-recovery] FAIL: current_exe: {error}");
            return 1;
        }
    };
    let status = std::process::Command::new(exe)
        .arg("--selftest-crash-child")
        .arg(&path)
        .status();
    let crashed = status.map(|status| !status.success()).unwrap_or(false);
    let backup_exists = path.with_extension("jsonl.bak").exists();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open(
        path.clone(),
        "s-crash",
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );
    let before_ok = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-crash", &text).ok())
        .map(|log| {
            log.events().iter().any(|event| {
                event.event_type == "message.user"
                    && event.payload["text"].as_str() == Some("before crash")
            })
        })
        .unwrap_or(false);
    let _ = state.log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "after crash" }),
    ));
    state.save();
    let after_ok = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-crash", &text).ok())
        .map(|log| {
            log.events().iter().any(|event| {
                event.event_type == "message.user"
                    && event.payload["text"].as_str() == Some("after crash")
            })
        })
        .unwrap_or(false);
    let ok = crashed && backup_exists && before_ok && after_ok;
    let notice_ok = state
        .plugin_errors
        .iter()
        .any(|message| message.starts_with("已恢复："));
    println!(
        "[selftest-crash-recovery] crashed={crashed} backup={backup_exists} before={before_ok} after={after_ok} notice={notice_ok}"
    );
    if ok && notice_ok {
        println!("[selftest-crash-recovery] PASS");
        0
    } else {
        println!("[selftest-crash-recovery] FAIL");
        1
    }
}

/// `--selftest-sidecar-recovery`：G2 的 sidecar 崩溃场景（headless）。
///
/// 先确认 sidecar 可调用，再 kill 子进程；断言会话仍可追加/保存、原生工具仍注册，
/// 且对已死 sidecar 的调用返回错误而不是挂起。
fn selftest_sidecar_recovery() -> i32 {
    let root = PathBuf::from(".tmp/m4-sidecar-recovery");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let path = root.join("s-sidecar.jsonl");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open(
        path.clone(),
        "s-sidecar",
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );

    let Some(mut host) = spawn_sidecar(None, None) else {
        println!("[selftest-sidecar-recovery] FAIL: sidecar 不可用（需要 node + sidecar/probe-*）");
        return 1;
    };
    let live = host
        .invoke(
            "probe_echo",
            json!({ "text": "live" }),
            Duration::from_secs(10),
        )
        .is_ok();
    let _ = state.log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "before kill" }),
    ));
    state.save();
    state.host = Some(host);

    let killed = state
        .host
        .as_mut()
        .map(|host| host.kill().is_ok())
        .unwrap_or(false);
    let dead_call = state
        .host
        .as_mut()
        .map(|host| {
            host.invoke(
                "probe_echo",
                json!({ "text": "after" }),
                Duration::from_secs(2),
            )
            .is_err()
        })
        .unwrap_or(false);
    let native_ok = state.plugins.registry().get("probe_native").is_some();
    let _ = state.log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "after kill" }),
    ));
    state.save();
    let replayed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-sidecar", &text).ok())
        .map(|log| log.events().len())
        .unwrap_or(0);
    let ok = live && killed && dead_call && native_ok && replayed >= 3;
    println!(
        "[selftest-sidecar-recovery] live={live} killed={killed} dead_call={dead_call} native={native_ok} replayed={replayed}"
    );
    if ok {
        println!("[selftest-sidecar-recovery] PASS");
        0
    } else {
        println!("[selftest-sidecar-recovery] FAIL");
        1
    }
}

/// 取消语义自检（P1-2）：turn 开始前置位 cancel → 必须走 TurnCancelled + session.interrupted，
/// 不出现“处理失败”，也不写空的 assistant 消息。
fn selftest_cancel() -> i32 {
    let _ = std::fs::create_dir_all(".tmp");
    std::env::set_var("DSHR_FORCE_MOCK", "1");
    let path = PathBuf::from(".tmp/chat-cancel.jsonl");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open(
        path,
        SESSION_ID,
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );
    cancel.store(true, Ordering::Relaxed);
    let reply = state.stream_turn("你好", &mut |_| {});
    let interrupted = state
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "session.interrupted");
    let failed = reply
        .as_deref()
        .map(|text| text.contains("处理失败"))
        .unwrap_or(false);
    let settled = state
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "message.assistant.settled");
    let tool_events = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type.starts_with("tool."))
        .count();
    println!(
        "[cancel] interrupted={interrupted} failed={failed} settled={settled} tool_events={tool_events}"
    );
    let ok = interrupted && !failed && !settled && tool_events == 0 && reply.as_deref() == Some("");
    if ok {
        println!("[selftest-cancel] PASS");
        0
    } else {
        println!("[selftest-cancel] FAIL");
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
    reset_session_file(&path);
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
/// 会话树前序顺序 + 深度（父在前、子紧随其后），侧栏与自检共用。
fn session_tree_order(store: &SessionIndex) -> Vec<(String, u32)> {
    match store.tree() {
        Ok(tree) => tree
            .flatten()
            .into_iter()
            .map(|(depth, node)| (node.id.clone(), depth))
            .collect(),
        Err(err) => {
            eprintln!("[chat] 会话树损坏（回退到扁平列表）: {err}");
            Vec::new()
        }
    }
}

/// 按会话树前序排好的会话列表 + 深度表（侧栏与启动初始化共用）。
fn ordered_sessions(
    store: &SessionIndex,
) -> (Vec<SessionMeta>, std::collections::BTreeMap<String, u32>) {
    let order = session_tree_order(store);
    let metas: std::collections::BTreeMap<String, SessionMeta> = store
        .scan()
        .unwrap_or_default()
        .into_iter()
        .map(|meta| (meta.id.clone(), meta))
        .collect();
    if order.is_empty() {
        return (
            metas.into_values().collect(),
            std::collections::BTreeMap::new(),
        );
    }
    let mut ordered = Vec::new();
    let mut depths = std::collections::BTreeMap::new();
    for (id, depth) in &order {
        depths.insert(id.clone(), *depth);
        if let Some(meta) = metas.get(id) {
            ordered.push(meta.clone());
        }
    }
    // 树里没有的（索引竞争等极端情况）按 id 补在末尾，保证不丢会话。
    for (id, meta) in &metas {
        if !depths.contains_key(id) {
            ordered.push(meta.clone());
        }
    }
    (ordered, depths)
}

fn sessions_dir_from_env() -> PathBuf {
    std::env::var("DSHR_SESSIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".tmp/sessions"))
}

/// `chat --export-session <id> [--export-dir <dir>]`：
/// 导出 JSONL + 外部化附件 + manifest（sha256），并立刻自校验。
fn export_session_cli(id: &str, out: Option<PathBuf>) -> i32 {
    let store = SessionIndex::new(sessions_dir_from_env());
    let path = store.path_for(id);
    if !path.exists() {
        eprintln!("[export] 会话不存在: {}", path.display());
        return 1;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("[export] 读取会话失败: {err}");
            return 1;
        }
    };
    let log = match SessionLog::from_jsonl(id, &text) {
        Ok(log) => log,
        Err(err) => {
            eprintln!("[export] 会话日志损坏: {err}");
            return 1;
        }
    };
    let spill = SpillStore::new(spill_dir_for(&path));
    let out_dir = out.unwrap_or_else(|| PathBuf::from(".tmp/exports").join(id));
    let manifest = match export_session(
        &log,
        &SpillStoreSource::new(&spill),
        &out_dir,
        &ExportOptions::new(now_ts()),
    ) {
        Ok(manifest) => manifest,
        Err(err) => {
            eprintln!("[export] 导出失败: {err}");
            return 1;
        }
    };
    println!(
        "[export] session={} events={} attachments={} missing={} bytes={} out={}",
        manifest.session_id,
        manifest.event_count,
        manifest.attachments.len(),
        manifest.missing_blobs.len(),
        manifest.total_bytes,
        out_dir.display()
    );
    match verify_export(&out_dir, id) {
        Ok(report) if report.is_ok() => {
            println!(
                "[export] verify=ok jsonl_ok={} attachments_checked={}",
                report.jsonl_ok, report.attachments_checked
            );
            0
        }
        Ok(report) => {
            eprintln!("[export] 自校验失败: {:?}", report.problems);
            1
        }
        Err(err) => {
            eprintln!("[export] 自校验失败: {err}");
            1
        }
    }
}

/// `chat --retention-plan`：只计算、不删除（每个会话一个 JSONL 段）。
fn retention_plan_cli() -> i32 {
    use dshr_session_export::{plan_retention, RetentionPolicy, SegmentUsage, SessionUsage};
    let store = SessionIndex::new(sessions_dir_from_env());
    let metas = match store.scan() {
        Ok(metas) => metas,
        Err(err) => {
            eprintln!("[retention] 扫描会话失败: {err}");
            return 1;
        }
    };
    let usages: Vec<SessionUsage> = metas
        .iter()
        .map(|meta| {
            let size = std::fs::metadata(&meta.path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            SessionUsage::new(
                meta.id.clone(),
                vec![SegmentUsage {
                    segment_id: String::from("main"),
                    relative_path: meta
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    size_bytes: size,
                    last_modified_secs: meta.updated_at,
                }],
                0,
            )
        })
        .collect();
    let policy = RetentionPolicy::new(2 * 1024 * 1024 * 1024, 90, 1);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let plan = plan_retention(&usages, &policy, now);
    println!(
        "[retention] sessions={} bytes_before={} bytes_after={} deletions={} over_cap={} reclaimed={}",
        usages.len(),
        plan.bytes_before,
        plan.bytes_after,
        plan.deletions.len(),
        plan.over_cap.len(),
        plan.bytes_reclaimed()
    );
    for deletion in &plan.deletions {
        println!(
            "[retention] delete session={} segment={} size={} reason={:?}",
            deletion.session_id, deletion.segment_id, deletion.size_bytes, deletion.reason
        );
    }
    // 只出计划，不调用 apply_retention：删除必须由用户显式触发。
    println!("[retention] plan-only（未删除任何文件）");
    0
}

struct InstallCliOptions {
    package: PathBuf,
    /// 非交互确认（CI / 脚本）；未验证签名的包必须显式给出。
    yes_install: bool,
    allow_downgrade: bool,
}

fn install_plugin_cli(package: PathBuf) -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let sidecar_dir = sidecar_dir_for_app();
    let profile_dir = profile_dir_for_app();
    install_plugin_with_options(
        &InstallCliOptions {
            package,
            yes_install: args.iter().any(|arg| arg == "--yes-install"),
            allow_downgrade: args.iter().any(|arg| arg == "--allow-downgrade"),
        },
        &sidecar_dir,
        &profile_dir,
    )
}

/// 可信安装 CLI（M3 G2）：摘要/签名校验 → 首装确认（`--yes-install`）→ 审计。
fn install_plugin_with_options(
    options: &InstallCliOptions,
    sidecar_dir: &std::path::Path,
    profile_dir: &std::path::Path,
) -> i32 {
    if let Err(err) = std::fs::create_dir_all(sidecar_dir) {
        eprintln!("[install] 创建 sidecar 目录失败: {err}");
        return 1;
    }
    let mut profile = match Profile::load(profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[install] 加载 profile 失败: {err}");
            return 1;
        }
    };
    let store = match TrustStore::load(profile_dir) {
        Ok(store) => store,
        Err(err) => {
            eprintln!("[install] 信任密钥库无效（{profile_dir:?}/trust-keys.json）: {err}");
            return 1;
        }
    };
    let policy = TrustPolicy {
        allow_downgrade: options.allow_downgrade,
        require_confirm_for_unsigned: true,
        store,
        now_ms: None,
    };
    let package = options.package.clone();
    let yes_install = options.yes_install;
    let result = install_package_trusted(sidecar_dir, &package, &mut profile, &policy, |preview| {
        // 非交互 CLI：必须显式 --yes-install；打印与 spec §5 一致的预览项。
        println!(
            "[install] 待安装: {} {}（{}）",
            preview.name,
            preview.version,
            preview.signature_status.as_str()
        );
        println!(
            "[install] 来源: {}；摘要: {}",
            package.display(),
            preview.digest_sha256
        );
        if let Some(key_id) = &preview.key_id {
            println!("[install] 签名 key: {key_id}");
        }
        if let Some(installed) = &preview.installed_version {
            println!("[install] 已安装版本: {installed}");
        }
        println!("[install] 会获得的权限: {}", preview.permissions_hint);
        println!("[install] 警告: 该插件可在你的用户权限下执行任意代码");
        if !yes_install {
            eprintln!("[install] 未验证签名的包需要显式确认：加 --yes-install 重跑（本次未安装）");
        }
        yes_install
    });
    match result {
        Ok(report) => {
            println!(
                "[install] {} -> {}（profile_added={}，signature={}）",
                report.install.name,
                report.install.destination.display(),
                report.install.added_to_profile,
                report.trust.signature_status.as_str()
            );
            println!(
                "[install] profile={}；重启 chat 后加载",
                profile.path().display()
            );
            println!(
                "[install] 审计记录: {}",
                profile_dir.join("install-audit.jsonl").display()
            );
            0
        }
        Err(err) => {
            if matches!(err, dshr_profile::TrustError::NotConfirmed) {
                eprintln!("[install] 已取消：未确认安装");
                return 2;
            }
            eprintln!("[install] 安装失败（{}）: {err}", err.code());
            1
        }
    }
}

/// 自检：可信安装端到端（首装确认、摘要校验、篡改拒绝、元数据/审计）。
fn selftest_trust_install() -> i32 {
    let root = PathBuf::from(".tmp/trust-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if std::fs::create_dir_all(package.join("lib")).is_err() {
        eprintln!("[selftest-trust] 创建目录失败");
        return 1;
    }
    let package_json = json!({ "name": "selftest.trust", "version": "0.1.0" });
    let _ = std::fs::write(package.join("package.json"), package_json.to_string());
    let _ = std::fs::write(package.join("lib/index.js"), "export const ok = true\n");
    let Ok(digest) = package_digest(&package) else {
        eprintln!("[selftest-trust] 计算摘要失败");
        return 1;
    };
    let trust = json!({
        "schema_version": 1,
        "plugin_id": "selftest.trust",
        "version": "0.1.0",
        "package": { "digest_sha256": digest }
    });
    let _ = std::fs::write(package.join("dshr-trust.json"), trust.to_string());

    // 1) 未确认 → 退出码 2 且未安装。
    let unconfirmed = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: false,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    );
    let destination = sidecar.join("node_modules").join("selftest.trust");
    let rejected_unconfirmed = unconfirmed == 2 && !destination.exists();

    // 2) 确认 → 安装成功，元数据/摘要一致。
    let confirmed = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: true,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    );
    let meta = read_installed_meta(&destination);
    let installed_ok = confirmed == 0
        && meta
            .as_ref()
            .map(|meta| meta.digest_sha256 == digest)
            .unwrap_or(false);

    // 3) 篡改内容后重装 → 摘要不匹配被拒。
    let _ = std::fs::write(package.join("lib/index.js"), "malicious()\n");
    let tampered = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: true,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    );
    let tamper_rejected = tampered == 1;

    // 4) 审计：至少包含 accepted 与 reject 两种记录。
    let audit =
        std::fs::read_to_string(profile_dir.join("install-audit.jsonl")).unwrap_or_default();
    let accepted = audit.contains("\"decision\":\"accepted\"");
    let rejected = audit.contains("\"decision\":\"rejected\"");
    let signature_status = meta.map(|meta| meta.signature.status).unwrap_or_default();
    let ok = rejected_unconfirmed && installed_ok && tamper_rejected && accepted && rejected;
    println!(
        "[selftest-trust] rejected_unconfirmed={rejected_unconfirmed} installed_ok={installed_ok} tamper_rejected={tamper_rejected} audit_accepted={accepted} audit_rejected={rejected} signature_status={signature_status}"
    );
    if ok {
        println!("[selftest-trust] PASS");
        0
    } else {
        println!("[selftest-trust] FAIL");
        1
    }
}

/// 侧车/插件安装目录（与 `--install-plugin`、sidecar 启动同源）。
fn sidecar_dir_for_app() -> PathBuf {
    std::env::var("DSHR_SIDECAR_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| app_root().join("sidecar/dsh-official"))
}

/// profile 目录（与 `--install-plugin` 同源）。
fn profile_dir_for_app() -> PathBuf {
    std::env::var("DSHR_PROFILE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".tmp/profile"))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// `chat --uninstall-plugin <name>`：目录 + profile + 审计（M4 W1）。
fn uninstall_plugin_cli(name: String) -> i32 {
    let sidecar_dir = sidecar_dir_for_app();
    let profile_dir = profile_dir_for_app();
    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[uninstall] 加载 profile 失败: {err}");
            return 1;
        }
    };
    match uninstall_package(&sidecar_dir, &name, &mut profile) {
        Ok(report) => {
            println!(
                "[uninstall] {} version={} dir_removed={} profile_removed={}",
                report.name,
                report.version.as_deref().unwrap_or("-"),
                report.removed_dir,
                report.removed_profile_entry
            );
            if let Some(error) = report.audit_error {
                eprintln!("[uninstall] 警告：审计写入失败（卸载已生效）: {error}");
            }
            println!(
                "[uninstall] 审计: {}",
                profile_dir.join("install-audit.jsonl").display()
            );
            0
        }
        Err(err) => {
            eprintln!("[uninstall] 卸载失败: {err}");
            1
        }
    }
}

fn trust_key_status(key: &TrustKey, at_ms: u64) -> &'static str {
    if key.revoked {
        "revoked"
    } else if key.not_before_ms.is_some_and(|before| at_ms < before) {
        "not-yet-valid"
    } else if key.not_after_ms.is_some_and(|after| at_ms > after) {
        "expired"
    } else {
        "active"
    }
}

/// `chat --trust-key <list|add|remove|revoke|rotate> ...`：本机信任密钥库管理。
fn trust_key_cli(args: &[String]) -> i32 {
    let profile_dir = profile_dir_for_app();
    if let Err(err) = std::fs::create_dir_all(&profile_dir) {
        eprintln!("[trust-key] 创建 profile 目录失败: {err}");
        return 1;
    }
    let action = args.first().map(String::as_str).unwrap_or("list");
    let at_ms = now_millis();
    match action {
        "list" => {
            let store = match TrustStore::load(&profile_dir) {
                Ok(store) => store,
                Err(err) => {
                    eprintln!("[trust-key] 密钥库无效（{}）: {err}", err.code());
                    return 1;
                }
            };
            if store.keys().is_empty() {
                println!("[trust-key] 密钥库为空（{}）", profile_dir.display());
            }
            for key in store.keys() {
                println!(
                    "[trust-key] key_id={} status={} not_before={} not_after={} note={}",
                    key.key_id,
                    trust_key_status(key, at_ms),
                    key.not_before_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| String::from("-")),
                    key.not_after_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| String::from("-")),
                    key.note.as_deref().unwrap_or("-")
                );
            }
            0
        }
        "add" => {
            let (Some(key_id), Some(public_key_hex)) = (args.get(1), args.get(2)) else {
                eprintln!("用法: chat --trust-key add <key_id> <public_key_hex> [--note <text>]");
                return 2;
            };
            let note = args
                .iter()
                .position(|arg| arg == "--note")
                .and_then(|index| args.get(index + 1))
                .cloned();
            let mut key = TrustKey::new(key_id, public_key_hex);
            key.note = note;
            match add_trust_key(&profile_dir, key, at_ms) {
                Ok(report) => {
                    println!(
                        "[trust-key] add {}（keys={}）",
                        report.key_id, report.key_count
                    );
                    0
                }
                Err(err) => {
                    eprintln!("[trust-key] 添加失败（{}）: {err}", err.code());
                    1
                }
            }
        }
        "remove" | "revoke" => {
            let Some(key_id) = args.get(1) else {
                eprintln!("用法: chat --trust-key {action} <key_id>");
                return 2;
            };
            let result = if action == "remove" {
                remove_trust_key(&profile_dir, key_id, at_ms)
            } else {
                revoke_trust_key(&profile_dir, key_id, at_ms)
            };
            match result {
                Ok(report) => {
                    println!(
                        "[trust-key] {} {}（keys={}）",
                        report.action, report.key_id, report.key_count
                    );
                    0
                }
                Err(err) => {
                    eprintln!("[trust-key] {action} 失败（{}）: {err}", err.code());
                    1
                }
            }
        }
        "rotate" => {
            let (Some(old_key_id), Some(new_key_id), Some(public_key_hex)) =
                (args.get(1), args.get(2), args.get(3))
            else {
                eprintln!(
                    "用法: chat --trust-key rotate <old_key_id> <new_key_id> <public_key_hex>"
                );
                return 2;
            };
            let new_key = TrustKey::new(new_key_id, public_key_hex);
            match rotate_trust_key(&profile_dir, old_key_id, new_key, at_ms) {
                Ok(report) => {
                    println!(
                        "[trust-key] rotate {} -> {}（keys={}）",
                        report.old_key_id.as_deref().unwrap_or("-"),
                        report.key_id,
                        report.key_count
                    );
                    0
                }
                Err(err) => {
                    eprintln!("[trust-key] 轮换失败（{}）: {err}", err.code());
                    1
                }
            }
        }
        other => {
            eprintln!(
                "未知 --trust-key 动作 `{other}`；可用: list / add / remove / revoke / rotate"
            );
            2
        }
    }
}

fn ed25519_keypair() -> Option<ring::signature::Ed25519KeyPair> {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).ok()?;
    ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).ok()
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 用给定 ed25519 钥重签包（自检专用；作者侧签名由 `tools/plugin-pack.mjs` 完成）。
fn sign_package_with_key(
    package_dir: &std::path::Path,
    name: &str,
    version: &str,
    keypair: &ring::signature::Ed25519KeyPair,
    key_id: &str,
) -> Result<String, String> {
    let digest = package_digest(package_dir).map_err(|err| err.to_string())?;
    let message = format!("dshr-trust/v1\n{name}\n{version}\n{digest}");
    let signature = keypair.sign(message.as_bytes());
    let trust = json!({
        "schema_version": 1,
        "plugin_id": name,
        "version": version,
        "package": { "digest_sha256": digest },
        "signature": {
            "scheme": "ed25519",
            "key_id": key_id,
            "value_hex": hex_lower(signature.as_ref())
        }
    });
    std::fs::write(
        package_dir.join("dshr-trust.json"),
        trust.to_string() + "\n",
    )
    .map_err(|err| format!("写 dshr-trust.json 失败: {err}"))?;
    Ok(digest)
}

/// 自检：安装 → 卸载（目录/profile/审计）→ 密钥轮换（旧钥拒、新钥装）。
fn selftest_plugin_lifecycle() -> i32 {
    use ring::signature::KeyPair as _;

    let root = PathBuf::from(".tmp/m4-plugin-lifecycle");
    let _ = std::fs::remove_dir_all(&root);
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if std::fs::create_dir_all(package.join("lib")).is_err() {
        eprintln!("[selftest-lifecycle] 创建目录失败");
        return 1;
    }
    let name = "m4.lifecycle";
    if std::fs::write(
        package.join("package.json"),
        json!({ "name": name, "version": "0.1.0" }).to_string(),
    )
    .is_err()
        || std::fs::write(package.join("lib/index.js"), "export const ok = true\n").is_err()
    {
        eprintln!("[selftest-lifecycle] 写包失败");
        return 1;
    }
    let Some(old_key) = ed25519_keypair() else {
        eprintln!("[selftest-lifecycle] 生成旧钥失败");
        return 1;
    };
    let Some(new_key) = ed25519_keypair() else {
        eprintln!("[selftest-lifecycle] 生成新钥失败");
        return 1;
    };

    // A. 首装（未签名 → --yes-install）→ 卸载。
    let digest_ok = package_digest(&package)
        .map(|digest| {
            let trust = json!({
                "schema_version": 1,
                "plugin_id": name,
                "version": "0.1.0",
                "package": { "digest_sha256": digest }
            });
            std::fs::write(package.join("dshr-trust.json"), trust.to_string()).is_ok()
        })
        .unwrap_or(false);
    let installed = digest_ok
        && install_plugin_with_options(
            &InstallCliOptions {
                package: package.clone(),
                yes_install: true,
                allow_downgrade: false,
            },
            &sidecar,
            &profile_dir,
        ) == 0;
    let destination = sidecar.join("node_modules").join(name);
    let installed_ok = installed && destination.exists();

    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[selftest-lifecycle] 加载 profile 失败: {err}");
            return 1;
        }
    };
    let uninstalled = uninstall_package(&sidecar, name, &mut profile);
    let audit_text =
        std::fs::read_to_string(profile_dir.join("install-audit.jsonl")).unwrap_or_default();
    let uninstall_ok = uninstalled
        .as_ref()
        .map(|report| report.removed_dir && report.removed_profile_entry)
        .unwrap_or(false)
        && !destination.exists()
        && !profile.entries().iter().any(|entry| entry.name == name)
        && audit_text.contains("\"action\":\"uninstall\"")
        && audit_text.contains("\"decision\":\"accepted\"");

    // B. 旧钥签名安装 → 轮换 → 旧钥被拒 → 新钥重签安装。
    let key_added = add_trust_key(
        &profile_dir,
        TrustKey::new("old", hex_lower(old_key.public_key().as_ref())),
        now_millis(),
    )
    .is_ok();
    let signed_old = sign_package_with_key(&package, name, "0.1.0", &old_key, "old").is_ok();
    let old_install = signed_old
        && install_plugin_with_options(
            &InstallCliOptions {
                package: package.clone(),
                yes_install: false,
                allow_downgrade: false,
            },
            &sidecar,
            &profile_dir,
        ) == 0;
    let rotated = rotate_trust_key(
        &profile_dir,
        "old",
        TrustKey::new("new", hex_lower(new_key.public_key().as_ref())),
        now_millis(),
    )
    .is_ok();
    let store_ok = TrustStore::load(&profile_dir)
        .map(|store| {
            store.get("old").is_some_and(|key| key.revoked)
                && store.get("new").is_some_and(|key| !key.revoked)
        })
        .unwrap_or(false);
    let old_rejected = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: false,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    ) == 1;
    let signed_new = sign_package_with_key(&package, name, "0.1.0", &new_key, "new").is_ok();
    let new_install = signed_new
        && install_plugin_with_options(
            &InstallCliOptions {
                package: package.clone(),
                yes_install: false,
                allow_downgrade: false,
            },
            &sidecar,
            &profile_dir,
        ) == 0;
    let meta_ok = read_installed_meta(&destination)
        .map(|meta| {
            meta.signature.status == "verified" && meta.signature.key_id.as_deref() == Some("new")
        })
        .unwrap_or(false);
    let key_audit =
        std::fs::read_to_string(profile_dir.join("trust-key-audit.jsonl")).unwrap_or_default();
    let key_audit_ok = key_audit.contains("\"action\":\"add\"")
        && key_audit.contains("\"action\":\"rotate\"")
        && key_audit.contains("\"old_key_id\":\"old\"");

    let ok = installed_ok
        && uninstall_ok
        && key_added
        && old_install
        && rotated
        && store_ok
        && old_rejected
        && new_install
        && meta_ok
        && key_audit_ok;
    println!(
        "[selftest-lifecycle] installed={installed_ok} uninstalled={uninstall_ok} key_added={key_added} old_install={old_install} rotated={rotated} store_ok={store_ok} old_rejected={old_rejected} new_install={new_install} meta_ok={meta_ok} key_audit={key_audit_ok}"
    );
    if ok {
        println!("[selftest-lifecycle] PASS");
        0
    } else {
        println!("[selftest-lifecycle] FAIL");
        1
    }
}

/// 宿主兼容基线：用 `dshr-plugin-api` 的 semver（0.1.0），不是应用 workspace 的 0.0.0。
fn host_sdk_version() -> HostVersion {
    HostVersion::parse(dshr_plugin_api::HOST_VERSION).unwrap_or_else(|_| HostVersion::new(0, 1, 0))
}

/// 从 profile 的 enabled 条目装载已安装插件（spec/cpp-v1.md §3.3）：
/// - `external-process` → [`ExternalPluginSpec`] + [`ExternalPlugin::spawn`]；
/// - `wasm-component` → [`PluginHost::load_wasm_plugin`]（P1-1：不再静默跳过）；
/// - 其它 runtime kind（如 `rust-static`）装错位置 → 记为 plugin error（不静默）。
///
/// 单个插件失败只记诊断，不影响其它插件与内置工具。
fn load_enabled_external_plugins(
    plugins: &mut PluginHost,
    profile: &Profile,
    sidecar_dir: &std::path::Path,
) -> Vec<String> {
    let mut errors = Vec::new();
    let node_modules = sidecar_dir.join("node_modules");
    for entry in profile.entries().iter().filter(|entry| entry.enabled) {
        let package_dir = node_modules.join(&entry.name);
        let manifest_path = package_dir.join("plugin.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = match Manifest::from_path(&manifest_path) {
            Ok(manifest) => manifest,
            Err(err) => {
                errors.push(format!("插件 {} 的 manifest 解析失败: {err}", entry.name));
                continue;
            }
        };
        match manifest.runtime.kind {
            RuntimeKind::ExternalProcess => {
                let plugin_id = manifest.plugin.id.clone();
                let spec = ExternalPluginSpec::new(manifest, &package_dir, host_sdk_version(), 1);
                match ExternalPlugin::spawn(spec) {
                    Ok(plugin) => {
                        let id = plugin.id().to_owned();
                        if let Err(err) = plugins.load(Box::new(plugin)) {
                            errors.push(format!("插件 {id} 装载失败: {err}"));
                        } else {
                            eprintln!("[plugin] external-process 已装载: {id}");
                        }
                    }
                    Err(err) => errors.push(format!("插件 {plugin_id} 启动失败: {err}")),
                }
            }
            RuntimeKind::WasmComponent => {
                let plugin_id = manifest.plugin.id.clone();
                match plugins.load_wasm_plugin(&manifest, &package_dir, &host_sdk_version()) {
                    Ok(()) => eprintln!("[plugin] wasm-component 已装载: {plugin_id}"),
                    Err(err) => errors.push(format!("插件 {plugin_id} WASM 装载失败: {err}")),
                }
            }
            // 安装包里出现 native-kind 的 plugin.toml 属于包装错位：必须报错，
            // 不能像 W5 前那样静默 `continue`（review P1-1）。
            other => errors.push(format!(
                "插件 {} 的 runtime.kind={other:?} 不支持从安装包装载（rust-static 需内置）",
                entry.name
            )),
        }
    }
    errors
}

/// 自检：external-process 插件产品路径（安装 → profile 装载 → 工具调用 → 卸载 → 重载）。
/// external-process fixture 插件的一次性准备（复制 exe → 写 manifest/包摘要 → 信任安装）。
///
/// `--selftest-external-plugin` 与 `--selftest-adc-panel` 共用；失败已按 `tag` 前缀打印，
/// 调用方直接返回 1 即可。
struct FixturePluginSetup {
    profile: Profile,
    sidecar: PathBuf,
    installed: bool,
}

fn prepare_fixture_plugin(
    root: &std::path::Path,
    plugin_id: &str,
    tag: &str,
) -> Option<FixturePluginSetup> {
    let _ = std::fs::remove_dir_all(root);
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if std::fs::create_dir_all(&package).is_err() {
        eprintln!("[{tag}] 创建目录失败");
        return None;
    }
    let fixture_name = if cfg!(windows) {
        "plugin-fixture.exe"
    } else {
        "plugin-fixture"
    };
    let Some(fixture) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(fixture_name)))
    else {
        eprintln!("[{tag}] 无法定位 fixture 可执行文件");
        return None;
    };
    if !fixture.is_file() {
        eprintln!(
            "[{tag}] 缺少 {}；先运行 `cargo build -p dshr-plugin-host --bins`",
            fixture.display()
        );
        return None;
    }
    if std::fs::copy(&fixture, package.join(fixture_name)).is_err() {
        eprintln!("[{tag}] 复制 fixture 失败");
        return None;
    }
    let manifest = format!(
        r#"
[plugin]
id = "{plugin_id}"
version = "0.1.0"
api = "cordis-portable/v1"
description = "external-process selftest fixture"

[runtime]
kind = "external-process"
entry = "{fixture_name}"
max_memory_mb = 128
timeout_ms = 60000

[permissions]
workspace_read = false
workspace_write = false
network = false
"#
    );
    let package_json = json!({ "name": plugin_id, "version": "0.1.0" });
    let _ = std::fs::write(package.join("plugin.toml"), manifest);
    let _ = std::fs::write(package.join("package.json"), package_json.to_string());
    let Ok(digest) = package_digest(&package) else {
        eprintln!("[{tag}] 计算包摘要失败");
        return None;
    };
    let trust = json!({
        "schema_version": 1,
        "plugin_id": plugin_id,
        "version": "0.1.0",
        "package": { "digest_sha256": digest }
    });
    let _ = std::fs::write(package.join("dshr-trust.json"), trust.to_string());

    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[{tag}] 加载 profile 失败: {err}");
            return None;
        }
    };
    let policy = TrustPolicy {
        allow_downgrade: false,
        require_confirm_for_unsigned: true,
        store: TrustStore::default(),
        now_ms: None,
    };
    let install = match install_package_trusted(&sidecar, &package, &mut profile, &policy, |_| true)
    {
        Ok(report) => report,
        Err(err) => {
            eprintln!("[{tag}] 安装失败（{}）: {err}", err.code());
            return None;
        }
    };
    let installed = install.install.destination.join("plugin.toml").is_file();
    Some(FixturePluginSetup {
        profile,
        sidecar,
        installed,
    })
}

fn selftest_external_plugin() -> i32 {
    let root = PathBuf::from(".tmp/external-plugin-selftest");
    let Some(setup) = prepare_fixture_plugin(&root, "selftest.external", "selftest-external")
    else {
        return 1;
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let approvals = ApprovalBroker::new();
    let mut state = ChatState::open(
        root.join("session.jsonl"),
        SESSION_ID,
        &cancel,
        approvals,
        false,
        false,
    );
    let errors = load_enabled_external_plugins(&mut state.plugins, &setup.profile, &setup.sidecar);
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("[selftest-external] {error}");
        }
        println!("[selftest-external] FAIL（装载失败）");
        return 1;
    }
    if !state.plugins.is_loaded("selftest.external") {
        println!("[selftest-external] FAIL（插件未装载）");
        return 1;
    }
    let echo = state.plugins.call("echo", &json!({ "text": "hi" }));
    let echo_ok = echo
        .as_ref()
        .map(|value| value.to_string().contains("hi"))
        .unwrap_or(false);
    let unloaded = state.plugins.unload("selftest.external").is_ok();
    let reload_errors =
        load_enabled_external_plugins(&mut state.plugins, &setup.profile, &setup.sidecar);
    let reload_ok = reload_errors.is_empty()
        && state.plugins.is_loaded("selftest.external")
        && state
            .plugins
            .call("echo", &json!({ "text": "again" }))
            .map(|value| value.to_string().contains("again"))
            .unwrap_or(false);
    // 面板声明也随 ready 注册（ADC panel 的宿主侧元数据）。
    let panel_ok = state
        .plugins
        .panels()
        .iter()
        .any(|panel| panel.plugin_id == "selftest.external");
    let installed_ok = setup.installed;
    let ok = echo_ok && unloaded && reload_ok && panel_ok && installed_ok;
    println!(
        "[selftest-external] installed_ok={installed_ok} echo_ok={echo_ok} unloaded={unloaded} reload_ok={reload_ok} panel_ok={panel_ok}"
    );
    if ok {
        println!("[selftest-external] PASS");
        0
    } else {
        println!("[selftest-external] FAIL");
        1
    }
}

/// `--selftest-wasm`：WASM 插件的产品链路（review P1-1）。
///
/// 链路：临时 WASM 包（package.json + plugin.toml + plugin.wasm(WAT) + schemas +
/// `dshr-trust.json`）→ `install_package_trusted` → 从 profile 装载（走
/// `PluginHost::load_wasm_plugin`）→ 调用 `echo` → 渲染 `ui.render` panel →
/// `trap` 工具撞 fuel 看门狗 → 宿主存活（再 call echo）→ unload。
///
/// 包建在 data root 下（不依赖用户 `%TEMP%` 是否可写），断言不联网。
fn selftest_wasm() -> i32 {
    let root = data_root().join("selftests").join("wasm-host");
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(&root) {
        eprintln!(
            "[selftest-wasm] FAIL：创建临时目录 {} 失败: {error}",
            root.display()
        );
        return 1;
    }
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if let Err(error) = std::fs::create_dir_all(package.join("schemas")) {
        eprintln!("[selftest-wasm] FAIL：创建包目录失败: {error}");
        return 1;
    }
    let _ = std::fs::create_dir_all(&sidecar);

    // 1) WAT fixture：'e'=echo 回显；'u'=ui.render 返回 data 段元素树；其它（trap）=死循环。
    let wat = r#"
(module
  (memory (export "memory") 1)
  (data (i32.const 1024) "{\"type\":\"Text\",\"props\":{\"text\":\"wasm\"}}")
  (global $heap (mut i32) (i32.const 4096))
  (func (export "dshr_alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap))
    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
    (local.get $ptr))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (local $tag i32)
    (local.set $tag (i32.load8_u offset=9 (local.get $ptr)))
    (if (i32.eq (local.get $tag) (i32.const 117))
      (then (return (i64.or
        (i64.shl (i64.extend_i32_u (i32.const 1024)) (i64.const 32))
        (i64.const 39)))))
    (if (i32.eq (local.get $tag) (i32.const 101))
      (then (return (i64.or
        (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
        (i64.extend_i32_u (local.get $len))))))
    (loop $spin (br $spin))
    (i64.const -1))
)
"#;
    let plugin_id = "selftest.wasm";
    let manifest = r#"
[plugin]
id = "selftest.wasm"
version = "0.1.0"
api = "cordis-portable/v1"
description = "wasm-component selftest fixture"

[runtime]
kind = "wasm-component"
entry = "plugin.wasm"
fuel = 2000000
max_memory_mb = 32
timeout_ms = 30000

[[tools]]
name = "echo"
description = "echo the request JSON"
input_schema = "schemas/echo.input.json"
output_schema = "schemas/echo.output.json"

[[tools]]
name = "trap"
description = "spin until the fuel watchdog kills the invocation"
input_schema = "schemas/trap.input.json"
output_schema = "schemas/trap.output.json"

[[ui]]
slot = "workspace.panel"
id = "selftest.wasm.panel"
label = "WASM Panel"
view = "selftest.wasm.view"
location = "right"
"#;
    let writes = [
        (package.join("plugin.wasm"), wat.as_bytes().to_vec()),
        (package.join("plugin.toml"), manifest.as_bytes().to_vec()),
        (
            package.join("package.json"),
            json!({ "name": plugin_id, "version": "0.1.0" })
                .to_string()
                .into_bytes(),
        ),
    ];
    for (path, bytes) in writes {
        if let Err(error) = std::fs::write(&path, bytes) {
            eprintln!("[selftest-wasm] FAIL：写 {} 失败: {error}", path.display());
            return 1;
        }
    }
    for name in ["echo", "trap"] {
        for direction in ["input", "output"] {
            let schema = json!({ "type": "object" });
            if let Err(error) = std::fs::write(
                package.join(format!("schemas/{name}.{direction}.json")),
                schema.to_string(),
            ) {
                eprintln!("[selftest-wasm] FAIL：写 schema 失败: {error}");
                return 1;
            }
        }
    }

    // 2) 信任安装（与产品安装路径一致）。
    let Ok(digest) = package_digest(&package) else {
        eprintln!("[selftest-wasm] FAIL：计算包摘要失败");
        return 1;
    };
    let trust = json!({
        "schema_version": 1,
        "plugin_id": plugin_id,
        "version": "0.1.0",
        "package": { "digest_sha256": digest }
    });
    let _ = std::fs::write(package.join("dshr-trust.json"), trust.to_string());

    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("[selftest-wasm] FAIL：加载 profile 失败: {error}");
            return 1;
        }
    };
    let policy = TrustPolicy {
        allow_downgrade: false,
        require_confirm_for_unsigned: true,
        store: TrustStore::default(),
        now_ms: None,
    };
    let install = match install_package_trusted(&sidecar, &package, &mut profile, &policy, |_| true)
    {
        Ok(report) => report,
        Err(error) => {
            eprintln!(
                "[selftest-wasm] FAIL：安装失败（{}）: {error}",
                error.code()
            );
            return 1;
        }
    };
    let installed_ok = install.install.destination.join("plugin.toml").is_file();

    // 3) 从 profile 装载（产品路径：loader 内部分发到 WASM 分支）。
    let mut plugins = PluginHost::new(ApprovalPolicy::desktop());
    let errors = load_enabled_external_plugins(&mut plugins, &profile, &sidecar);
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("[selftest-wasm] {error}");
        }
        println!("[selftest-wasm] FAIL（装载失败）");
        return 1;
    }
    let loaded = plugins.is_loaded(plugin_id);
    let echo_ok = plugins
        .call("echo", &json!({ "text": "wasm" }))
        .ok()
        .and_then(|value| {
            value
                .get("arguments")
                .and_then(|arguments| arguments.get("text"))
                .and_then(|text| text.as_str())
                .map(str::to_owned)
        })
        .as_deref()
        == Some("wasm");
    let panel_ok = plugins
        .panels()
        .iter()
        .any(|panel| panel.plugin_id == plugin_id && panel.panel.id == "selftest.wasm.panel")
        && plugins
            .panels()
            .iter()
            .any(|panel| panel.panel.view_name() == "selftest.wasm.view");
    let render_ok = plugins
        .render_panel(plugin_id, "selftest.wasm.view")
        .map(|tree| tree["type"] == json!("Text"))
        .unwrap_or(false);

    // 4) trap：fuel 看门狗杀本次调用，宿主/插件存活（再 call echo 成功）。
    let trap = plugins.call("trap", &json!({}));
    let trap_isolated = trap
        .as_ref()
        .err()
        .map(|error| {
            let text = error.to_string();
            text.contains("E_QUOTA") || text.contains("E_TIMEOUT")
        })
        .unwrap_or(false);
    let survived = plugins
        .call("echo", &json!({ "text": "alive" }))
        .map(|value| value.to_string().contains("alive"))
        .unwrap_or(false);
    let unloaded = plugins.unload(plugin_id).is_ok() && !plugins.is_loaded(plugin_id);
    let ok = installed_ok
        && loaded
        && echo_ok
        && panel_ok
        && render_ok
        && trap_isolated
        && survived
        && unloaded;
    println!(
        "[selftest-wasm] installed_ok={installed_ok} loaded={loaded} echo_ok={echo_ok} panel_ok={panel_ok} render_ok={render_ok} trap_isolated={trap_isolated} survived={survived} unloaded={unloaded}"
    );
    let _ = std::fs::remove_dir_all(&root);
    if ok {
        println!("[selftest-wasm] PASS");
        0
    } else {
        println!("[selftest-wasm] FAIL");
        1
    }
}

/// `--selftest-adc-panel`：ADC panel 产品链路（无窗口，验证到 `AnyElement` 构造层）。
///
/// 链路：external fixture 装载 → `PluginHost::render_panel` → `adc_panel_body` →
/// GPUI 元素。断言：合法树 0 诊断、未知元素 → Placeholder + 诊断、未知 view /
/// 未知插件 / 不实现 view 都是可读错误、面板缓存可全部构造且无 panic。
fn selftest_adc_panel() -> i32 {
    let root = PathBuf::from(".tmp/adc-panel-selftest");
    let Some(setup) = prepare_fixture_plugin(&root, "selftest.adc", "selftest-adc-panel") else {
        return 1;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open(
        root.join("session.jsonl"),
        SESSION_ID,
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );
    let errors = load_enabled_external_plugins(&mut state.plugins, &setup.profile, &setup.sidecar);
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("[selftest-adc-panel] {error}");
        }
        println!("[selftest-adc-panel] FAIL（装载失败）");
        return 1;
    }
    if !state.plugins.is_loaded("selftest.adc") {
        println!("[selftest-adc-panel] FAIL（插件未装载）");
        return 1;
    }
    // 装载后刷新面板缓存——产品面（右侧 panel）渲染读的就是这份缓存。
    state.refresh_adc_panels();
    let theme = Theme::light();
    let mut ok = true;

    // 1) 合法树：render_panel → adc_panel_body，0 诊断、元素构造成功。
    let legal_tree = match state.plugins.render_panel("selftest.adc", "fixture.adc") {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("[selftest-adc-panel] render_panel 失败: {error}");
            println!("[selftest-adc-panel] FAIL");
            return 1;
        }
    };
    let (legal_element, legal_diagnostics) = adc_panel_body(&theme, &legal_tree);
    drop(legal_element);
    let legal_ok = legal_diagnostics.is_empty();
    ok &= legal_ok;

    // 2) 未知元素（插件返回 Sparkline）：Placeholder + 诊断，不 panic。
    let unknown_tree = match state
        .plugins
        .render_panel("selftest.adc", "fixture.unknown")
    {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("[selftest-adc-panel] 未知元素 view 渲染失败: {error}");
            println!("[selftest-adc-panel] FAIL");
            return 1;
        }
    };
    let (unknown_element, unknown_diagnostics) = adc_panel_body(&theme, &unknown_tree);
    drop(unknown_element);
    let unknown_ok = unknown_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::UnknownElement);
    ok &= unknown_ok;

    // 3) 未知 view / 未知插件 / 不实现 view：可读错误，不 panic。
    let unknown_view_ok = state.plugins.render_panel("selftest.adc", "nope").is_err();
    let unknown_plugin_ok = state
        .plugins
        .render_panel("missing.plugin", "fixture.adc")
        .as_ref()
        .is_err_and(|error| error.to_string().contains("missing.plugin"));
    let default_view_ok = state
        .plugins
        .render_panel("builtin.probe-write", "anything")
        .as_ref()
        .is_err_and(|error| error.to_string().contains("does not render views"));
    ok &= unknown_view_ok && unknown_plugin_ok && default_view_ok;

    // 4) 面板缓存：至少 2 条 workspace.panel（fixture 声明了 right/bottom 两条），
    //    每条都能构造元素；失败走错误占位（这里不应失败）。
    let panels = state.adc_panels.len();
    let mut rendered = 0usize;
    let mut panel_errors = 0usize;
    let mut diag_total = 0usize;
    for panel in &state.adc_panels {
        match &panel.tree {
            Some(tree) => {
                let (element, diagnostics) = adc_panel_body(&theme, tree);
                drop(element);
                diag_total += diagnostics.len();
                rendered += 1;
            }
            None => panel_errors += 1,
        }
    }
    let panels_ok = panels >= 2 && rendered == panels && panel_errors == 0;
    ok &= panels_ok;

    println!(
        "[selftest-adc-panel] panels={panels} rendered={rendered} panel_errors={panel_errors} legal_diags={} unknown_diag={unknown_ok} diag_total={diag_total} unknown_view={unknown_view_ok} unknown_plugin={unknown_plugin_ok} default_view={default_view_ok}",
        legal_diagnostics.len()
    );
    if ok {
        println!("[selftest-adc-panel] PASS");
        0
    } else {
        println!("[selftest-adc-panel] FAIL");
        1
    }
}

/// 自检用的"重复同一工具调用" provider：前 3 步都发同一个工具+参数，第 4 步收尾。
///
/// 只用于 `--selftest-defaults` 验证 `agent-core` 的重复工具软提醒阈值（3/5/8）。
struct RepeatProbeProvider;

impl ModelProvider for RepeatProbeProvider {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        let executed = request
            .messages
            .iter()
            .filter(|message| matches!(message, ChatMessage::Tool { .. }))
            .count();
        if executed >= 3 {
            return Ok(ModelReply::text_only("done"));
        }
        Ok(ModelReply {
            text: String::new(),
            tool_calls: vec![ModelToolCall {
                id: format!("repeat-{executed}"),
                name: "probe_repeat".to_owned(),
                arguments: json!({ "text": "same" }),
            }],
        })
    }
}

/// 自检用的执行器：无论调用什么都返回成功。
struct ProbeRepeatExecutor;

impl ToolExecutor for ProbeRepeatExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "probe_repeat".to_owned(),
            description: "repeat probe".to_owned(),
            parameters: json!({ "type": "object" }),
        }]
    }

    fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
        ToolOutcome::success(
            json!([{ "type": "json", "value": { "ok": true } }]),
            json!({ "ok": true }),
        )
    }
}

/// `--selftest-defaults`：M3 默认差异接线的无网络自检。
///
/// 覆盖：approval 三种权限模式（`DSHR_PERMISSION_MODE`）、重复工具软提醒在真实 agent
/// 序列里触发并经 `tool.repeat` 落 durable 日志、sidecar per-tool timeout 往返、
/// retainRatio 与 reasoning effort 的 env 解析。全部断言走纯函数/内存日志，不联网。
fn selftest_defaults() -> i32 {
    let mut ok = true;

    // 1) approval 模式：env → ApprovalPolicy（缺省 workspace-write = 原 desktop 行为）。
    let write_tool = ToolDefinition::new(
        "write",
        "写文件（自检探针）",
        ToolOrigin::Native {
            plugin_id: "selftest.defaults".to_owned(),
        },
    )
    .with_presentation("diff");
    let read_tool = ToolDefinition::new(
        "read",
        "读文件（自检探针）",
        ToolOrigin::Native {
            plugin_id: "selftest.defaults".to_owned(),
        },
    )
    .with_presentation("read");

    std::env::remove_var(PermissionMode::ENV);
    let default_mode_ok = PermissionMode::from_env() == PermissionMode::WorkspaceWrite;
    let default_policy_ok = matches!(
        ApprovalPolicy::from_env().check(&write_tool, &json!({})),
        PolicyDecision::Ask
    ) && matches!(
        ApprovalPolicy::from_env().check(&read_tool, &json!({})),
        PolicyDecision::Allow
    );

    std::env::set_var(PermissionMode::ENV, "read-only");
    let read_only_mode = PermissionMode::from_env();
    let read_only_policy = ApprovalPolicy::from_env();
    let read_only_ok = read_only_mode == PermissionMode::ReadOnly
        && matches!(
            read_only_policy.check(&write_tool, &json!({})),
            PolicyDecision::Deny(_)
        )
        && matches!(
            read_only_policy.check(&read_tool, &json!({})),
            PolicyDecision::Allow
        );

    std::env::set_var(PermissionMode::ENV, "danger-full-access");
    let danger_mode = PermissionMode::from_env();
    let danger_policy = ApprovalPolicy::from_env();
    let danger_ok = danger_mode == PermissionMode::DangerFullAccess
        && matches!(
            danger_policy.check(&write_tool, &json!({})),
            PolicyDecision::Allow
        )
        && matches!(
            danger_policy.check(&read_tool, &json!({})),
            PolicyDecision::Allow
        );
    std::env::remove_var(PermissionMode::ENV);
    ok &= default_mode_ok && default_policy_ok && read_only_ok && danger_ok;

    // 2) 重复工具软提醒：默认阈值 3 在真实 agent 序列里触发，并落成 `tool.repeat`。
    std::env::remove_var(dshr_agent_core::REPEAT_TOOL_REMINDER_CONFIG_ENV);
    std::env::remove_var(dshr_agent_core::REPEAT_TOOL_REMINDER_THRESHOLDS_ENV);
    let mut agent = Agent::new(RepeatProbeProvider);
    let default_thresholds_ok = agent.repeat_reminder_config().enabled
        && agent.repeat_reminder_config().thresholds == vec![3, 5, 8];
    let mut executor = ProbeRepeatExecutor;
    let events = agent.run_turn("repeat", &mut executor);
    let tool_calls = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolCall(_)))
        .count();
    let mut log = SessionLog::new("s-selftest-defaults");
    let ts = now_ts();
    let _ = log.append(EventSpec::durable(
        "session.created",
        &ts,
        SessionCreated::root(".").payload(),
    ));
    let written = append_repeat_reminders(&mut agent, &mut log, &ts);
    let reminder = log
        .events()
        .iter()
        .find(|event| event.event_type == "tool.repeat");
    let repeat_event_ok = reminder.is_some_and(|event| {
        event.payload["tool"] == json!("probe_repeat")
            && event.payload["count"] == json!(3)
            && event.payload["threshold"] == json!(3)
            && event.payload["message"]
                .as_str()
                .is_some_and(|text| text.contains("重复工具调用提醒"))
    });
    let repeat_ok = default_thresholds_ok && tool_calls == 3 && written == 1 && repeat_event_ok;
    ok &= repeat_ok;

    // 3) sidecar per-tool timeout：`tool.register.timeout_ms` 往返 + 归一化。
    let registration = parse_tool_registration(
        &json!({"name": "bash", "description": "run", "timeout_ms": 5_000}),
        "dsh-official",
        "reg-selftest-1",
    );
    let definition = registration.to_tool_definition("gen-selftest");
    let timeout_ok = registration.timeout_ms == Some(5_000)
        && definition.timeout_ms == 5_000
        && definition.effective_timeout_ms() == 5_000
        && definition.origin.generation() == Some("gen-selftest")
        && parse_tool_registration(
            &json!({"name": "slow", "timeout_ms": 50_000_000u64}),
            "p",
            "r",
        )
        .timeout_ms
            == Some(600_000)
        && parse_tool_registration(&json!({"name": "read"}), "p", "r")
            .timeout_ms
            .is_none();
    ok &= timeout_ok;

    // 4) retainRatio：默认 DSH 0.16，env 可回退旧行为 0.6。
    std::env::remove_var(dshr_agent_context::RETAIN_RATIO_ENV);
    let retain_default = dshr_agent_context::retain_ratio_from_env();
    let retain_default_ok = (retain_default - dshr_agent_context::DSH_RETAIN_RATIO).abs() < 1e-6;
    std::env::set_var(dshr_agent_context::RETAIN_RATIO_ENV, "0.6");
    let retain_override = dshr_agent_context::retain_ratio_from_env();
    let retain_override_ok = (retain_override - 0.6).abs() < 1e-6;
    std::env::remove_var(dshr_agent_context::RETAIN_RATIO_ENV);
    ok &= retain_default_ok && retain_override_ok;

    // 5) reasoning effort：provider 构造时读 env，chat 不覆盖。
    std::env::remove_var(ReasoningEffort::ENV);
    let reasoning_unset_ok = DeepSeekProvider::new("selftest-key")
        .reasoning_effort()
        .is_none();
    std::env::set_var(ReasoningEffort::ENV, "high");
    let reasoning_high_ok = DeepSeekProvider::new("selftest-key").reasoning_effort()
        == Some(ReasoningEffort::High)
        && OpenAiProvider::new("selftest-key").reasoning_effort() == Some(ReasoningEffort::High);
    std::env::set_var(ReasoningEffort::ENV, "max");
    let reasoning_max_ok =
        OpenAiProvider::new("selftest-key").reasoning_effort() == Some(ReasoningEffort::Max);
    std::env::remove_var(ReasoningEffort::ENV);
    ok &= reasoning_unset_ok && reasoning_high_ok && reasoning_max_ok;

    println!(
        "[selftest-defaults] approval_default={default_policy_ok} approval_read_only={read_only_ok} \
         approval_danger={danger_ok} repeat_reminders={tool_calls} tool_repeat_events={written} \
         repeat_event_ok={repeat_event_ok} timeout_roundtrip={timeout_ok} \
         retain_default={retain_default:.2} retain_env={retain_override:.2} \
         reasoning_env={reasoning_high_ok}"
    );
    if ok {
        println!("[selftest-defaults] PASS");
        0
    } else {
        println!("[selftest-defaults] FAIL");
        1
    }
}

/// 会话树产品面自检：spawn_child 真执行（子会话独立日志 + 父链接 + 父子事件）、
/// 默认单层（子会话不能再派发）、中断传播（interrupted_by_parent）、
/// 侧栏树顺序/深度、重启后 from_logs 重建。
fn selftest_tree() -> i32 {
    let dir = PathBuf::from(".tmp/tree-selftest");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let store = SessionIndex::new(&dir);
    let approvals = ApprovalBroker::new();
    let load = |id: &str| -> Option<SessionLog> {
        let meta = store.scan().ok()?.into_iter().find(|meta| meta.id == id)?;
        let text = std::fs::read_to_string(&meta.path).ok()?;
        SessionLog::from_jsonl(&meta.id, &text).ok()
    };

    // 1) 父会话派发一次子会话；子会话同步跑完一轮并回写父子日志。
    let cancel = Arc::new(AtomicBool::new(false));
    let mut parent = ChatState::new(
        dir.join("s-parent.jsonl"),
        "s-parent",
        &cancel,
        approvals.clone(),
    );
    let spawn = parent.execute_tool(&ToolCallRequest {
        call_id: String::from("tree-spawn-1"),
        tool: SPAWN_CHILD_TOOL.to_owned(),
        arguments: json!({ "prompt": "你好，子会话只做一句话回答" }),
    });
    let child_id = spawn
        .value
        .get("child_session_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let depth = spawn
        .value
        .get("depth")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let parent_created = parent.log.created().is_some();
    let parent_spawned = parent
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "child.spawned");
    let parent_finished = parent
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "child.finished");
    let child_log = load(&child_id);
    let child_parent_link = child_log
        .as_ref()
        .and_then(|log| log.created())
        .and_then(|created| created.parent_session_id.clone())
        .as_deref()
        == Some("s-parent");
    let child_has_reply = child_log.as_ref().is_some_and(|log| {
        log.events()
            .iter()
            .any(|event| event.event_type == "message.assistant.settled")
    });
    let order = session_tree_order(&store);
    let parent_index = order.iter().position(|(id, _)| id == "s-parent");
    let child_index = order.iter().position(|(id, _)| id == &child_id);
    let tree_ok = depth == 1
        && child_parent_link
        && child_has_reply
        && parent_spawned
        && parent_finished
        && parent_created
        && order
            .iter()
            .any(|(id, level)| id == &child_id && *level == 1)
        && parent_index.is_some()
        && child_index.is_some()
        && parent_index < child_index;

    // 2) 子会话不再派发（默认单层）。
    let mut child_state = ChatState::open(
        store.path_for(&child_id),
        &child_id,
        &cancel,
        approvals.clone(),
        false,
        false,
    );
    let nested = child_state.execute_tool(&ToolCallRequest {
        call_id: String::from("tree-spawn-nested"),
        tool: SPAWN_CHILD_TOOL.to_owned(),
        arguments: json!({ "prompt": "孙会话" }),
    });
    let nested_rejected = nested.is_error;

    // 2b) 产品链路：mock 模型选择 spawn_child → 执行器派发 → 子会话回写 → 模型拿到结果。
    let cancel3 = Arc::new(AtomicBool::new(false));
    let mut parent3 = ChatState::new(
        dir.join("s-parent3.jsonl"),
        "s-parent3",
        &cancel3,
        approvals.clone(),
    );
    let loop_reply = parent3.stream_turn("请派发一个子会话，让它回答：子会话你好", &mut |_| {});
    let has_event =
        |log: &SessionLog, kind: &str| log.events().iter().any(|event| event.event_type == kind);
    let loop_ok = loop_reply
        .as_deref()
        .is_some_and(|reply| !reply.is_empty() && !reply.starts_with("处理失败"))
        && parent3.log.events().iter().any(|event| {
            event.event_type == "tool.call"
                && event.payload.get("tool").and_then(|value| value.as_str())
                    == Some(SPAWN_CHILD_TOOL)
        })
        && has_event(&parent3.log, "child.spawned")
        && has_event(&parent3.log, "tool.result")
        && has_event(&parent3.log, "child.finished");

    // 3) 中断传播：父取消后派发 → 父 session.interrupted + 子 interrupted_by_parent。
    let cancel2 = Arc::new(AtomicBool::new(false));
    let mut parent2 = ChatState::new(
        dir.join("s-parent2.jsonl"),
        "s-parent2",
        &cancel2,
        approvals.clone(),
    );
    cancel2.store(true, Ordering::Relaxed);
    let interrupted_spawn = parent2.execute_tool(&ToolCallRequest {
        call_id: String::from("tree-spawn-interrupted"),
        tool: SPAWN_CHILD_TOOL.to_owned(),
        arguments: json!({ "prompt": "中断测试" }),
    });
    let child2_id = interrupted_spawn
        .value
        .get("child_session_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let outcome_interrupted = interrupted_spawn
        .value
        .get("outcome")
        .and_then(|value| value.as_str())
        == Some("interrupted");
    let child2_log = load(&child2_id);
    let child_interrupted = child2_log.as_ref().is_some_and(|log| {
        log.events()
            .iter()
            .any(|event| event.event_type == "session.interrupted_by_parent")
    });
    let parent_interrupted = parent2
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "session.interrupted");

    // 4) 重启恢复：从磁盘日志重建运行态树，深度与中断状态一致。
    let logs: Vec<SessionLog> = store
        .scan()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|meta| {
            std::fs::read_to_string(&meta.path)
                .ok()
                .and_then(|text| SessionLog::from_jsonl(&meta.id, &text).ok())
        })
        .collect();
    let rebuilt = SessionTreeRuntime::from_logs(logs, DEFAULT_MAX_DEPTH);
    let (rebuilt_depth, rebuilt_interrupted, replay_equal) = match &rebuilt {
        Ok(runtime) => (
            runtime.depth(&child_id),
            runtime
                .session(&child2_id)
                .map(|session| session.status.is_interrupted())
                .unwrap_or(false),
            // 父日志回放：新事件不影响对话模型构建（未知事件忽略）。
            child_log.as_ref().is_some_and(|log| {
                let jsonl = log.to_jsonl();
                SessionLog::from_jsonl(log.session_id(), &jsonl)
                    .map(|reloaded| reloaded.len() == log.len())
                    .unwrap_or(false)
            }),
        ),
        Err(_) => (None, false, false),
    };
    let rebuilt_ok = rebuilt_depth == Some(1) && rebuilt_interrupted && replay_equal;

    let ok = !spawn.is_error
        && !child_id.is_empty()
        && tree_ok
        && nested_rejected
        && loop_ok
        && outcome_interrupted
        && child_interrupted
        && parent_interrupted
        && rebuilt_ok;
    println!(
        "[tree] child={child_id} depth={depth} tree_ok={tree_ok} nested_rejected={nested_rejected} \
         loop_ok={loop_ok} interrupted={outcome_interrupted}/{child_interrupted}/{parent_interrupted} \
         rebuilt_ok={rebuilt_ok} parent_created={parent_created} spawned={parent_spawned} \
         finished={parent_finished} link={child_parent_link} reply={child_has_reply} \
         order={:?}",
        order
            .iter()
            .map(|(id, level)| format!("{id}:{level}"))
            .collect::<Vec<_>>()
    );
    if ok {
        println!("[selftest-tree] PASS");
        0
    } else {
        println!("[selftest-tree] FAIL");
        1
    }
}

/// 导出/保留自检：JSONL + 外部化附件 + manifest sha256，导出后可自校验；
/// CLI 入口 `--export-session` 也走同一路径。
fn selftest_export() -> i32 {
    let root = PathBuf::from(".tmp/export-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let sessions = root.join("sessions");
    let _ = std::fs::create_dir_all(&sessions);
    std::env::set_var("DSHR_SESSIONS_DIR", &sessions);
    std::env::set_var("DSHR_SPILL_DIR", root.join("spill"));
    std::env::set_var("DSHR_CONTEXT_WINDOW", "200000");

    let cancel = Arc::new(AtomicBool::new(false));
    let path = sessions.join("s-export.jsonl");
    let mut state = ChatState::new(path.clone(), "s-export", &cancel, ApprovalBroker::new());
    let big = format!("{}导出附件标记", "外部化内容".repeat(6_000));
    let _ = state.log.append(EventSpec::durable(
        "tool.call",
        "t0",
        json!({ "call_id": "ex-c1", "tool": "probe_echo", "arguments": {} }),
    ));
    let _ = state.log.append(EventSpec::durable(
        "tool.result",
        "t0",
        json!({
            "call_id": "ex-c1", "is_error": false,
            "content": [{ "type": "text", "text": big }]
        }),
    ));
    state.save();
    let _ = state.plan_with_spill();

    let out = root.join("out");
    let manifest = match export_session(
        &state.log,
        &SpillStoreSource::new(&state.spill),
        &out,
        &ExportOptions::new(now_ts()),
    ) {
        Ok(manifest) => manifest,
        Err(err) => {
            println!("[selftest-export] FAIL: 导出失败 {err}");
            return 1;
        }
    };
    let verified = match verify_export(&out, "s-export") {
        Ok(report) => report,
        Err(err) => {
            println!("[selftest-export] FAIL: 自校验失败 {err}");
            return 1;
        }
    };
    let exported_jsonl = out.join(&manifest.jsonl.path);
    let reloaded = std::fs::read_to_string(&exported_jsonl)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-export", &text).ok());
    let reload_equal = reloaded
        .as_ref()
        .map(|log| log.len() == state.log.len())
        .unwrap_or(false);
    // CLI 入口同样可复现（导出 + 自校验）。
    let cli_out = root.join("out-cli");
    let cli_ok = export_session_cli("s-export", Some(cli_out)) == 0;

    let ok = manifest.event_count == state.log.len()
        && manifest.attachments.len() == 1
        && manifest.missing_blobs.is_empty()
        && verified.is_ok()
        && verified.attachments_checked == 1
        && reload_equal
        && cli_ok
        && manifest.jsonl.size > 0
        && !manifest.jsonl.sha256.is_empty();
    println!(
        "[export] events={} attachments={} missing={} bytes={} verify={} reload_equal={reload_equal} cli={cli_ok}",
        manifest.event_count,
        manifest.attachments.len(),
        manifest.missing_blobs.len(),
        manifest.total_bytes,
        verified.is_ok()
    );
    // 保留计划：目录里只有本会话、未超限 → 不需要删除。
    let plan_code = retention_plan_cli();
    let ok = ok && plan_code == 0;
    std::env::remove_var("DSHR_SPILL_DIR");
    std::env::remove_var("DSHR_CONTEXT_WINDOW");
    if ok {
        println!("[selftest-export] PASS");
        0
    } else {
        println!("[selftest-export] FAIL");
        1
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
/// 重试/故障注入自检：每第 3 次请求返回 503，20 轮工具调用全部应成功。
struct FlakyProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    fail_every: usize,
}

impl ModelProvider for FlakyProvider {
    fn complete(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<dshr_agent_core::ModelReply, String> {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if self.fail_every > 0 && call.is_multiple_of(self.fail_every) {
            return Err(String::from("HTTP 503 temporarily unavailable"));
        }
        // 第二轮（已看到工具结果）返回文本收尾；第一轮返回一次工具调用。
        let saw_tool_result = matches!(request.messages.last(), Some(ChatMessage::Tool { .. }));
        if saw_tool_result {
            Ok(dshr_agent_core::ModelReply {
                text: format!("ok-{call}"),
                tool_calls: Vec::new(),
            })
        } else {
            Ok(dshr_agent_core::ModelReply {
                text: String::new(),
                tool_calls: vec![dshr_agent_core::ModelToolCall {
                    id: format!("call-{call}"),
                    name: String::from("probe_native"),
                    arguments: json!({ "text": "ping" }),
                }],
            })
        }
    }
}

struct CountingExecutor {
    executed: usize,
}

impl ToolExecutor for CountingExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: String::from("probe_native"),
            description: String::from("自检用"),
            parameters: json!({ "type": "object" }),
        }]
    }

    fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
        self.executed += 1;
        ToolOutcome::success(
            json!([{ "type": "text", "text": "ok" }]),
            json!({ "ok": true }),
        )
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
    let mut executor = CountingExecutor { executed: 0 };
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    for _ in 0..20 {
        let events = agent.run_turn("继续", &mut executor);
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
        "[retry] turns=20 succeeded={succeeded} failed={failed} tool_executions={} \
         provider_calls={total_calls} (fault every 3rd call, max attempts 4)",
        executor.executed
    );
    let ok = failed == 0 && succeeded == 20 && executor.executed == 20 && total_calls > 40;
    if ok {
        println!("[selftest-retry] PASS");
        0
    } else {
        println!("[selftest-retry] FAIL");
        1
    }
}

/// 10k 事件性能基线：冷加载 + 回放 + 快照 + 历史重建（M2 gate 2 的数据层）。
/// 生成 10k 事件会话（2500 轮 user/assistant/tool.call/tool.result），供性能自检与帧探针复用。
fn build_perf_log(session_id: &str) -> SessionLog {
    let mut log = SessionLog::new(session_id);
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
    log
}

/// 把 10k 事件会话写进会话目录，返回会话 id（供窗口帧探针/截图复用）。
fn seed_perf_session(dir: &std::path::Path) -> i32 {
    let store = SessionIndex::new(dir);
    if let Err(err) = store.ensure_dir() {
        eprintln!("[seed] 无法创建会话目录: {err}");
        return 1;
    }
    let id = store.new_id();
    if let Err(err) = store.create(&id) {
        eprintln!("[seed] 无法创建会话: {err}");
        return 1;
    }
    let log = build_perf_log(&id);
    let jsonl = log.to_jsonl();
    let path = store.path_for(&id);
    if let Err(err) = std::fs::write(&path, jsonl.as_bytes()) {
        eprintln!("[seed] 写入会话失败: {err}");
        return 1;
    }
    println!(
        "[seed] id={id} events={} bytes={} path={}",
        log.len(),
        jsonl.len(),
        path.display()
    );
    0
}

fn selftest_perf() -> i32 {
    let log = build_perf_log("s-perf");
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
    reset_session_file(&path);
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

/// Gate 1 真实网络自检：真实 provider 连跑 20 轮，每轮要求模型调用一次本地工具。
///
/// 统计口径：以"每轮一次模型请求 + 至少一次真实工具调用"为一步；一步成功 =
/// 有回复、无失败标记、`tool.call`/`tool.result` 配对且无错误/中止。
/// 需要 `DEEPSEEK_API_KEY` 或 `OPENAI_API_KEY`；两者都没有时返回 2（SKIP，不伪装通过）。
fn selftest_live_20() -> i32 {
    if DeepSeekProvider::from_env().is_none() && OpenAiProvider::from_env().is_none() {
        println!("[selftest-live-20] SKIP: 没有 DEEPSEEK_API_KEY / OPENAI_API_KEY");
        return 2;
    }
    let path = PathBuf::from(".tmp/chat-live-20.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    // 这一步只统计"网络 + 模型 + 工具执行"链路；审批链由 --selftest-approval 覆盖。
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let mut state = ChatState::new(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    let count = |state: &ChatState, kind: &str| {
        state
            .log
            .events()
            .iter()
            .filter(|event| event.event_type == kind)
            .count()
    };
    let count_errors = |state: &ChatState| {
        state
            .log
            .events()
            .iter()
            .filter(|event| {
                event.event_type == "tool.result"
                    && event
                        .payload
                        .get("is_error")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
            })
            .count()
    };

    let steps = 20usize;
    let mut ok_steps = 0usize;
    let mut failed_steps = 0usize;
    let mut total_calls = 0usize;
    let mut total_results = 0usize;
    let mut total_errors = 0usize;
    let mut latencies = Vec::new();

    for step in 0..steps {
        let prompt = format!(
            "第 {step} 步：只调用一次 probe_native 工具，参数 text 为 \"step-{step}\"，\
             不要调用其它工具；拿到工具结果后用一句中文确认。"
        );
        let before_calls = count(&state, "tool.call");
        let before_results = count(&state, "tool.result");
        let before_errors = count_errors(&state);
        let started = std::time::Instant::now();
        let reply = state.stream_turn(&prompt, &mut |_| {});
        let elapsed_ms = started.elapsed().as_millis() as u64;
        latencies.push(elapsed_ms);

        let calls = count(&state, "tool.call") - before_calls;
        let results = count(&state, "tool.result") - before_results;
        let errors = count_errors(&state) - before_errors;
        let aborted = count(&state, "tool.aborted");
        total_calls += calls;
        total_results += results;
        total_errors += errors;

        let reply_ok = reply
            .as_deref()
            .is_some_and(|text| !text.is_empty() && !text.starts_with("处理失败"));
        let step_ok = reply_ok && calls >= 1 && results >= 1 && errors == 0 && aborted == 0;
        if step_ok {
            ok_steps += 1;
        } else {
            failed_steps += 1;
        }
        println!(
            "[live-20] step={step} ok={step_ok} calls={calls} results={results} \
             errors={errors} ms={elapsed_ms} reply={}",
            reply.unwrap_or_default()
        );
    }

    latencies.sort_unstable();
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[(latencies.len() * 95 / 100).min(latencies.len() - 1)];
    let rate = ok_steps as f64 / steps as f64;
    let provider = std::env::var("DSHR_PROVIDER").unwrap_or_else(|_| String::from("auto"));
    println!(
        "[live-20] provider={provider} steps={steps} ok={ok_steps} failed={failed_steps} \
         rate={rate:.4} tool_calls={total_calls} tool_results={total_results} \
         tool_errors={total_errors} p50_ms={p50} p95_ms={p95}"
    );
    std::env::remove_var("DSHR_APPROVAL_MODE");
    if rate >= 0.99 {
        println!("[selftest-live-20] PASS");
        0
    } else {
        println!("[selftest-live-20] FAIL");
        1
    }
}

/// `--selftest-l2`：v2 Host Service 的 CLI 层 smoke（不依赖真实 DSH 插件执行）。
///
/// Phase A（可选，需要 vendored 官方 sidecar）：真握手到 v2，断言
/// `negotiated_protocol == 2`、`host_services_active`、白名单 3 张表。
/// Phase B（始终）：用 compat-dsh-host 的 fixture sidecar 跑
/// `storage-kv/v1` open→putRecord→loadAll 的真实 `host.service.request` 往返。
fn selftest_l2() -> i32 {
    let mut pass = true;

    // Phase A：真实官方 sidecar（无 vendored 依赖时 SKIP，不算失败）。
    let dsh_dir = app_root().join("sidecar/dsh-official");
    if dsh_dir.join("sidecar-dsh.mjs").exists() && dsh_dir.join("node_modules").exists() {
        let l2 = ChatSessionAccess::new(sessions_dir_from_env());
        let env_keys = [
            "DSHR_SIDECAR",
            "DSHR_DSH_PLUGINS",
            "DSHR_HOST_SERVICES",
            "DSHR_FS_WRITE",
        ];
        let saved: Vec<Option<std::ffi::OsString>> =
            env_keys.iter().map(std::env::var_os).collect();
        std::env::set_var("DSHR_SIDECAR", "dsh");
        std::env::set_var("DSHR_DSH_PLUGINS", "[]");
        std::env::set_var("DSHR_HOST_SERVICES", "1");
        std::env::set_var("DSHR_FS_WRITE", "0");
        let host = spawn_sidecar(None, Some(l2));
        for (key, value) in env_keys.iter().zip(saved) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        match host {
            None => println!("[selftest-l2] SKIP phase A: 官方 sidecar 启动失败（环境受限）"),
            Some(host) => {
                let protocol = host.negotiated_protocol();
                let active = host.host_services_active();
                let tables = host
                    .host_services_payload()
                    .and_then(|payload| payload.as_array().map(Vec::len))
                    .unwrap_or(0);
                drop(host);
                if protocol == 2 && active && tables == 3 {
                    println!(
                        "[selftest-l2] phase A PASS protocol={protocol} active={active} host_services={tables}"
                    );
                } else {
                    eprintln!(
                        "[selftest-l2] phase A FAIL protocol={protocol} active={active} host_services={tables}"
                    );
                    pass = false;
                }
            }
        }
    } else {
        println!(
            "[selftest-l2] SKIP phase A: 未找到 vendored 官方 sidecar（先运行 node tools/dsh-vendor.mjs）"
        );
    }

    // Phase B：fixture sidecar 的真实 host.service.request 往返（storage-kv）。
    let fixture = std::path::absolute(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/compat-dsh-host/tests/fixtures/l2-sidecar.mjs"),
    )
    .unwrap_or_else(|_| PathBuf::from("crates/compat-dsh-host/tests/fixtures/l2-sidecar.mjs"));
    if !fixture.exists() {
        eprintln!(
            "[selftest-l2] FAIL phase B: fixture 不存在: {}",
            fixture.display()
        );
        return 1;
    }
    let root = data_root()
        .join("selftests")
        .join(format!("l2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(&root) {
        // review P2-4：不再用用户 `%TEMP%`，失败要打印真实路径与原因。
        eprintln!(
            "[selftest-l2] FAIL phase B: 创建临时目录 {} 失败: {error}",
            root.display()
        );
        return 1;
    }
    let access = ChatSessionAccess::new(root.join("sessions"));
    let router = match build_l2_router_at(
        access,
        root.join("storage"),
        root.join("workspace"),
        "l2.fixture",
    ) {
        Ok(router) => router,
        Err(err) => {
            eprintln!("[selftest-l2] FAIL phase B: {err}");
            return 1;
        }
    };
    let options = HostOptions::default()
        .with_env("L2_SCENARIO", "storage")
        .with_registration_window(Duration::from_secs(3))
        .with_host_services(router);
    match SidecarHost::spawn_with(&fixture, &fixture, options) {
        Err(err) => {
            eprintln!("[selftest-l2] FAIL phase B: spawn fixture: {err}");
            pass = false;
        }
        Ok(mut host) => {
            let protocol = host.negotiated_protocol();
            let active = host.host_services_active();
            match host.invoke("l2_probe", json!({}), Duration::from_secs(15)) {
                Ok(outcome) => {
                    let steps = &outcome.terminal.payload["value"]["steps"];
                    let open_ok = steps[0]["ok"].as_bool().unwrap_or(false);
                    let put_ok = steps[1]["ok"].as_bool().unwrap_or(false);
                    let load_n = steps[2]["result"]["tables"]["items"]["a"]["n"].as_i64();
                    let unit_id = steps[0]["result"]["unit_id"]
                        .as_str()
                        .unwrap_or("<none>")
                        .to_owned();
                    if protocol == 2 && active && open_ok && put_ok && load_n == Some(1) {
                        println!(
                            "[selftest-l2] phase B PASS protocol={protocol} active={active} unit_id={unit_id} roundtrip=ok"
                        );
                    } else {
                        eprintln!(
                            "[selftest-l2] phase B FAIL protocol={protocol} active={active} payload={}",
                            outcome.terminal.payload
                        );
                        pass = false;
                    }
                }
                Err(err) => {
                    eprintln!("[selftest-l2] FAIL phase B: invoke: {err}");
                    pass = false;
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    if pass {
        println!("[selftest-l2] PASS");
        0
    } else {
        println!("[selftest-l2] FAIL");
        1
    }
}

/// `--selftest-parallel-tools`：M3 并行工具的 CLI 层证据（串行 vs 并行墙钟 + 顺序）。
///
/// 用 probe sidecar 的 `probe_slow`（每个 ≈2s）跑 3 个：串行 ≈6s、并行 ≈2s；
/// 另用 `probe_echo` 验证 `execute_batch` 的结果按原顺序返回。取消/超时/迟到语义
/// 由 `compat-dsh-host` 的 `M3-PARALLEL-101..107` 覆盖。
fn selftest_parallel_tools() -> i32 {
    let path = PathBuf::from(".tmp/chat-parallel-tools.jsonl");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open(
        path,
        "s-parallel",
        &cancel,
        ApprovalBroker::new(),
        false,
        true,
    );
    if state.host.is_none() {
        println!(
            "[selftest-parallel-tools] SKIP: sidecar unavailable（找不到 node/probe sidecar）"
        );
        return 0;
    }
    let log_cell = std::rc::Rc::new(std::cell::RefCell::new(std::mem::replace(
        &mut state.log,
        SessionLog::new("s-parallel"),
    )));
    let mut executor = ChatExecutor {
        plugins: &mut state.plugins,
        host: state.host.as_mut(),
        approvals: &state.approvals,
        log: log_cell.clone(),
        sessions_dir: &state.sessions_dir,
        parent_id: "s-parallel",
        cancel: &cancel,
        allow_spawn: false,
        l2_access: state.l2_access.clone(),
    };

    // 1) 顺序证据：3 个 probe_echo（fast），并行批返回顺序必须等于调用顺序。
    let order_calls: Vec<ToolCallRequest> = (0..3)
        .map(|index| ToolCallRequest {
            call_id: format!("parallel-order-{index}"),
            tool: "probe_echo".to_owned(),
            arguments: json!({ "i": index }),
        })
        .collect();
    let order_results = executor.execute_batch(&order_calls, 3);
    let order_ok = order_results.iter().enumerate().all(|(index, outcome)| {
        !outcome.is_error && outcome.value["echo"]["i"].as_u64() == Some(index as u64)
    });

    // 2) 墙钟证据：3 个 probe_slow（每个 ≈2s）先串行、再并行。
    let slow_calls: Vec<ToolCallRequest> = (0..3)
        .map(|index| ToolCallRequest {
            call_id: format!("parallel-slow-{index}"),
            tool: "probe_slow".to_owned(),
            arguments: json!({}),
        })
        .collect();
    let serial_started = std::time::Instant::now();
    let serial_results: Vec<ToolOutcome> = slow_calls
        .iter()
        .map(|call| executor.execute(call))
        .collect();
    let serial_elapsed = serial_started.elapsed();
    let parallel_started = std::time::Instant::now();
    let parallel_results = executor.execute_batch(&slow_calls, 3);
    let parallel_elapsed = parallel_started.elapsed();

    drop(executor);
    state.log = std::rc::Rc::try_unwrap(log_cell)
        .map(|cell| cell.into_inner())
        .unwrap_or_else(|cell| cell.borrow().clone());
    state.save();

    let all_ok = serial_results
        .iter()
        .chain(parallel_results.iter())
        .all(|outcome| !outcome.is_error && outcome.value["done"] == true);
    let ratio = if serial_elapsed.is_zero() {
        f64::INFINITY
    } else {
        parallel_elapsed.as_secs_f64() / serial_elapsed.as_secs_f64()
    };
    println!(
        "[selftest-parallel-tools] order_ok={order_ok} all_ok={all_ok} serial={serial_elapsed:?} parallel={parallel_elapsed:?} ratio={ratio:.2}"
    );
    if order_ok && all_ok && ratio < 0.6 {
        println!("[selftest-parallel-tools] PASS");
        0
    } else {
        println!("[selftest-parallel-tools] FAIL（并行加速比应 < 0.6）");
        1
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // 1. 先把命令行用法的输出来路接好（release GUI 子系统没有控制台）。
    prepare_cli_console(is_cli_invocation(&args));
    // 2. 数据根必须在崩溃 hook 之前解析：hook 通过 DSHR_CRASH_LOG 写日志。
    prepare_data_env();
    install_crash_hook();
    // 3. 自检的 `.tmp` 相对路径与用户 CWD 解耦（只读 CWD 下也能跑）。
    prepare_selftest_cwd(&args);
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

    if std::env::args().any(|arg| arg == "--selftest-live-20") {
        std::process::exit(selftest_live_20());
    }
    if std::env::args().any(|arg| arg == "--selftest-stream") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_stream());
    }
    if std::env::args().any(|arg| arg == "--selftest-context") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_context());
    }
    if std::env::args().any(|arg| arg == "--selftest-long-context") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_long_context());
    }
    if std::env::args().any(|arg| arg == "--selftest-spill") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_spill());
    }
    if std::env::args().any(|arg| arg == "--selftest-approval") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_approval());
    }
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|arg| arg == "--selftest-crash-child") {
        let path = args.get(index + 1).cloned().unwrap_or_default();
        std::process::exit(selftest_crash_child(&path));
    }
    if args.iter().any(|arg| arg == "--selftest-crash-recovery") {
        std::process::exit(selftest_crash_recovery());
    }
    if args.iter().any(|arg| arg == "--selftest-corrupt-session") {
        std::process::exit(selftest_corrupt_session());
    }
    if std::env::args().any(|arg| arg == "--selftest-recovery") {
        std::process::exit(selftest_recovery());
    }
    if std::env::args().any(|arg| arg == "--selftest-sidecar-recovery") {
        std::process::exit(selftest_sidecar_recovery());
    }
    if std::env::args().any(|arg| arg == "--selftest-cancel") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_cancel());
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
    if let Some(index) = std::env::args().position(|arg| arg == "--seed-perf-session") {
        let dir = std::env::args()
            .nth(index + 1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".tmp/sessions"));
        std::process::exit(seed_perf_session(&dir));
    }
    if std::env::args().any(|arg| arg == "--selftest-trust-install") {
        std::process::exit(selftest_trust_install());
    }
    if std::env::args().any(|arg| arg == "--selftest-plugin-lifecycle") {
        std::process::exit(selftest_plugin_lifecycle());
    }
    if std::env::args().any(|arg| arg == "--selftest-external-plugin") {
        std::process::exit(selftest_external_plugin());
    }
    if std::env::args().any(|arg| arg == "--selftest-wasm") {
        std::process::exit(selftest_wasm());
    }
    if std::env::args().any(|arg| arg == "--selftest-adc-panel") {
        std::process::exit(selftest_adc_panel());
    }
    if std::env::args().any(|arg| arg == "--selftest-defaults") {
        std::process::exit(selftest_defaults());
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--install-plugin") {
        let Some(path) = std::env::args().nth(index + 1) else {
            eprintln!(
                "用法: chat --install-plugin <本地包目录> [--yes-install] [--allow-downgrade]"
            );
            std::process::exit(2);
        };
        std::process::exit(install_plugin_cli(PathBuf::from(path)));
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--uninstall-plugin") {
        let Some(name) = std::env::args().nth(index + 1) else {
            eprintln!("用法: chat --uninstall-plugin <插件名>");
            std::process::exit(2);
        };
        std::process::exit(uninstall_plugin_cli(name));
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--trust-key") {
        let rest: Vec<String> = std::env::args().skip(index + 1).collect();
        std::process::exit(trust_key_cli(&rest));
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--export-session") {
        let Some(id) = std::env::args().nth(index + 1) else {
            eprintln!("用法: chat --export-session <会话 id> [--export-dir <目录>]");
            std::process::exit(2);
        };
        let out = std::env::args()
            .position(|arg| arg == "--export-dir")
            .and_then(|dir_index| std::env::args().nth(dir_index + 1))
            .map(PathBuf::from);
        std::process::exit(export_session_cli(&id, out));
    }
    if std::env::args().any(|arg| arg == "--retention-plan") {
        std::process::exit(retention_plan_cli());
    }
    if std::env::args().any(|arg| arg == "--selftest-tree") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_tree());
    }
    if std::env::args().any(|arg| arg == "--selftest-export") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_export());
    }
    if std::env::args().any(|arg| arg == "--selftest-dsh-write") {
        std::process::exit(selftest_dsh_write());
    }
    if std::env::args().any(|arg| arg == "--selftest-dsh") {
        std::env::set_var("DSHR_SIDECAR", "dsh");
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_dsh());
    }
    if std::env::args().any(|arg| arg == "--selftest-l2") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_l2());
    }
    if std::env::args().any(|arg| arg == "--selftest-parallel-tools") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_parallel_tools());
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
    // 帧探针：必须在第一帧之前开启 profiler trace，否则 FrameTiming 不会被记录。
    let frame_probe = FrameProbe::from_env();
    if frame_probe.is_some() {
        set_trace_enabled(true);
    }
    // 行窗口上限：默认不限制（虚拟化负责渲染成本），仅压力/对比实验用
    // DSHR_VISIBLE_LIMIT 保留"只看尾部 N 行"的旧行为。
    let visible_limit = std::env::var("DSHR_VISIBLE_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
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

    let (mut sessions, mut session_depths) = ordered_sessions(&store);
    if sessions.is_empty() {
        let id = store.new_id();
        if let Err(err) = store.create(&id) {
            eprintln!("[chat] 无法创建初始会话: {err}");
            std::process::exit(1);
        }
        let rebuilt = ordered_sessions(&store);
        sessions = rebuilt.0;
        session_depths = rebuilt.1;
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
    // 已安装的 external-process 插件：按 profile 的 enabled 条目在启动时装载；
    // 单个插件失败只进 plugin_errors，不阻塞窗口。
    {
        let mut guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let external_errors =
            load_enabled_external_plugins(&mut guard.plugins, &profile, &sidecar_dir_for_app());
        guard.plugin_errors.extend(external_errors);
        // external-process 面板也在这里取一次树（之后渲染只读缓存）。
        guard.refresh_adc_panels();
    }
    let (rows, plugin_errors, adc_panels) = {
        let guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (
            rows_snapshot(&guard.log, None),
            guard.plugin_errors.clone(),
            guard.adc_panels.clone(),
        )
    };

    application().run(move |cx: &mut App| {
        // M4 W2：全局键盘动作（cmd 在 Windows 映射 Ctrl，在 macOS 映射 Cmd）。
        cx.bind_keys(chat_key_bindings());
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
        let session_depths = session_depths;
        let plugin_errors = plugin_errors;
        let adc_panels = adc_panels;
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
                    pending_external: None,
                    adc_editor: None,
                    visible_limit,
                    frame_probe,
                    session_depths,
                    plugin_errors,
                    adc_panels,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn args(values: &[&str]) -> Vec<String> {
        // 第 0 个元素模拟 exe 路径，与 `std::env::args()` 一致。
        std::iter::once("chat.exe")
            .chain(values.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn gui_launch_is_not_cli() {
        assert!(!is_cli_invocation(&args(&[])));
    }

    #[test]
    fn cli_switches_attach_console() {
        for flag in [
            "--selftest",
            "--selftest-stream",
            "--selftest-dsh",
            "--selftest-l2",
            "--selftest-parallel-tools",
            "--selftest-dsh-write",
            "--selftest-approval",
            "--selftest-corrupt-session",
            "--selftest-crash-recovery",
            "--selftest-crash-child",
            "--selftest-recovery",
            "--selftest-sidecar-recovery",
            "--selftest-cancel",
            "--live",
            "--live-chat",
            "--export-session",
            "--retention-plan",
            "--install-plugin",
            "--uninstall-plugin",
            "--trust-key",
            "--selftest-plugin-lifecycle",
            "--seed-perf-session",
            "--console",
        ] {
            assert!(
                is_cli_invocation(&args(&[flag])),
                "{flag} 应识别为 CLI 用法"
            );
        }
    }

    // M4-A11Y-001：快捷键表与绑定数量一致、覆盖 G3 主流程键位、提示文案含平台主修饰键。
    #[test]
    fn keyboard_shortcut_table_is_unique_and_covers_primary_flow() {
        use std::collections::HashSet;

        let keys: HashSet<&str> = CHAT_SHORTCUTS.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys.len(), CHAT_SHORTCUTS.len(), "快捷键不能重复");
        for required in [
            "cmd-n",
            "cmd-k",
            "cmd-b",
            "cmd-j",
            "cmd-shift-a",
            "cmd-shift-d",
            "escape",
            "cmd-w",
        ] {
            assert!(keys.contains(required), "缺少快捷键 {required}");
        }
        assert_eq!(
            chat_key_bindings().len(),
            CHAT_SHORTCUTS.len(),
            "绑定数量必须与说明表一致"
        );
        let hint = chat_shortcut_hint();
        assert!(hint.contains("Ctrl") || hint.contains("Cmd"), "{hint}");
        assert_eq!(format_shortcut("cmd-shift-a", "Ctrl"), "Ctrl+Shift+A");
        assert_eq!(format_shortcut("escape", "Ctrl"), "Esc");
    }

    #[test]
    fn dshr_home_wins_over_packaged_layout() {
        let home = Path::new("D:\\dshr-home");
        let picked = pick_data_root(
            Some(home),
            Some(Path::new("C:\\app")),
            Some(Path::new("C:\\Users\\me\\AppData\\Local")),
            true,
        );
        assert_eq!(picked, Some((DataRootSource::Home, home.to_path_buf())));
    }

    #[test]
    fn packaged_layout_prefers_exe_dir() {
        let exe = Path::new("C:\\app");
        let picked = pick_data_root(
            None,
            Some(exe),
            Some(Path::new("C:\\Users\\me\\AppData\\Local")),
            true,
        );
        assert_eq!(picked, Some((DataRootSource::ExeDir, exe.to_path_buf())));
    }

    #[test]
    fn dev_layout_uses_local_app_data() {
        // 从 target/release 跑（exe 旁没有 sidecar/）→ 数据落 %LOCALAPPDATA%\dshr。
        let local = Path::new("C:\\Users\\me\\AppData\\Local");
        let picked = pick_data_root(
            None,
            Some(Path::new("C:\\repo\\target\\release")),
            Some(local),
            false,
        );
        assert_eq!(
            picked,
            Some((DataRootSource::LocalAppData, local.join("dshr")))
        );
    }

    #[test]
    fn without_any_candidate_there_is_no_pick() {
        assert_eq!(pick_data_root(None, None, None, false), None);
        // 打包布局标记不可用（不可写）时也不能选 exe 目录。
        assert_eq!(
            pick_data_root(None, Some(Path::new("C:\\app")), None, false),
            None
        );
    }

    #[test]
    fn tool_timeout_is_clamped_to_sane_range() {
        assert_eq!(clamp_tool_timeout_ms(0), Duration::from_secs(1));
        assert_eq!(clamp_tool_timeout_ms(999), Duration::from_secs(1));
        assert_eq!(clamp_tool_timeout_ms(120_000), Duration::from_secs(120));
        assert_eq!(clamp_tool_timeout_ms(601_000), Duration::from_secs(600));
        assert_eq!(clamp_tool_timeout_ms(u64::MAX), Duration::from_secs(600));
    }

    #[test]
    fn sidecar_tool_timeout_follows_declaration_without_override() {
        if std::env::var_os("DSHR_TOOL_TIMEOUT_MS").is_some() {
            // 显式覆盖存在时以覆盖为准（clamp 后）。
            let definition = ToolDefinition::new(
                "x",
                "x",
                ToolOrigin::Native {
                    plugin_id: "p".into(),
                },
            );
            assert_eq!(
                sidecar_tool_timeout(&definition),
                clamp_tool_timeout_ms(
                    std::env::var("DSHR_TOOL_TIMEOUT_MS")
                        .expect("present")
                        .parse()
                        .expect("number")
                )
            );
            return;
        }
        let mut definition = ToolDefinition::new(
            "bash",
            "run shell",
            ToolOrigin::Native {
                plugin_id: "p".into(),
            },
        );
        definition.timeout_ms = 30_000;
        assert_eq!(sidecar_tool_timeout(&definition), Duration::from_secs(30));
        definition.timeout_ms = 120_000;
        assert_eq!(sidecar_tool_timeout(&definition), Duration::from_secs(120));
    }
}
