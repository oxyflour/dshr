//! Sidecar OS 级隔离（`docs/PLAN.md` §7）：Windows Job Object、环境变量清洗、
//! 独立 profile 目录与 Node 运行时解析。
//!
//! 目标不是把 Node 变成权限沙箱，而是**让失控的 sidecar 不能拖垮宿主**，并确保
//! 宿主凭据不会随 `Command` 的默认环境继承泄漏给插件运行时：
//!
//! - [`spawn_guarded`] / [`spawn_guarded_with`]：Windows 下创建 Job Object
//!   （`KILL_ON_JOB_CLOSE` + 进程数/内存/可选 CPU 时间上限），把子进程放进 job；
//!   [`GuardedChild`] Drop 时终止整个 job。非 Windows 平台退化为普通
//!   `Command::spawn`（Linux/macOS 的隔离在 M4）。
//! - [`sanitize_env`] / [`inherited_env`]：剔除 `DEEPSEEK_API_KEY`、`*_KEY`、
//!   `*_TOKEN`、`*_SECRET`，支持 allowlist。
//! - [`ensure_profile_dir`]：为 sidecar 创建独立的 profile 目录。
//! - [`resolve_node`]：`DSHR_NODE` → 随包 `runtime/node` → `PATH` 的 Node 解析顺序。
//!
//! **安全边界**（详见 `docs/m2-runtime.md`）：Job Object 限制的是资源与进程生命周期，
//! 不是文件/网络的权限沙箱；装有 npm 插件的 sidecar 仍以当前用户身份执行任意代码，
//! 安装前必须显式确认。

pub mod node;
pub mod process;
pub mod profile;

pub use node::{resolve_node, NodeRuntime, NodeSource};
pub use process::{spawn_guarded, spawn_guarded_with, GuardLimits, GuardedChild};
pub use profile::{ensure_profile_dir, ProfileDirs};

use std::fmt;
use std::io;

/// guard 层错误：I/O、Windows API、参数与平台能力。
#[derive(Debug)]
pub enum GuardError {
    /// 标准库 I/O（spawn/wait/create_dir 等）。
    Io(io::Error),
    /// Windows API 调用失败（保留调用名与 Win32 错误码，便于诊断）。
    Windows {
        call: &'static str,
        code: u32,
        message: String,
    },
    /// 当前平台没有该能力（例如非 Windows 的 Job Object 限制）。
    Unsupported(&'static str),
    /// 参数不合法（例如 CPU 时间上限小于一个 100ns tick）。
    Invalid(String),
}

impl fmt::Display for GuardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuardError::Io(err) => write!(formatter, "io: {err}"),
            GuardError::Windows {
                call,
                code,
                message,
            } => write!(formatter, "Windows API {call} 失败（{code}）: {message}"),
            GuardError::Unsupported(what) => write!(formatter, "当前平台不支持: {what}"),
            GuardError::Invalid(message) => write!(formatter, "参数无效: {message}"),
        }
    }
}

impl std::error::Error for GuardError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GuardError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for GuardError {
    fn from(err: io::Error) -> Self {
        GuardError::Io(err)
    }
}

/// 环境变量名是否属于必须剔除的凭据类。
///
/// 规则（大小写不敏感）：`DEEPSEEK_API_KEY` 精确匹配，或后缀 `_KEY` / `_TOKEN` /
/// `_SECRET`。`allowlist` 中的名字即使命中规则也会保留——用于已知需要透传、
/// 且由调用方显式审计过的变量。
pub fn is_secret_env(key: &str, allowlist: &[&str]) -> bool {
    if allowlist
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(key))
    {
        return false;
    }
    let upper = key.to_ascii_uppercase();
    upper == "DEEPSEEK_API_KEY"
        || upper.ends_with("_KEY")
        || upper.ends_with("_TOKEN")
        || upper.ends_with("_SECRET")
}

/// 过滤环境变量：保持原有顺序，只删除凭据类变量。
///
/// 纯函数，便于单元测试；`spawn_guarded` 的默认策略是 allowlist 为空。
pub fn sanitize_env(vars: &[(String, String)], allowlist: &[&str]) -> Vec<(String, String)> {
    vars.iter()
        .filter(|(key, _)| !is_secret_env(key, allowlist))
        .cloned()
        .collect()
}

/// 当前进程的继承环境（已清洗），供子进程重建环境用。
///
/// 非 Unicode 的环境变量名/值直接丢弃（Windows 上极罕见；宁可少传也不猜编码）。
pub fn inherited_env(allowlist: &[&str]) -> Vec<(String, String)> {
    let vars: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    sanitize_env(&vars, allowlist)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vec<(String, String)> {
        [
            ("PATH", "C:\\Windows"),
            ("DEEPSEEK_API_KEY", "sk-secret"),
            ("OPENAI_API_KEY", "sk-openai"),
            ("GITHUB_TOKEN", "gh-token"),
            ("AWS_SECRET_ACCESS_KEY", "aws-secret"),
            ("AWS_CLIENT_SECRET", "client-secret"),
            ("DSHR_SIDECAR", "dsh"),
            ("USERPROFILE", "C:\\Users\\demo"),
        ]
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
    }

    #[test]
    fn sanitize_removes_credentials_and_keeps_normal_vars() {
        let cleaned = sanitize_env(&vars(), &[]);
        let keys: Vec<&str> = cleaned.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, vec!["PATH", "DSHR_SIDECAR", "USERPROFILE"]);
    }

    #[test]
    fn sanitize_matches_case_insensitively() {
        let input = vec![
            ("deepseek_api_key".to_string(), "x".to_string()),
            ("my_Token".to_string(), "y".to_string()),
            ("My_Secret".to_string(), "z".to_string()),
            ("KEEP_ME".to_string(), "ok".to_string()),
        ];
        let cleaned = sanitize_env(&input, &[]);
        assert_eq!(cleaned, vec![("KEEP_ME".to_string(), "ok".to_string())]);
    }

    #[test]
    fn allowlist_preserves_named_secret_vars() {
        let cleaned = sanitize_env(&vars(), &["deepseek_api_key", "GITHUB_TOKEN"]);
        let keys: Vec<&str> = cleaned.iter().map(|(key, _)| key.as_str()).collect();
        assert!(keys.contains(&"DEEPSEEK_API_KEY"));
        assert!(keys.contains(&"GITHUB_TOKEN"));
        // 不在 allowlist 里的凭据仍然被剔除。
        assert!(!keys.contains(&"OPENAI_API_KEY"));
        assert!(!keys.contains(&"AWS_CLIENT_SECRET"));
    }

    #[test]
    fn inherited_env_never_contains_dedicated_api_key() {
        // 只做语义断言：不存在则跳过（CI 环境不保证有这个变量）。
        let cleaned = inherited_env(&[]);
        assert!(cleaned.iter().all(|(key, _)| !is_secret_env(key, &[])));
    }
}
