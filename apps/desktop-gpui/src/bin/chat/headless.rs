use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use dshr_profile::headless::{load, valid_name};
use dshr_profile::Profile;
use dshr_session_store::{save_atomic, try_lock_session, SessionCreated, SessionIndex, SessionLog};
use dshr_tool_runtime::{ApprovalBroker, PermissionMode};
use serde_json::{json, Value};

use super::{
    configured_models, data_root, non_empty_env_path, now_ts, prepare_data_env,
    profile_dir_for_app, sessions_dir_from_env, ChatRuntime, ChatState, DesktopSettings, ModelKind,
    TurnReason, TurnUpdate,
};

const HELP: &str = concat!(
    "Usage: dshr --profile <name> [--patch <path>]... [--json] [--session-id <id>] [--] [<task>...]\n",
    "\nRun one task without a window. Omit the task or pass - to read redirected stdin.\n\n",
    "  --patch <path>      Apply a static profile patch before the headless arguments\n",
    "  --json              Write newline-delimited run events\n",
    "  --session-id <id>   Continue an existing DSHR session in the same workspace\n",
    "  -h, --help          Show this help\n",
);

struct CliError {
    code: &'static str,
    message: String,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn usage(message: impl Into<String>) -> Self {
        Self::new("E_USAGE", message)
    }

    fn config(message: impl Into<String>) -> Self {
        Self::new("E_CONFIG", message)
    }

    fn session_io(error: impl std::fmt::Display) -> Self {
        Self::new("E_SESSION_IO", error.to_string())
    }
}

struct Args {
    name: String,
    patches: Vec<PathBuf>,
    task: Option<String>,
    force_stdin: bool,
    id: Option<String>,
    help: bool,
}

pub(super) fn selected(args: &[String]) -> bool {
    args.first()
        .is_some_and(|arg| arg == "--profile" || arg.starts_with("--profile="))
}

fn json_requested(args: &[String]) -> bool {
    let mut args = args.iter().map(String::as_str).peekable();
    while let Some(arg) = args.next() {
        match arg {
            "--" => return false,
            "--json" => return true,
            "--profile" | "--session-id" | "--patch" => {
                args.next_if(|value| !value.starts_with('-'));
            }
            _ => {}
        }
    }
    false
}

fn operand<'a>(
    args: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
    option: &str,
) -> Result<&'a str, CliError> {
    args.next_if(|arg| !arg.is_empty() && !arg.starts_with('-'))
        .ok_or_else(|| CliError::usage(format!("{option} 需要一个值")))
}

fn parse(args: &[String]) -> Result<Args, CliError> {
    let mut args = args.iter().map(String::as_str).peekable();
    let mode = args.next().unwrap_or("");
    let name = match mode.strip_prefix("--profile=") {
        Some(value) => value,
        None => operand(&mut args, "--profile")?,
    };
    if !valid_name(name) {
        return Err(CliError::usage("无效 profile 名称"));
    }
    let mut patches = Vec::new();
    while args.next_if_eq(&"--patch").is_some() {
        patches.push(PathBuf::from(operand(&mut args, "--patch")?));
    }
    let mut id = None;
    let mut json = false;
    let mut help = false;
    let mut task = Vec::new();
    while let Some(arg) = args.next() {
        match arg {
            "--" => {
                task.extend(args);
                break;
            }
            "--json" if !json => json = true,
            "--session-id" if id.is_none() => {
                let value = operand(&mut args, "--session-id")?;
                if !valid_name(value) {
                    return Err(CliError::usage("无效会话 ID"));
                }
                id = Some(value.to_owned());
            }
            "-h" | "--help" if !help => help = true,
            "-" => task.push(arg),
            option if option.starts_with('-') => {
                return Err(CliError::usage(format!("未知或重复选项 {option}")));
            }
            word => task.push(word),
        }
    }
    let force_stdin = task.as_slice() == ["-"];
    let task = match task.as_slice() {
        [] | ["-"] => None,
        words if words.contains(&"-") => return Err(CliError::usage("- 必须是唯一的任务参数")),
        words => {
            let task = words.join(" ");
            if task.trim().is_empty() {
                return Err(CliError::usage("任务不能为空"));
            }
            Some(task)
        }
    };
    Ok(Args {
        name: name.to_owned(),
        patches,
        task,
        force_stdin,
        id,
        help,
    })
}

fn read_task(task: Option<String>, force_stdin: bool) -> Result<String, CliError> {
    if let Some(task) = task {
        return Ok(task);
    }
    let stdin = io::stdin();
    if stdin.is_terminal() && !force_stdin {
        return Err(CliError::usage("请提供任务，或从 stdin 重定向输入"));
    }
    let mut text = String::new();
    stdin
        .lock()
        .read_to_string(&mut text)
        .map_err(|error| CliError::new("E_INPUT", format!("无法从 stdin 读取 UTF-8：{error}")))?;
    if text.trim().is_empty() {
        return Err(CliError::new("E_INPUT", "stdin 中没有任务"));
    }
    Ok(text)
}

fn settings() -> Result<DesktopSettings, CliError> {
    let permission = match std::env::var(PermissionMode::ENV).as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("workspace-write") => {
            PermissionMode::WorkspaceWrite
        }
        Ok("read-only") => PermissionMode::ReadOnly,
        _ => {
            return Err(CliError::config(
                "DSHR_PERMISSION_MODE 需为 read-only 或 workspace-write",
            ))
        }
    };
    let force_mock = std::env::var("DSHR_FORCE_MOCK").as_deref() == Ok("1");
    let model_kind = match std::env::var("DSHR_PROVIDER").as_deref() {
        Ok("mock") => Some(ModelKind::Mock),
        Ok("deepseek") => Some(ModelKind::DeepSeek),
        Ok("openai") => Some(ModelKind::OpenAi),
        Err(std::env::VarError::NotPresent) => force_mock.then_some(ModelKind::Mock),
        _ => {
            return Err(CliError::config(
                "DSHR_PROVIDER 需为 deepseek、openai 或 mock",
            ))
        }
    };
    if force_mock && model_kind != Some(ModelKind::Mock) {
        return Err(CliError::config("DSHR_FORCE_MOCK 与 DSHR_PROVIDER 冲突"));
    }
    let model = configured_models()
        .into_iter()
        .find(|model| {
            model_kind.map_or(model.kind() != ModelKind::Mock, |kind| model.kind() == kind)
        })
        .ok_or_else(|| CliError::config("找不到可用的模型凭据"))?;
    Ok(DesktopSettings {
        model,
        permission: Some(permission),
    })
}

fn prepare_plugins(args: &Args) -> Result<bool, CliError> {
    let mode = std::env::var("DSHR_SIDECAR").unwrap_or_default();
    if !matches!(mode.as_str(), "" | "probe" | "dsh" | "cordis") {
        return Err(CliError::config("DSHR_SIDECAR 需为 probe、dsh 或 cordis"));
    }
    let declared = load(&data_root(), &args.name, &args.patches).map_err(CliError::config)?;
    let loader_bundle = non_empty_env_path("DSHR_LOADER_BUNDLE");
    let loader_patch = non_empty_env_path("DSHR_LOADER_PATCH");
    let profile_dir = profile_dir_for_app();
    let plugins = if let Some(entries) = declared {
        if loader_bundle.is_some() || loader_patch.is_some() {
            return Err(CliError::config(
                "具名声明或补丁与 DSHR_LOADER_BUNDLE / DSHR_LOADER_PATCH 冲突",
            ));
        }
        let entries: Vec<_> = entries.into_iter().filter(|entry| entry.enabled).collect();
        serde_json::to_string(&entries).expect("serializable plugin entries")
    } else {
        let profile = match loader_bundle {
            Some(bundle) => {
                let patches: Vec<_> = loader_patch.into_iter().collect();
                Profile::from_loader(&profile_dir, &bundle, &patches).map_err(CliError::config)?
            }
            None if loader_patch.is_some() => {
                return Err(CliError::config(
                    "DSHR_LOADER_PATCH 需要 DSHR_LOADER_BUNDLE",
                ))
            }
            None => Profile::load(&profile_dir).map_err(CliError::config)?,
        };
        profile.sidecar_json()
    };
    std::env::set_var("DSHR_DSH_PLUGINS", plugins);
    Ok(!mode.is_empty())
}

struct Session {
    path: PathBuf,
    log: SessionLog,
    _writer: File,
    needs_save: bool,
}

fn read_log(path: &Path, id: &str) -> Result<Option<SessionLog>, CliError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(CliError::session_io(format!(
                "会话日志不是普通文件：{}",
                path.display()
            )))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(CliError::session_io(error)),
    }
    let text = fs::read_to_string(path).map_err(CliError::session_io)?;
    SessionLog::from_jsonl(id, &text)
        .map(Some)
        .map_err(CliError::session_io)
}

fn open_session(id: Option<&str>, workspace: &Path) -> Result<Session, CliError> {
    let store = SessionIndex::new(sessions_dir_from_env());
    store.ensure_dir().map_err(CliError::session_io)?;
    if let Some(id) = id {
        let path = store.path_for(id);
        let writer = try_lock_session(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                CliError::new("E_SESSION_BUSY", format!("会话 {id} 正被写入"))
            } else {
                CliError::session_io(error)
            }
        })?;
        let primary = read_log(&path, id)?;
        let needs_save = primary.is_none();
        let log = match primary {
            Some(log) => log,
            None => read_log(&path.with_extension("jsonl.bak"), id)?
                .ok_or_else(|| CliError::new("E_SESSION_NOT_FOUND", format!("找不到会话 {id}")))?,
        };
        let previous = log
            .created()
            .map(|created| Path::new(&created.workspace_id));
        let canonical_matches = previous
            .filter(|path| path.is_absolute())
            .and_then(|path| fs::canonicalize(path).ok())
            .zip(fs::canonicalize(workspace).ok())
            .is_some_and(|(previous, current)| previous == current);
        if !canonical_matches {
            return Err(CliError::new(
                "E_SESSION_WORKSPACE",
                format!("会话 {id} 的工作区与当前工作区不匹配"),
            ));
        }
        return Ok(Session {
            path,
            log,
            _writer: writer,
            needs_save,
        });
    }
    loop {
        let id = store.new_id();
        let path = store.path_for(&id);
        let writer = match try_lock_session(&path) {
            Ok(writer) => writer,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(CliError::session_io(error)),
        };
        if path.try_exists().map_err(CliError::session_io)?
            || path
                .with_extension("jsonl.bak")
                .try_exists()
                .map_err(CliError::session_io)?
        {
            continue;
        }
        let mut log = SessionLog::new(id);
        log.append_created(now_ts(), &SessionCreated::root(workspace.to_string_lossy()))
            .map_err(CliError::session_io)?;
        return Ok(Session {
            path,
            log,
            _writer: writer,
            needs_save: true,
        });
    }
}

static CANCEL: OnceLock<Arc<AtomicBool>> = OnceLock::new();

fn request_cancel() {
    if let Some(cancel) = CANCEL.get() {
        cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(windows)]
unsafe extern "system" fn handle_ctrl(event: u32) -> i32 {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT) {
        request_cancel();
        return 1;
    }
    0
}

#[cfg(unix)]
extern "C" fn handle_signal(_: libc::c_int) {
    request_cancel();
}

fn install_cancel(cancel: &Arc<AtomicBool>) -> io::Result<()> {
    let _ = CANCEL.set(cancel.clone());
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        use windows_sys::Win32::System::Threading::{OpenEventW, WaitForSingleObject, INFINITE};

        unsafe { SetConsoleCtrlHandler(None, 0) };
        if unsafe { SetConsoleCtrlHandler(Some(handle_ctrl), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if let Some(name) = std::env::var_os("DSHR_CONSOLE_CANCEL_EVENT") {
            let name: Vec<_> = name.encode_wide().chain([0]).collect();
            let raw = unsafe { OpenEventW(SYNCHRONIZE, 0, name.as_ptr()) };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            let event = unsafe { OwnedHandle::from_raw_handle(raw) };
            std::thread::Builder::new().spawn(move || {
                if unsafe { WaitForSingleObject(event.as_raw_handle(), INFINITE) } == WAIT_OBJECT_0
                {
                    request_cancel();
                }
            })?;
        }
    }
    #[cfg(unix)]
    for signal in [libc::SIGINT, libc::SIGTERM] {
        if unsafe { libc::signal(signal, handle_signal as *const () as libc::sighandler_t) }
            == libc::SIG_ERR
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn write_event(out: &mut impl Write, event: Value) -> io::Result<()> {
    serde_json::to_writer(&mut *out, &event)?;
    out.write_all(b"\n")?;
    out.flush()
}

fn output_error(error: io::Error) -> CliError {
    CliError::new("E_RUNTIME", format!("无法写入 stdout：{error}"))
}

fn run(
    mut args: Args,
    json: bool,
    out: &mut impl Write,
    emitted_session: &mut Option<String>,
) -> Result<i32, CliError> {
    if args.help {
        out.write_all(HELP.as_bytes())
            .and_then(|()| out.flush())
            .map_err(output_error)?;
        return Ok(0);
    }
    let text = read_task(args.task.take(), args.force_stdin)?;
    let settings = settings()?;
    let workspace = non_empty_env_path("DSHR_WORKSPACE")
        .map_or_else(std::env::current_dir, std::path::absolute)
        .map_err(|error| CliError::config(format!("无法解析工作区：{error}")))?;
    if !workspace.is_dir() || workspace.to_str().is_none() {
        return Err(CliError::config(format!(
            "无效工作区：{}",
            workspace.display()
        )));
    }
    prepare_data_env();
    let sidecar_required = prepare_plugins(&args)?;
    let cancel = Arc::new(AtomicBool::new(false));
    install_cancel(&cancel)
        .map_err(|error| CliError::new("E_RUNTIME", format!("无法注册取消信号：{error}")))?;
    let Session {
        path,
        log,
        needs_save,
        _writer,
    } = open_session(args.id.as_deref(), &workspace)?;
    let id = log.session_id().to_owned();
    let mut state = ChatState::from_log(
        path,
        log,
        workspace,
        &cancel,
        ApprovalBroker::noninteractive(),
        settings,
        ChatRuntime::Headless,
    );
    if sidecar_required {
        match &state.host {
            Some(host) if host.boot_errors().is_empty() => {}
            Some(_) => {
                return Err(CliError::new(
                    "E_RUNTIME",
                    "显式配置的 sidecar 插件加载失败",
                ))
            }
            None => return Err(CliError::new("E_RUNTIME", "显式配置的 sidecar 启动失败")),
        }
    }
    if needs_save {
        save_atomic(&state.path, state.log.to_jsonl().as_bytes()).map_err(CliError::session_io)?;
    }
    if json {
        write_event(out, json!({ "type": "session", "sessionId": id })).map_err(output_error)?;
        *emitted_session = Some(id.clone());
    }
    let mut output_failure = None;
    let result = state.run_turn(&text, &[], &mut |update| {
        if !json || output_failure.is_some() {
            return;
        }
        let event = match update {
            TurnUpdate::Call { seq, tool } => {
                json!({ "type": "tool_call", "sessionId": id, "callSeq": seq, "tool": tool })
            }
            TurnUpdate::Result { seq, status } => {
                json!({ "type": "tool_result", "sessionId": id, "callSeq": seq, "status": status })
            }
            TurnUpdate::Preview(..) => return,
        };
        if let Err(error) = write_event(out, event) {
            output_failure = Some(error);
            cancel.store(true, Ordering::Relaxed);
        }
    });
    if let Some(error) = output_failure {
        return Err(output_error(error));
    }
    let result = result.map_err(CliError::session_io)?;
    let completed = result.reason == TurnReason::Completed;
    if json {
        write_event(
            out,
            json!({
                "type": "final", "sessionId": id,
                "text": if completed { &result.reply } else { "" },
                "reason": { "kind": result.reason.as_str() },
            }),
        )
        .map_err(output_error)?;
    } else if completed && !result.reply.is_empty() {
        out.write_all(result.reply.as_bytes())
            .map_err(output_error)?;
        if !result.reply.ends_with('\n') {
            out.write_all(b"\n").map_err(output_error)?;
        }
        out.flush().map_err(output_error)?;
    }
    match result.reason {
        TurnReason::Completed => Ok(0),
        TurnReason::Error => {
            eprintln!("dshr: 模型轮次失败");
            Ok(1)
        }
        TurnReason::Cancelled => {
            eprintln!("dshr: 任务已取消");
            Ok(1)
        }
    }
}

pub(super) fn main(args: &[String]) -> i32 {
    let json = json_requested(args);
    let mut out = io::stdout().lock();
    let mut session = None;
    let result = parse(args).and_then(|parsed| run(parsed, json, &mut out, &mut session));
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("dshr: {}: {}", error.code, error.message);
            if json {
                let mut record =
                    json!({ "type": "error", "code": error.code, "message": error.message });
                if let Some(id) = session {
                    record["sessionId"] = json!(id);
                }
                let _ = write_event(&mut out, record);
            }
            1
        }
    }
}
