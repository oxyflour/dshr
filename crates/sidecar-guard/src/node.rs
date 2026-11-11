//! Node 运行时解析（`docs/PLAN.md` §5.5）。
//!
//! 解析顺序（与 `docs/m2-runtime.md` 一致）：
//!
//! 1. `DSHR_NODE`：显式指定的 node 可执行文件（开发/企业自定义路径）；
//! 2. `DSHR_RUNTIME_DIR`：显式指定的 runtime 目录（其下 `node/node.exe`）；
//! 3. 随包 vendor 的 `runtime/node/node.exe`（相对宿主 exe 或当前目录查找）；
//! 4. `PATH` 上的 `node`。
//!
//! 返回 `None` 表示四处都没有找到：调用方应进入 Native-only 模式并提示用户
//! （PLAN §5.5「首次启动无网络时进入 Native-only 模式，不阻塞 UI」）。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// node 可执行文件的来源（用于日志/诊断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeSource {
    /// `DSHR_NODE` 显式指定。
    Env,
    /// 随包 vendored 运行时（`runtime/node`）。
    Vendored,
    /// 系统 `PATH`。
    Path,
}

impl NodeSource {
    pub fn describe(self) -> &'static str {
        match self {
            NodeSource::Env => "DSHR_NODE",
            NodeSource::Vendored => "runtime/node",
            NodeSource::Path => "PATH",
        }
    }
}

/// 解析到的 Node 运行时。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRuntime {
    pub program: PathBuf,
    pub source: NodeSource,
}

impl NodeRuntime {
    /// 是否随包分发（决定是否需要提示用户安装 DSH）。
    pub fn is_vendored(&self) -> bool {
        self.source == NodeSource::Vendored
    }
}

/// 用当前进程环境解析 Node 可执行文件。
pub fn resolve_node() -> Option<NodeRuntime> {
    let exe = std::env::current_exe().ok();
    let cwd = std::env::current_dir().ok();
    let path_env = std::env::var_os("PATH");
    resolve_node_with(
        std::env::var("DSHR_NODE").ok().as_deref(),
        std::env::var("DSHR_RUNTIME_DIR").ok().as_deref(),
        exe.as_deref(),
        cwd.as_deref(),
        path_env.as_deref(),
    )
}

/// 可注入环境的解析实现（单测不需要修改进程全局环境）。
pub fn resolve_node_with(
    dshr_node: Option<&str>,
    runtime_dir: Option<&str>,
    current_exe: Option<&Path>,
    cwd: Option<&Path>,
    path_env: Option<&OsStr>,
) -> Option<NodeRuntime> {
    if let Some(explicit) = dshr_node.filter(|value| !value.is_empty()) {
        return Some(NodeRuntime {
            program: PathBuf::from(explicit),
            source: NodeSource::Env,
        });
    }
    if let Some(runtime) = runtime_dir.filter(|value| !value.is_empty()) {
        if let Some(found) = find_vendored_node(Path::new(runtime)) {
            return Some(NodeRuntime {
                program: found,
                source: NodeSource::Vendored,
            });
        }
    }
    if let Some(exe) = current_exe {
        for ancestor in exe.ancestors().filter(|path| path.is_dir()) {
            if let Some(found) = find_vendored_node(ancestor) {
                return Some(NodeRuntime {
                    program: found,
                    source: NodeSource::Vendored,
                });
            }
        }
    }
    if let Some(cwd) = cwd {
        if let Some(found) = find_vendored_node(cwd) {
            return Some(NodeRuntime {
                program: found,
                source: NodeSource::Vendored,
            });
        }
    }
    if let Some(path_env) = path_env {
        if let Some(found) = find_in_path(path_env, node_exe_name()) {
            return Some(NodeRuntime {
                program: found,
                source: NodeSource::Path,
            });
        }
    }
    None
}

/// 在 `base` 下查找 vendored node。`base` 可以是仓库根、`runtime/` 或
/// `runtime/node/`，三种布局都接受。
pub fn find_vendored_node(base: &Path) -> Option<PathBuf> {
    let exe = node_exe_name();
    let candidates = [
        base.join("runtime").join("node").join(exe),
        base.join("node").join(exe),
        base.join("node").join("bin").join(exe),
        base.join(exe),
    ];
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// 在 `PATH` 风格的列表里查找可执行文件；跳过空条目（避免把 CWD 当 PATH）。
pub fn find_in_path(path_env: &OsStr, executable: &str) -> Option<PathBuf> {
    std::env::split_paths(path_env)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(executable))
        .find(|candidate| candidate.is_file())
}

/// 平台上的 node 可执行文件名。
pub fn node_exe_name() -> &'static str {
    if cfg!(windows) {
        "node.exe"
    } else {
        "node"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dshr-guard-node-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&root).expect("temp root");
        root
    }

    fn fake_node(dir: &Path) -> PathBuf {
        let program = dir.join(node_exe_name());
        fs::write(&program, b"fake").expect("fake node");
        program
    }

    #[test]
    fn finds_vendored_node_in_three_layouts() {
        let root = temp_root("layouts");

        let repo = root.join("repo");
        fs::create_dir_all(repo.join("runtime").join("node")).unwrap();
        let program = fake_node(&repo.join("runtime").join("node"));
        assert_eq!(find_vendored_node(&repo), Some(program));

        let runtime = root.join("runtime");
        fs::create_dir_all(runtime.join("node")).unwrap();
        let program = fake_node(&runtime.join("node"));
        assert_eq!(find_vendored_node(&runtime), Some(program));

        let node_dir = root.join("node-dir");
        fs::create_dir_all(&node_dir).unwrap();
        let program = fake_node(&node_dir);
        assert_eq!(find_vendored_node(&node_dir), Some(program));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn explicit_dshr_node_wins() {
        let resolved = resolve_node_with(
            Some("C:\\custom\\node.exe"),
            Some("C:\\dsh\\runtime"),
            None,
            None,
            None,
        )
        .expect("env override");
        assert_eq!(resolved.source, NodeSource::Env);
        assert_eq!(resolved.program, PathBuf::from("C:\\custom\\node.exe"));
    }

    #[test]
    fn runtime_dir_and_path_fallbacks_work() {
        let root = temp_root("fallback");
        fs::create_dir_all(root.join("node")).unwrap();
        fake_node(&root.join("node"));
        let resolved = resolve_node_with(
            None,
            Some(root.join("runtime-parent").to_str().unwrap()),
            None,
            None,
            None,
        );
        assert!(resolved.is_none(), "不存在的 runtime_dir 不应命中");

        let runtime = root.join("runtime");
        fs::create_dir_all(runtime.join("node")).unwrap();
        let program2 = fake_node(&runtime.join("node"));
        let resolved = resolve_node_with(None, Some(runtime.to_str().unwrap()), None, None, None)
            .expect("runtime dir");
        assert_eq!(resolved.source, NodeSource::Vendored);
        assert_eq!(resolved.program, program2);

        let bin_dir = root.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let path_program = fake_node(&bin_dir);
        let path_env = std::env::join_paths([&bin_dir]).expect("join paths");
        let resolved =
            resolve_node_with(None, None, None, None, Some(path_env.as_os_str())).expect("path");
        assert_eq!(resolved.source, NodeSource::Path);
        assert_eq!(resolved.program, path_program);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn nothing_found_returns_none() {
        let root = temp_root("none");
        let empty_path = std::env::join_paths([&root]).expect("join paths");
        // 不传 current_exe/cwd：否则临时目录的祖先可能命中仓库里已 vendor 的
        // runtime/node（build-env 把 TMP/TEMP 指向仓库 .tmp），测试会依赖本机状态。
        let resolved =
            resolve_node_with(Some(""), Some(""), None, None, Some(empty_path.as_os_str()));
        assert!(resolved.is_none());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn find_in_path_skips_empty_entries() {
        assert!(find_in_path(OsStr::new(""), node_exe_name()).is_none());
    }
}
