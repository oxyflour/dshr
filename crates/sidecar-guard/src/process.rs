//! 子进程守卫：Windows 用 Job Object 把 sidecar（及其子进程）装进一个可整体终止、
//! 带资源上限的容器；其它平台退化为普通 spawn。
//!
//! # 用法
//!
//! ```no_run
//! use std::process::Command;
//! use dshr_sidecar_guard::{spawn_guarded, GuardLimits};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut command = Command::new("node");
//! command.arg("sidecar/dsh-official/sidecar-dsh.mjs");
//! let mut child = spawn_guarded(
//!     command,
//!     "sidecar/dsh-official",
//!     &[("DSHR_DSH_PLUGINS".to_string(), "[]".to_string())],
//!     &GuardLimits::default(),
//! )?;
//! let exit = child.wait()?;
//! # let _ = exit;
//! # Ok(())
//! # }
//! ```
//!
//! # 边界
//!
//! Job Object 只限制 CPU 时间/内存/活跃进程数并保证"关掉 job 就杀干净"，**不是**
//! 文件系统或网络沙箱。`KILL_ON_JOB_CLOSE` 保证宿主进程异常退出时子进程不残留。

use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::time::Duration;

use crate::GuardError;

/// 资源上限。`None` 表示该项不限制。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardLimits {
    /// job 内同时存在的活跃进程数上限（Node 主进程 + 它派生的子进程）。
    pub max_processes: Option<u32>,
    /// job 内所有进程的提交内存上限（JobMemoryLimit）。
    pub max_memory_bytes: Option<u64>,
    /// job 累计用户态 CPU 时间上限；超过后系统终止 job 内所有进程。
    pub max_cpu_time: Option<Duration>,
}

impl GuardLimits {
    /// 不设任何上限（仍保留 `KILL_ON_JOB_CLOSE`）。
    pub const fn unlimited() -> Self {
        Self {
            max_processes: None,
            max_memory_bytes: None,
            max_cpu_time: None,
        }
    }

    /// sidecar 的默认上限：16 个进程、1 GiB 内存、不限 CPU 时间。
    pub const fn sidecar_default() -> Self {
        Self {
            max_processes: Some(16),
            max_memory_bytes: Some(1024 * 1024 * 1024),
            max_cpu_time: None,
        }
    }
}

impl Default for GuardLimits {
    fn default() -> Self {
        Self::sidecar_default()
    }
}

/// 被 guard 包裹的子进程。
///
/// `Drop` 时（Windows）先 `TerminateJobObject` 杀掉 job 内所有进程，再关闭 job
/// 句柄（`KILL_ON_JOB_CLOSE` 兜底），最后 `wait` 回收；其它平台直接 kill + wait。
pub struct GuardedChild {
    child: Child,
    #[cfg(windows)]
    job: imp::JobHandle,
}

/// Job Object 句柄是所有权的内核对象；`GuardedChild` 在任意时刻只由一个线程使用，
/// 移动所有权到别的线程是安全的（例如放进 `Mutex<SidecarHost>` 后再由工作线程操作）。
/// `HANDLE` 本身不是 `Send`，因此需要显式声明。
#[cfg(windows)]
unsafe impl Send for GuardedChild {}

impl GuardedChild {
    /// 子进程 PID。
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// 只读访问底层 `Child`（例如读 `stdout`）。
    pub fn child(&self) -> &Child {
        &self.child
    }

    /// 可变访问底层 `Child`（例如 `take` 管道）。
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// 终止 job 内全部进程（Windows）或单个子进程（其它平台）。
    pub fn kill(&mut self) -> std::io::Result<()> {
        #[cfg(windows)]
        {
            let job_terminated = imp::terminate_job(&self.job);
            let child_result = self.child.kill();
            // TerminateJobObject 成功即视为成功（进程可能已先被杀掉）。
            if job_terminated {
                Ok(())
            } else {
                child_result
            }
        }
        #[cfg(not(windows))]
        {
            self.child.kill()
        }
    }

    /// 等待子进程退出。
    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.child.wait()
    }

    /// 非阻塞地查询子进程是否退出。
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

impl Drop for GuardedChild {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // 即使调用方忘了 kill，也不会留下孤儿 Node 进程。
            let _ = imp::terminate_job(&self.job);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 启动一个受 guard 保护的子进程。
///
/// 环境变量策略：**先清空**继承环境，再写入清洗后的当前进程环境（剔除凭据类变量），
/// 最后叠加 `env` 参数（显式传入的变量不过滤，代表调用方的显式授权）。
/// `cwd` 也由这里统一设置，避免调用方漏设。
pub fn spawn_guarded(
    command: Command,
    cwd: impl AsRef<Path>,
    env: &[(String, String)],
    limits: &GuardLimits,
) -> Result<GuardedChild, GuardError> {
    spawn_guarded_with(command, cwd, env, limits, &[])
}

/// 与 [`spawn_guarded`] 相同，但允许调用方指定继承环境的 allowlist。
pub fn spawn_guarded_with(
    mut command: Command,
    cwd: impl AsRef<Path>,
    env: &[(String, String)],
    limits: &GuardLimits,
    allowlist: &[&str],
) -> Result<GuardedChild, GuardError> {
    command.current_dir(cwd.as_ref());
    command.env_clear();
    for (key, value) in crate::inherited_env(allowlist) {
        command.env(key, value);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    imp::spawn(command, limits)
}

/// Duration → Job Object 的 100ns tick；0 或不足 1 tick 视为参数错误。
pub(crate) fn cpu_limit_ticks(limit: Duration) -> Result<i64, GuardError> {
    let ticks = limit.as_nanos() / 100;
    if ticks == 0 {
        return Err(GuardError::Invalid(String::from(
            "max_cpu_time 必须至少 100ns（Job Object 时间单位为 100ns）",
        )));
    }
    Ok(ticks.min(i64::MAX as u128) as i64)
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::io;
    use std::mem::{size_of, zeroed};
    use std::os::windows::io::AsRawHandle;
    use std::process::Command;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_JOB_TIME,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    use super::{cpu_limit_ticks, GuardLimits, GuardedChild};
    use crate::GuardError;

    /// Job Object 句柄的 RAII 包装：Drop 时 CloseHandle。
    ///
    /// job 配置了 `KILL_ON_JOB_CLOSE`，所以最后一个句柄关闭会连带终止 job 内进程。
    pub(super) struct JobHandle(HANDLE);

    impl Drop for JobHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    pub(super) fn spawn(
        mut command: Command,
        limits: &GuardLimits,
    ) -> Result<GuardedChild, GuardError> {
        let job = create_job()?;
        configure(&job, limits)?;
        let child = command.spawn()?;
        let process_handle = child.as_raw_handle() as HANDLE;
        let assigned = unsafe { AssignProcessToJobObject(job.0, process_handle) };
        if assigned == 0 {
            let error = windows_error("AssignProcessToJobObject");
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(GuardedChild { child, job })
    }

    fn create_job() -> Result<JobHandle, GuardError> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(windows_error("CreateJobObjectW"));
        }
        Ok(JobHandle(job))
    }

    fn configure(job: &JobHandle, limits: &GuardLimits) -> Result<(), GuardError> {
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        if let Some(max_processes) = limits.max_processes {
            flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            info.BasicLimitInformation.ActiveProcessLimit = max_processes;
        }
        if let Some(max_memory_bytes) = limits.max_memory_bytes {
            flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.JobMemoryLimit = usize::try_from(max_memory_bytes).unwrap_or(usize::MAX);
        }
        if let Some(max_cpu_time) = limits.max_cpu_time {
            flags |= JOB_OBJECT_LIMIT_JOB_TIME;
            info.BasicLimitInformation.PerJobUserTimeLimit = cpu_limit_ticks(max_cpu_time)?;
        }
        info.BasicLimitInformation.LimitFlags = flags;

        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(windows_error("SetInformationJobObject"));
        }
        Ok(())
    }

    /// 终止 job 内所有进程；返回是否成功。
    pub(super) fn terminate_job(job: &JobHandle) -> bool {
        !job.0.is_null() && unsafe { TerminateJobObject(job.0, 1) != 0 }
    }

    fn windows_error(call: &'static str) -> GuardError {
        let error = io::Error::last_os_error();
        GuardError::Windows {
            call,
            code: error.raw_os_error().unwrap_or(0) as u32,
            message: error.to_string(),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::process::Command;

    use super::{GuardLimits, GuardedChild};
    use crate::GuardError;

    pub(super) fn spawn(
        command: Command,
        _limits: &GuardLimits,
    ) -> Result<GuardedChild, GuardError> {
        let child = command.spawn()?;
        Ok(GuardedChild { child })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    use std::io::Read;
    #[cfg(windows)]
    use std::process::Stdio;
    #[cfg(windows)]
    use std::time::Instant;

    #[cfg(windows)]
    #[test]
    fn spawns_cmd_and_reads_stdout() {
        let mut command = Command::new("cmd");
        command
            .args(["/c", "echo", "guarded-ok"])
            .stdout(Stdio::piped());
        let mut guarded = spawn_guarded(
            command,
            std::env::temp_dir(),
            &[],
            &GuardLimits::sidecar_default(),
        )
        .expect("spawn guarded");
        let mut stdout = guarded.child_mut().stdout.take().expect("piped stdout");
        let mut text = String::new();
        stdout.read_to_string(&mut text).expect("read stdout");
        let status = guarded.wait().expect("wait");
        assert!(status.success(), "status={status:?}");
        assert!(text.contains("guarded-ok"), "stdout={text:?}");
    }

    #[cfg(windows)]
    #[test]
    fn drop_terminates_the_guarded_process() {
        // ping 在本机回环上大约跑 29 秒；如果 Drop 没杀进程，测试会超时失败。
        let mut command = Command::new("cmd");
        command
            .args(["/c", "ping", "-n", "30", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let guarded = spawn_guarded(
            command,
            std::env::temp_dir(),
            &[],
            &GuardLimits::sidecar_default(),
        )
        .expect("spawn guarded");
        let pid = guarded.id();
        assert!(process_alive(pid), "子进程 {pid} 应当在运行");
        drop(guarded);

        // TerminateJobObject 是同步调用，但仍给系统一点时间清理。
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if !process_alive(pid) {
                break;
            }
            assert!(Instant::now() < deadline, "Drop 之后子进程 {pid} 仍在运行");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[cfg(windows)]
    fn process_alive(pid: u32) -> bool {
        let filter = format!("PID eq {pid}");
        let output = Command::new("tasklist")
            .args(["/FI", &filter, "/NH", "/FO", "CSV"])
            .output()
            .expect("run tasklist");
        String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
    }

    #[test]
    fn cpu_limit_rejects_zero_and_converts_ticks() {
        assert!(cpu_limit_ticks(Duration::ZERO).is_err());
        assert_eq!(
            cpu_limit_ticks(Duration::from_millis(1)).expect("ticks"),
            10_000
        );
        assert_eq!(
            cpu_limit_ticks(Duration::from_secs(60)).expect("ticks"),
            600_000_000
        );
    }
}
