use crate::now_ts;
use dshr_tool_runtime::PermissionMode;
use std::path::PathBuf;
use std::sync::OnceLock;

pub(crate) fn desktop_permission_mode() -> Option<PermissionMode> {
    if std::env::var("DSHR_TOOL_POLICY").as_deref() == Ok("ask-all") {
        None
    } else {
        Some(PermissionMode::from_env())
    }
}

/// 非空环境变量 → 路径。空字符串按"未设置"处理（Windows 上 `set VAR=` 是常见写法）。
pub(crate) fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// 只在用户没显式设置时写入默认值；自检/测试里显式设置的 env 永远优先。
pub(crate) fn set_env_default(key: &str, value: impl Into<std::ffi::OsString>) {
    let missing = std::env::var_os(key)
        .map(|existing| existing.is_empty())
        .unwrap_or(true);
    if missing {
        std::env::set_var(key, value.into());
    }
}

/// 目录可写探测：创建一个短命探针文件。Program Files 等位置会在这里被排除。
pub(crate) fn is_writable_dir(dir: &std::path::Path) -> bool {
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
pub(crate) enum DataRootSource {
    /// 用户显式 `DSHR_HOME`。
    Home,
    /// 打包布局：exe 旁边有 `sidecar/` 且目录可写。
    ExeDir,
    /// 默认 `%LOCALAPPDATA%\dshr`。
    LocalAppData,
}

/// 纯选择逻辑：`DSHR_HOME` 优先；打包布局（exe 目录含 `sidecar/` 且可写）其次；
/// 否则 `%LOCALAPPDATA%\dshr`。创建/回退不在这个函数里做，便于单测。
pub(crate) fn pick_data_root(
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
pub(crate) fn data_root() -> PathBuf {
    DATA_ROOT.get().cloned().unwrap_or_else(resolve_data_root)
}

/// 把默认状态目录挂到数据根上；必须在 `install_crash_hook()` 之前调用（hook 读 env）。
pub(crate) fn prepare_data_env() {
    let root = resolve_data_root();
    let _ = DATA_ROOT.set(root.clone());
    let debug_assets = if non_empty_env_path("DSHR_APP_ROOT").is_none() {
        std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(std::path::Path::parent)
            .and_then(windows_debug_assets)
    } else {
        None
    };
    if let Some(assets) = &debug_assets {
        set_env_default("DSHR_RUNTIME_DIR", assets.join("runtime"));
    }
    let profile = debug_assets
        .filter(|_| non_empty_env_path("DSHR_HOME").is_none())
        .map_or_else(
            || root.join("profile"),
            |assets| assets.with_file_name("profile"),
        );
    set_env_default("DSHR_SESSIONS_DIR", root.join("sessions"));
    set_env_default("DSHR_PROFILE_DIR", profile);
    set_env_default("DSHR_SPILL_DIR", root.join("spill"));
    set_env_default("DSHR_CRASH_LOG", root.join("crash.log"));
}

/// 自检从只读 CWD（`C:\Windows\System32`、受限打包目录等）启动时，`--selftest*`
/// 使用的 `.tmp/...` 相对路径会失败。自检不依赖用户 CWD，统一切到数据根（可写、与
/// 用户数据同源）；GUI 与 `--install-plugin`/`--export-session` 等命令不切。
pub(crate) fn prepare_selftest_cwd(args: &[String]) {
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
pub(crate) fn is_cli_invocation(args: &[String]) -> bool {
    args.iter().skip(1).any(|arg| arg.starts_with("--"))
}

fn windows_debug_assets(exe_dir: &std::path::Path) -> Option<PathBuf> {
    if !cfg!(all(windows, debug_assertions)) || exe_dir.join("sidecar").is_dir() {
        return None;
    }
    let assets = exe_dir.join("dshr-debug-assets");
    assets
        .join("sidecar/dsh-official/default-profile.json")
        .is_file()
        .then_some(assets)
}

/// 应用资源根：`DSHR_APP_ROOT` → 包内资源 → 暂存的 Windows 调试资源 → 编译期仓库根。
/// 打包后整目录拷走即可运行，不再依赖构建机上的源码树。
pub(crate) fn app_root() -> PathBuf {
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
        if let Some(assets) = windows_debug_assets(&dir) {
            return assets;
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
pub(crate) fn prepare_cli_console(is_cli: bool) {
    if is_cli {
        win_console::attach_parent_console();
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
pub(crate) fn prepare_cli_console(_is_cli: bool) {}

/// 崩溃诊断：panic 时把位置/消息/工作目录追加到 `DSHR_CRASH_LOG`
/// （默认落在数据根下，见 `prepare_data_env`），再交给默认 hook（stderr 照常输出）。
/// GUI 模式下用户能看到"出过什么错"。
pub(crate) fn install_crash_hook() {
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
