//! Native Windows console entry shipped as bin/dshr.exe.

#[cfg(windows)]
use std::sync::atomic::{AtomicPtr, Ordering};

#[cfg(windows)]
static CANCEL_EVENT: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    match launch() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("dshr: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn launch() -> std::io::Result<std::process::ExitCode> {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::Path;
    use std::process::{Command, ExitCode, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::Win32::System::Threading::CreateEventW;

    let current = std::env::current_exe()?;
    let root = current
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "找不到 DSHR 包目录"))?;
    let desktop = root.join("dshr.exe");
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mut command = Command::new(desktop);
    if args.is_empty() {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        return Ok(ExitCode::SUCCESS);
    }
    // A GUI-subsystem child may have pipes without console control delivery.
    // Relay cancellation explicitly, then wait for the child to clean up.
    let headless = args[0]
        .to_str()
        .is_some_and(|arg| arg == "--profile" || arg.starts_with("--profile="));
    let _event = if headless {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!("Local\\DSHR-Headless-{}-{stamp}", std::process::id());
        let wide: Vec<_> = name.encode_utf16().chain([0]).collect();
        let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let event = unsafe { OwnedHandle::from_raw_handle(raw) };
        CANCEL_EVENT.store(event.as_raw_handle(), Ordering::Release);
        unsafe { SetConsoleCtrlHandler(None, 0) };
        if unsafe { SetConsoleCtrlHandler(Some(cancel_child), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        command.env("DSHR_CONSOLE_CANCEL_EVENT", name);
        Some(event)
    } else {
        None
    };
    let code = command
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?
        .code()
        .unwrap_or(1);
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}

#[cfg(windows)]
unsafe extern "system" fn cancel_child(event: u32) -> i32 {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    use windows_sys::Win32::System::Threading::SetEvent;
    if !matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT) {
        return 0;
    }
    let handle = CANCEL_EVENT.load(Ordering::Acquire);
    if !handle.is_null() {
        unsafe { SetEvent(handle) };
    }
    1
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::FAILURE
}
