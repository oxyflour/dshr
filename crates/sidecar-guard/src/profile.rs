//! 独立 profile 目录：sidecar 的插件数据、日志与临时文件不再落在仓库根或用户
//! 全局目录，而是集中在一个可整体清理、可单独审计的目录里（PLAN §7）。
//!
//! 目录布局：
//!
//! ```text
//! <root>/
//!   node_modules/   # 安装的 npm 插件包（离线解包目标）
//!   logs/           # sidecar / 安装器日志
//!   tmp/            # 安装解包与一次性文件的暂存区
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 已创建的 profile 目录集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDirs {
    pub root: PathBuf,
    pub node_modules: PathBuf,
    pub logs: PathBuf,
    pub tmp: PathBuf,
}

impl ProfileDirs {
    /// 逐层创建目录；已存在时是幂等的。
    pub fn create(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        let node_modules = root.join("node_modules");
        let logs = root.join("logs");
        let tmp = root.join("tmp");
        for dir in [&root, &node_modules, &logs, &tmp] {
            fs::create_dir_all(dir)?;
        }
        Ok(Self {
            root,
            node_modules,
            logs,
            tmp,
        })
    }

    /// 安装解包的暂存目录（例如 `.tgz` 解压到这里后再搬到 node_modules）。
    pub fn staging_dir(&self, name: &str) -> PathBuf {
        self.tmp.join(name)
    }
}

/// 创建侧车的独立 profile 目录。
pub fn ensure_profile_dir(root: impl AsRef<Path>) -> io::Result<ProfileDirs> {
    ProfileDirs::create(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dshr-guard-profile-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn creates_profile_layout() {
        let root = temp_root("create");
        let dirs = ensure_profile_dir(&root).expect("create profile dirs");
        assert!(dirs.root.is_dir());
        assert!(dirs.node_modules.is_dir());
        assert!(dirs.logs.is_dir());
        assert!(dirs.tmp.is_dir());
        assert_eq!(dirs.staging_dir("x.tgz"), dirs.tmp.join("x.tgz"));
        // 幂等：再次调用不报错。
        ensure_profile_dir(&root).expect("idempotent");
        fs::remove_dir_all(root).ok();
    }
}
