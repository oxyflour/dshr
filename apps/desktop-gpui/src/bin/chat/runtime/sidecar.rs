use crate::{
    app_root, creator, data_root, dsh_goals, dsh_image_attachments, dsh_schedules,
    dsh_session_query, dsh_subagents, dsh_teams, dsh_user_questions, is_writable_dir,
    non_empty_env_path, runtime_components, set_env_default, ChatSessionAccess, LivePermission,
    ParsedAgentPresets,
};
use dshr_compat_dsh_fs::FsWriteService;
use dshr_compat_dsh_host::{
    HostOptions, HostServiceRouter, SessionLogService, ShellProgram, SidecarHost, StorageKvService,
    ToolJobService, ToolProcessService, ToolTerminalService, WorkingDirectoryService,
    CAP_AGENT_TOOL_CONTEXT, CAP_BACKGROUND_JOBS, CAP_IMAGE_ATTACHMENTS, CAP_PROGRAM_TOOLS,
    CAP_SEARCH_PROCESS, CAP_SERVICE_PROXY, CAP_SHELL_PROCESS, CAP_TERMINAL_PROCESS, CAP_TOOL_GOALS,
    CAP_TOOL_SCHEDULES, CAP_TOOL_SESSION_QUERY, CAP_TOOL_SUBAGENTS, CAP_TOOL_TEAMS,
    CAP_USER_QUESTIONS, IMAGE_ATTACHMENTS_V1, PROTOCOL_MAX, SESSION_LOG_V1, STORAGE_KV_V1,
    TOOL_GOALS_V1, TOOL_JOBS_V1, TOOL_PROCESS_V1, TOOL_SCHEDULES_V1, TOOL_SESSION_QUERY_V1,
    TOOL_SUBAGENTS_V1, TOOL_TEAMS_V1, TOOL_TERMINALS_V1, USER_QUESTIONS_V1, WORKING_DIRECTORY_V1,
};
use dshr_plugin_host::PluginHost;
use dshr_profile::Profile;
use dshr_session_store::{SessionCreated, SessionLog};
use dshr_sidecar_guard::GuardLimits;
use dshr_tool_runtime::ApprovalBroker;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// 工具/写入的工作区根：`DSHR_WORKSPACE` → **可写的**当前目录 → 数据根下的 `workspace/`。
///
/// 和状态目录一样必须与 CWD 解耦：桌面快捷方式"起始位置"为空时 CWD 可能是
/// `C:\Windows\System32`，或 exe 被放进 Program Files。旧实现把工具工作区钉在 CWD，
/// 沙箱初始化在不可写的 CWD 上直接失败（`os error 5`），用户看到的就是"启动即退出"。
pub(crate) fn workspace_root() -> PathBuf {
    if let Some(dir) = non_empty_env_path("DSHR_WORKSPACE") {
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    if let Some(cwd) = std::env::current_dir()
        .ok()
        .filter(|dir| is_writable_dir(dir))
    {
        return cwd;
    }
    let fallback = data_root().join("workspace");
    let _ = std::fs::create_dir_all(&fallback);
    eprintln!(
        "[chat] 当前工作目录不可写，工具工作区改用 {}（可用 DSHR_WORKSPACE 指定）",
        fallback.display()
    );
    fallback
}

pub(crate) fn loader_inputs() -> Option<(PathBuf, Vec<PathBuf>)> {
    let bundle = PathBuf::from(std::env::var_os("DSHR_LOADER_BUNDLE")?);
    let patches = std::env::var_os("DSHR_LOADER_PATCH")
        .map(|value| {
            std::env::split_paths(&value)
                .filter(|path| !path.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some((bundle, patches))
}

pub(crate) fn read_agent_presets() -> ParsedAgentPresets {
    read_agent_presets_from(loader_inputs().as_ref())
}

pub(crate) fn read_agent_presets_from(
    inputs: Option<&(PathBuf, Vec<PathBuf>)>,
) -> ParsedAgentPresets {
    match inputs {
        Some((bundle, patches)) => dshr_profile::loader::AgentPresets::from_files(bundle, patches),
        None => Ok(dshr_profile::loader::AgentPresets::default()),
    }
}

pub(crate) fn creator_preset_id() -> &'static str {
    if std::env::var("DSHR_SIDECAR").as_deref() == Ok("dsh") {
        creator::CORDIS_PRESET
    } else {
        creator::PRESET
    }
}

pub(crate) fn blank_root_created(log: &SessionLog) -> Option<&SessionCreated> {
    log.created()
        .filter(|created| !created.is_child() && log.events().len() == 1)
}

pub(crate) fn prepare_desktop_sidecar(profile: &Profile) {
    set_env_default("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_DSH_PLUGINS", profile.sidecar_json());
}

/// 启动 Sidecar；失败时原生工具仍可用。
///
/// 三种运行时（`DSHR_SIDECAR`）：
/// - `probe`：最小协议探针（仅显式诊断命令）；
/// - `cordis`：真实 Cordis 4.0.5 + 只注册 `ctx.bridge.tool` 的探针插件（仅显式诊断命令）；
/// - `dsh`：桌面默认，真实 Cordis + 官方 DSH 插件。
///
/// 启动路径统一走：
/// 1. Node 解析 `DSHR_NODE` → 捆绑 `runtime/node` → PATH（`dshr-sidecar-guard`）；
/// 2. Job Object 包裹（进程数/内存上限 + 随宿主退出，Windows）；
/// 3. 官方 DSH 侧车额外协商 `fs.write`：官方 write/edit/apply_patch 的写盘请求
///    回到 Rust 走审批 + 沙箱（见 `docs/m2-official-write.md`）。
pub(crate) fn spawn_sidecar(
    approvals: Option<Arc<ApprovalBroker>>,
    l2_access: Option<Arc<ChatSessionAccess>>,
) -> Option<SidecarHost> {
    spawn_sidecar_in(&workspace_root(), approvals, None, l2_access, true)
        .inspect_err(|error| eprintln!("[chat] {error}"))
        .ok()
}

pub(crate) fn spawn_sidecar_in(
    workspace: &Path,
    approvals: Option<Arc<ApprovalBroker>>,
    live_controls: Option<(LivePermission, Arc<AtomicBool>)>,
    l2_access: Option<Arc<ChatSessionAccess>>,
    diagnostics: bool,
) -> Result<SidecarHost, String> {
    if !workspace.is_dir() {
        return Err(format!("sidecar 工作区目录不可用：{}", workspace.display()));
    }
    // 资源根按 exe 相对解析：`dist/dshr-dev` 整目录拷走后仍能找到 sidecar/（见 `app_root`）。
    let root = app_root();
    let interactive = live_controls.is_some();
    let mode = std::env::var("DSHR_SIDECAR")
        .ok()
        .filter(|mode| !mode.is_empty())
        .unwrap_or_else(|| if diagnostics { "probe" } else { "dsh" }.to_owned());
    let (script, plugin) = match mode.as_str() {
        "cordis" if diagnostics => (
            root.join("sidecar/cordis/sidecar-cordis.mjs"),
            root.join("sidecar/cordis/bridge-plugin.mjs"),
        ),
        "dsh" => {
            let dir = root.join("sidecar/dsh-official");
            let modules = dir.join("node_modules");
            if !modules.is_dir() {
                return Err(format!("官方 DSH sidecar 缺少依赖：{}", modules.display()));
            }
            let script = dir.join("sidecar-dsh.mjs");
            // 官方侧车不需要插件路径参数，但协议签名保持两个文件参数。
            (script.clone(), script)
        }
        "probe" if diagnostics => (
            root.join("sidecar/probe-sidecar.mjs"),
            root.join("sidecar/probe-plugin.mjs"),
        ),
        "probe" | "cordis" => return Err(format!("Sidecar {mode} 仅供诊断命令使用")),
        _ => return Err(format!("不支持的 Sidecar 模式 {mode}")),
    };

    let mut options = HostOptions::default()
        .with_current_dir(workspace)
        .with_env("DSHR_WORKSPACE", workspace.to_string_lossy());
    if mode == "dsh" {
        if let Some(effective) = std::env::var("DSHR_DSH_PLUGINS")
            .ok()
            .and_then(|raw| runtime_components::sidecar_profile(&raw))
        {
            let credentials: std::collections::BTreeMap<_, _> =
                runtime_components::web_credential_refs(&effective)
                    .into_iter()
                    .filter_map(|name| std::env::var(&name).ok().map(|value| (name, value)))
                    .collect();
            if !credentials.is_empty() {
                options = options.with_env(
                    "DSHR_WEB_CREDENTIAL_VALUES",
                    serde_json::to_string(&credentials).expect("string map"),
                );
            }
            options = options.with_env("DSHR_DSH_PLUGINS", effective);
        }
    }
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
            match FsWriteService::open(workspace, approvals) {
                Ok(service) => {
                    let service = if let Some((permission, cancel)) = live_controls {
                        service.with_live_controls(move || permission.file_write_policy(), cancel)
                    } else {
                        service
                    };
                    options = options.with_fs_write(Arc::new(service));
                }
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
            let search_name = if cfg!(windows) { "rg.exe" } else { "rg" };
            let arch = match std::env::consts::ARCH {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                "x86" => "ia32",
                other => other,
            };
            let platform = match std::env::consts::OS {
                "windows" => "win32",
                "macos" => "darwin",
                other => other,
            };
            let packaged_search = root.join(format!(
                "sidecar/dsh-official/node_modules/@vscode/ripgrep-{platform}-{arch}/bin/{search_name}"
            ));
            let search = if packaged_search.is_file() {
                Some(packaged_search)
            } else {
                path_executable(search_name)
            };
            let shell = if interactive {
                let (name, executable) = if cfg!(windows) {
                    ("pwsh", "pwsh.exe")
                } else {
                    ("bash", "bash")
                };
                let bundled = root.join("runtime/powershell/pwsh.exe");
                let path = if cfg!(windows) && bundled.is_file() {
                    Some(bundled)
                } else {
                    path_executable(executable)
                };
                path.map(|path| ShellProgram { name, path })
            } else {
                None
            };
            let process_caps = [
                (search.is_some(), CAP_SEARCH_PROCESS),
                (shell.is_some(), CAP_SHELL_PROCESS),
            ];
            let jobs = shell.clone().and_then(|shell| {
                ToolJobService::new(workspace, data_root().join("tool-jobs"), shell)
                    .inspect_err(|error| eprintln!("[chat] 官方后台任务不可用: {error}"))
                    .ok()
            });
            let jobs_available = jobs.is_some();
            let terminals = shell.as_ref().and_then(|shell| {
                let terminals = ToolTerminalService::new(workspace, access.clone())
                    .inspect_err(|error| eprintln!("[chat] 官方终端不可用: {error}"))
                    .ok()?;
                options.env.push((
                    "DSHR_TERMINAL_SHELL".to_owned(),
                    shell.path.to_string_lossy().into_owned(),
                ));
                Some(terminals)
            });
            let terminals_available = terminals.is_some();
            let processes = if process_caps.iter().any(|(enabled, _)| *enabled) {
                match ToolProcessService::new(workspace, search, shell) {
                    Ok(service) => Some(service),
                    Err(error) => {
                        eprintln!("[chat] 官方进程工具不可用: {error}");
                        None
                    }
                }
            } else {
                None
            };
            let processes_available = processes.is_some();
            match build_l2_router_at(
                access,
                data_root().join("storage"),
                workspace.to_path_buf(),
                "dsh-official",
                processes,
                jobs,
                terminals,
                interactive,
            ) {
                Ok(router) => {
                    options = options.with_host_services(router);
                    options.extra_caps.push(CAP_IMAGE_ATTACHMENTS.to_owned());
                    options.extra_caps.push(CAP_USER_QUESTIONS.to_owned());
                    options.extra_caps.push(CAP_AGENT_TOOL_CONTEXT.to_owned());
                    options.extra_caps.push(CAP_TOOL_GOALS.to_owned());
                    options.extra_caps.push(CAP_TOOL_SESSION_QUERY.to_owned());
                    options.extra_caps.push(CAP_TOOL_SUBAGENTS.to_owned());
                    options.extra_caps.push(CAP_TOOL_TEAMS.to_owned());
                    options.extra_caps.push(CAP_TOOL_SCHEDULES.to_owned());
                    options.extra_caps.push(CAP_PROGRAM_TOOLS.to_owned());
                    if jobs_available {
                        options.extra_caps.push(CAP_BACKGROUND_JOBS.to_owned());
                    }
                    if terminals_available {
                        options.extra_caps.push(CAP_TERMINAL_PROCESS.to_owned());
                    }
                    if processes_available {
                        options.extra_caps.extend(
                            process_caps
                                .into_iter()
                                .filter(|(enabled, _)| *enabled)
                                .map(|(_, cap)| cap.to_owned()),
                        );
                    }
                }
                Err(err) => eprintln!("[chat] L2 host-service 不可用（回退 v1）: {err}"),
            }
        }
    }
    options = options.with_guard(GuardLimits::sidecar_default());
    SidecarHost::spawn_with(script, plugin, options)
        .map_err(|error| format!("sidecar 启动失败（{mode}）：{error}"))
}

fn path_executable(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

/// 构造 v2 Host Service Router，并在 runtime.ready 协商后更新 generation。
pub(crate) fn build_l2_router_at(
    access: Arc<ChatSessionAccess>,
    storage_root: PathBuf,
    workspace: PathBuf,
    plugin_id: &str,
    processes: Option<ToolProcessService>,
    jobs: Option<ToolJobService>,
    terminals: Option<ToolTerminalService>,
    interactive: bool,
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
            &workspace,
        )))
        .map_err(|error| {
            format!(
                "register working-directory: {} ({})",
                error.message, error.code
            )
        })?;
    let images = dsh_image_attachments::ImageAttachmentService::new(
        &workspace,
        access.spill_store(),
        access.clone(),
    )
    .map_err(|error| format!("image-attachments: {error}"))?;
    router.register(Box::new(images)).map_err(|error| {
        format!(
            "register image-attachments: {} ({})",
            error.message, error.code
        )
    })?;
    router
        .register(Box::new(dsh_user_questions::UserQuestionService::new(
            interactive.then(|| access.questions.clone()),
        )))
        .map_err(|error| {
            format!(
                "register user-questions: {} ({})",
                error.message, error.code
            )
        })?;
    router
        .register(Box::new(dsh_goals::GoalService::new(access.clone())))
        .map_err(|error| format!("register tool-goals: {} ({})", error.message, error.code))?;
    router
        .register(Box::new(dsh_session_query::SessionQueryService::new(
            access.clone(),
        )))
        .map_err(|error| {
            format!(
                "register tool-session-query: {} ({})",
                error.message, error.code
            )
        })?;
    router
        .register(Box::new(dsh_subagents::SubagentService::new(
            access.clone(),
        )))
        .map_err(|error| {
            format!(
                "register tool-subagents: {} ({})",
                error.message, error.code
            )
        })?;
    router
        .register(Box::new(dsh_teams::TeamService::new(access.clone())))
        .map_err(|error| format!("register tool-teams: {} ({})", error.message, error.code))?;
    router
        .register(Box::new(dsh_schedules::ScheduleService::new(
            access.clone(),
        )))
        .map_err(|error| {
            format!(
                "register tool-schedules: {} ({})",
                error.message, error.code
            )
        })?;
    router
        .register(Box::new(SessionLogService::new(access)))
        .map_err(|error| format!("register session-log: {} ({})", error.message, error.code))?;
    let mut inject = vec![
        STORAGE_KV_V1.to_owned(),
        WORKING_DIRECTORY_V1.to_owned(),
        SESSION_LOG_V1.to_owned(),
        IMAGE_ATTACHMENTS_V1.to_owned(),
        USER_QUESTIONS_V1.to_owned(),
        TOOL_GOALS_V1.to_owned(),
        TOOL_SESSION_QUERY_V1.to_owned(),
        TOOL_SUBAGENTS_V1.to_owned(),
        TOOL_TEAMS_V1.to_owned(),
        TOOL_SCHEDULES_V1.to_owned(),
    ];
    if let Some(processes) = processes {
        router.register(Box::new(processes)).map_err(|error| {
            format!("register tool-process: {} ({})", error.message, error.code)
        })?;
        inject.push(TOOL_PROCESS_V1.to_owned());
    }
    if let Some(jobs) = jobs {
        router
            .register(Box::new(jobs))
            .map_err(|error| format!("register tool-jobs: {} ({})", error.message, error.code))?;
        inject.push(TOOL_JOBS_V1.to_owned());
    }
    if let Some(terminals) = terminals {
        router.register(Box::new(terminals)).map_err(|error| {
            format!(
                "register tool-terminals: {} ({})",
                error.message, error.code
            )
        })?;
        inject.push(TOOL_TERMINALS_V1.to_owned());
    }
    router.register_plugin_inject(plugin_id, inject);
    Ok(Arc::new(Mutex::new(router)))
}

/// 把 sidecar 注册映射进同一个 ToolRegistry。
pub(crate) fn register_sidecar_tools(plugins: &mut PluginHost, host: &SidecarHost, headless: bool) {
    for definition in host.tool_definitions() {
        if headless
            && (definition.presentation.as_deref() == Some("terminal")
                || definition.name.starts_with("terminal_")
                || matches!(
                    definition.name.as_str(),
                    "bash"
                        | "pwsh"
                        | "shell"
                        | "powershell"
                        | "exec"
                        | "job_list"
                        | "job_output"
                        | "job_kill"
                ))
        {
            continue;
        }
        let name = definition.name.clone();
        if let Err(err) = plugins.registry_mut().register(definition) {
            eprintln!("[chat] 注册 sidecar 工具 `{name}` 失败: {err}");
        }
    }
}

/// 侧车/插件安装目录（与 `--install-plugin`、sidecar 启动同源）。
pub(crate) fn sidecar_dir_for_app() -> PathBuf {
    std::env::var("DSHR_SIDECAR_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| app_root().join("sidecar/dsh-official"))
}

/// profile 目录（与 `--install-plugin` 同源）。
pub(crate) fn profile_dir_for_app() -> PathBuf {
    std::env::var("DSHR_PROFILE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".tmp/profile"))
}
