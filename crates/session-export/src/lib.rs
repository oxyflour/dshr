//! 会话导出与保留策略（PLAN §6.2「保留与导出」）。
//!
//! 两件事分开做，互不隐式触发：
//!
//! 1. **导出**（[`export`]）：把会话日志写成 JSONL，把日志里引用到的外部化 blob
//!    （`spill.created`）复制到 `<session>.attachments/`，再写一份带 sha256 的
//!    `<session>.manifest.json`。导出结果可离线回放、可逐字节校验。
//! 2. **保留**（[`retention`]）：按单会话磁盘上限与段年龄**计算**可删除集合
//!    （[`retention::plan_retention`]），`apply` 只在显式调用时删文件，且做路径逃逸校验。
//!    不做"到期自动删除"，也不默认删任何东西。
//!
//! 未做项（M2 范围外，见 `docs/m2-spill.md`）：服务端/云同步附件、加密、附件 TTL
//! 自动清理、分段日志落地（依赖 session-store 的分段存储）。

#![forbid(unsafe_code)]

pub mod export;
pub mod retention;

pub use export::{
    export_session, jsonl_for_export, manifest_path, verify_export, BlobSource, ExportCounts,
    ExportError, ExportManifest, ExportOptions, NoBlobs, SpillStoreSource, VerifyReport,
    ATTACHMENTS_SUFFIX, EXPORT_FORMAT, JSONL_SUFFIX, MANIFEST_SUFFIX,
};
pub use retention::{
    apply_retention, plan_retention, DeleteReason, RetentionError, RetentionPlan, RetentionPolicy,
    RetentionReport, SegmentDeletion, SegmentUsage, SessionOverCap, SessionUsage,
    DEFAULT_KEEP_RECENT_SEGMENTS, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SESSION_BYTES,
};

#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// 测试用临时目录：每个实例唯一，Drop 时删除。
    pub struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "dshr-session-export-{}-{}-{}",
                tag,
                std::process::id(),
                counter
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        pub fn path(&self) -> &Path {
            &self.path
        }

        pub fn join(&self, relative: &str) -> PathBuf {
            self.path.join(relative)
        }

        pub fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
            let path = self.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(&path, bytes).expect("write temp file");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
