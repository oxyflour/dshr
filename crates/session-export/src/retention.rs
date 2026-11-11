//! 保留策略：**plan（纯计算）与 apply（显式删除）分离**。
//!
//! - [`plan_retention`] 只读输入（`SessionUsage` 列表 + 当前时间），返回"可删除集合"，
//!   不接触磁盘、不删除任何东西；
//! - [`apply_retention`] 只有在调用方显式调用时才删 `plan.deletions` 里列出的段文件，
//!   并且逐个做路径逃逸校验（绝对路径 / `..` / 符号链接逃逸一律拒绝）。
//!
//! 删除判定（按 spec/session-log.md §8 的默认值）：
//! 1. **按段年龄**：段最后修改时间早于 `max_age_days` 天，且不属于最近
//!    `keep_recent_segments` 段（安全兜底：最新一段永远不因年龄被删）；
//! 2. **单会话磁盘上限**：会话（段 + 附件）超过 `max_session_bytes` 时，从最旧的
//!    未保护段继续删，直到降到上限内；
//! 3. 只删段，不自动删附件；附件字节仍计入会话总量，因此"附件导致无法达标"会记录在
//!    `over_cap` 里由调用方决定（M2 不做附件 TTL 自动清理）。
//!
//! 输入由调用方（未来的 session-store 分段落地 / 导出目录扫描）提供，这样保留逻辑
//! 不依赖具体存储布局，也便于用临时目录测试。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path};

use thiserror::Error;

/// 单会话磁盘上限（spec §8：2 GB）。
pub const DEFAULT_MAX_SESSION_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 段/附件默认保留期（天）。
pub const DEFAULT_MAX_AGE_DAYS: u64 = 90;

/// 每个会话始终保留的最近段数（安全兜底）。
pub const DEFAULT_KEEP_RECENT_SEGMENTS: usize = 1;

const SECS_PER_DAY: u64 = 86_400;

/// 保留策略参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub max_session_bytes: u64,
    pub max_age_days: u64,
    pub keep_recent_segments: usize,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_session_bytes: DEFAULT_MAX_SESSION_BYTES,
            max_age_days: DEFAULT_MAX_AGE_DAYS,
            keep_recent_segments: DEFAULT_KEEP_RECENT_SEGMENTS,
        }
    }
}

impl RetentionPolicy {
    /// 按显式参数构造。
    pub fn new(max_session_bytes: u64, max_age_days: u64, keep_recent_segments: usize) -> Self {
        Self {
            max_session_bytes,
            max_age_days,
            keep_recent_segments,
        }
    }
}

/// 一个可删除的日志段（会话内按 turn 切分的文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentUsage {
    pub segment_id: String,
    /// 相对 `root` 的文件路径（`apply_retention` 的安全校验对象）。
    pub relative_path: String,
    pub size_bytes: u64,
    /// 段最后活动时间（Unix 秒）。
    pub last_modified_secs: u64,
}

/// 一个会话的磁盘占用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUsage {
    pub session_id: String,
    pub segments: Vec<SegmentUsage>,
    /// 该会话外部化附件总字节数（计入上限，但不由本模块删除）。
    pub attachment_bytes: u64,
}

impl SessionUsage {
    pub fn new(
        session_id: impl Into<String>,
        segments: Vec<SegmentUsage>,
        attachment_bytes: u64,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            segments,
            attachment_bytes,
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.segments
            .iter()
            .map(|segment| segment.size_bytes)
            .sum::<u64>()
            + self.attachment_bytes
    }
}

/// 删除原因（进计划、可审计）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteReason {
    /// 段年龄超期。
    Age,
    /// 会话超过磁盘上限。
    OverCap,
}

/// 计划中的一条删除。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentDeletion {
    pub session_id: String,
    pub segment_id: String,
    pub relative_path: String,
    pub size_bytes: u64,
    pub reason: DeleteReason,
}

/// 删到上限内仍不达标的会话（通常因为附件不可删）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOverCap {
    pub session_id: String,
    pub total_bytes: u64,
    pub bytes_after_deletions: u64,
}

/// 保留计划：纯数据，`is_empty()` 为真时表示不需要任何清理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPlan {
    pub deletions: Vec<SegmentDeletion>,
    pub over_cap: Vec<SessionOverCap>,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

impl RetentionPlan {
    pub fn is_empty(&self) -> bool {
        self.deletions.is_empty()
    }

    pub fn bytes_reclaimed(&self) -> u64 {
        self.bytes_before.saturating_sub(self.bytes_after)
    }

    pub fn deletions_for(&self, session_id: &str) -> Vec<&SegmentDeletion> {
        self.deletions
            .iter()
            .filter(|deletion| deletion.session_id == session_id)
            .collect()
    }
}

/// 计算可删除集合（不触碰磁盘、不删除）。
///
/// 输入顺序不影响结果：每个会话内部按"最旧优先 / 最新保护"确定规则排序。
pub fn plan_retention(
    sessions: &[SessionUsage],
    policy: &RetentionPolicy,
    now_epoch_secs: u64,
) -> RetentionPlan {
    let max_age_secs = policy.max_age_days.saturating_mul(SECS_PER_DAY);
    let mut deletions: Vec<SegmentDeletion> = Vec::new();
    let mut over_cap: Vec<SessionOverCap> = Vec::new();
    let mut bytes_before = 0u64;

    for session in sessions {
        let total = session.total_bytes();
        bytes_before = bytes_before.saturating_add(total);

        // 最新 N 段保护（时间相同按 segment_id 稳定排序）。
        let mut recency: Vec<usize> = (0..session.segments.len()).collect();
        recency.sort_by(|left, right| {
            let a = &session.segments[*left];
            let b = &session.segments[*right];
            b.last_modified_secs
                .cmp(&a.last_modified_secs)
                .then_with(|| a.segment_id.cmp(&b.segment_id))
        });
        let protected: HashSet<usize> = recency
            .into_iter()
            .take(policy.keep_recent_segments)
            .collect();

        // 最旧优先的候选顺序。
        let mut order: Vec<usize> = (0..session.segments.len()).collect();
        order.sort_by(|left, right| {
            let a = &session.segments[*left];
            let b = &session.segments[*right];
            a.last_modified_secs
                .cmp(&b.last_modified_secs)
                .then_with(|| a.segment_id.cmp(&b.segment_id))
        });

        let mut reasons: HashMap<usize, DeleteReason> = HashMap::new();
        let mut remaining = total;

        // 1) 按年龄：只删未保护段。
        for index in &order {
            if protected.contains(index) {
                continue;
            }
            let segment = &session.segments[*index];
            let age = now_epoch_secs.saturating_sub(segment.last_modified_secs);
            if age > max_age_secs {
                reasons.insert(*index, DeleteReason::Age);
                remaining = remaining.saturating_sub(segment.size_bytes);
            }
        }

        // 2) 单会话上限：仍超则从最旧的未保护段继续删（绝不重复/越过保护段）。
        if remaining > policy.max_session_bytes {
            for index in &order {
                if remaining <= policy.max_session_bytes {
                    break;
                }
                if protected.contains(index) || reasons.contains_key(index) {
                    continue;
                }
                let segment = &session.segments[*index];
                reasons.insert(*index, DeleteReason::OverCap);
                remaining = remaining.saturating_sub(segment.size_bytes);
            }
        }

        // 计划按"最旧优先"的顺序输出，保证同一输入产生同一计划。
        for index in &order {
            let Some(reason) = reasons.get(index) else {
                continue;
            };
            let segment = &session.segments[*index];
            deletions.push(SegmentDeletion {
                session_id: session.session_id.clone(),
                segment_id: segment.segment_id.clone(),
                relative_path: segment.relative_path.clone(),
                size_bytes: segment.size_bytes,
                reason: *reason,
            });
        }

        if remaining > policy.max_session_bytes {
            over_cap.push(SessionOverCap {
                session_id: session.session_id.clone(),
                total_bytes: total,
                bytes_after_deletions: remaining,
            });
        }
    }

    let bytes_reclaimed: u64 = deletions.iter().map(|deletion| deletion.size_bytes).sum();
    RetentionPlan {
        deletions,
        over_cap,
        bytes_before,
        bytes_after: bytes_before.saturating_sub(bytes_reclaimed),
    }
}

/// 执行保留计划时的问题（逐文件记录，不中断其余删除）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RetentionReport {
    pub deleted_files: usize,
    pub bytes_freed: u64,
    pub errors: Vec<String>,
}

impl RetentionReport {
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum RetentionError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("root 不存在或不是目录: {0}")]
    InvalidRoot(String),
}

/// 显式执行删除：只删计划里列出的段文件，逐个做路径逃逸校验。
///
/// 这一步**不会**被 plan 自动触发；调用方必须先展示计划并确认。
pub fn apply_retention(
    root: &Path,
    plan: &RetentionPlan,
) -> Result<RetentionReport, RetentionError> {
    let canonical_root = root
        .canonicalize()
        .map_err(|_| RetentionError::InvalidRoot(root.display().to_string()))?;
    if !canonical_root.is_dir() {
        return Err(RetentionError::InvalidRoot(root.display().to_string()));
    }

    let mut report = RetentionReport::default();
    for deletion in &plan.deletions {
        let relative = Path::new(&deletion.relative_path);
        if !is_safe_relative(relative) {
            report
                .errors
                .push(format!("非法相对路径，已跳过: {}", deletion.relative_path));
            continue;
        }
        let target = canonical_root.join(relative);
        let canonical = match target.canonicalize() {
            Ok(path) => path,
            Err(err) => {
                report
                    .errors
                    .push(format!("无法定位 {}: {err}", deletion.relative_path));
                continue;
            }
        };
        // 符号链接/前缀攻击：解析后的真实路径必须仍在 root 之内。
        if !canonical.starts_with(&canonical_root) {
            report
                .errors
                .push(format!("路径逃逸 root，已拒绝: {}", deletion.relative_path));
            continue;
        }
        if !canonical.is_file() {
            report
                .errors
                .push(format!("不是普通文件: {}", deletion.relative_path));
            continue;
        }
        match fs::remove_file(&canonical) {
            Ok(()) => {
                report.deleted_files += 1;
                report.bytes_freed = report.bytes_freed.saturating_add(deletion.size_bytes);
            }
            Err(err) => report
                .errors
                .push(format!("删除 {} 失败: {err}", deletion.relative_path)),
        }
    }
    Ok(report)
}

fn is_safe_relative(path: &Path) -> bool {
    !path.is_absolute()
        && !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    const NOW: u64 = 1_000_000_000;
    const DAY: u64 = SECS_PER_DAY;

    fn segment(id: &str, relative_path: &str, size: u64, age_days: u64) -> SegmentUsage {
        SegmentUsage {
            segment_id: id.to_owned(),
            relative_path: relative_path.to_owned(),
            size_bytes: size,
            last_modified_secs: NOW - age_days * DAY,
        }
    }

    #[test]
    fn defaults_match_session_log_spec() {
        let policy = RetentionPolicy::default();
        assert_eq!(policy.max_session_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(policy.max_age_days, 90);
        assert_eq!(policy.keep_recent_segments, 1);
    }

    #[test]
    fn age_based_plan_keeps_newest_segment() {
        let session = SessionUsage::new(
            "s-1",
            vec![
                segment("seg-1", "s-1/seg-1.jsonl", 100, 100),
                segment("seg-2", "s-1/seg-2.jsonl", 200, 50),
                segment("seg-3", "s-1/seg-3.jsonl", 300, 1),
            ],
            0,
        );
        let policy = RetentionPolicy::default();
        let plan = plan_retention(&[session], &policy, NOW);

        assert_eq!(plan.deletions.len(), 1, "{plan:?}");
        let deletion = &plan.deletions[0];
        assert_eq!(deletion.segment_id, "seg-1");
        assert_eq!(deletion.reason, DeleteReason::Age);
        assert_eq!(plan.bytes_reclaimed(), 100);
        assert_eq!(plan.bytes_before, 600);
        assert_eq!(plan.bytes_after, 500);
        assert!(plan.over_cap.is_empty());
    }

    #[test]
    fn over_cap_deletes_oldest_until_under_limit() {
        let session = SessionUsage::new(
            "s-cap",
            vec![
                segment("seg-1", "s-cap/seg-1.jsonl", 400, 5),
                segment("seg-2", "s-cap/seg-2.jsonl", 400, 4),
                segment("seg-3", "s-cap/seg-3.jsonl", 400, 3),
                segment("seg-4", "s-cap/seg-4.jsonl", 400, 1),
            ],
            0,
        );
        let policy = RetentionPolicy::new(1_000, 90, 1);
        let plan = plan_retention(&[session], &policy, NOW);

        // 总 1600，上限 1000：从最旧删起，删 2 段后剩 800。
        assert_eq!(plan.deletions.len(), 2, "{plan:?}");
        assert_eq!(plan.deletions[0].segment_id, "seg-1");
        assert_eq!(plan.deletions[1].segment_id, "seg-2");
        assert!(plan
            .deletions
            .iter()
            .all(|deletion| deletion.reason == DeleteReason::OverCap));
        assert_eq!(plan.bytes_after, 800);
        assert!(plan.over_cap.is_empty());
    }

    #[test]
    fn protected_newest_is_never_deleted_and_unreachable_cap_is_reported() {
        let session = SessionUsage::new(
            "s-attach",
            vec![
                segment("seg-1", "s-attach/seg-1.jsonl", 100, 10),
                segment("seg-2", "s-attach/seg-2.jsonl", 100, 5),
            ],
            // 附件 500 字节计入上限但本模块不删：删光段后仍超上限。
            500,
        );
        let policy = RetentionPolicy::new(100, 90, 1);
        let plan = plan_retention(&[session], &policy, NOW);

        assert_eq!(plan.deletions.len(), 1, "{plan:?}");
        assert_eq!(plan.deletions[0].segment_id, "seg-1");
        assert_eq!(plan.over_cap.len(), 1);
        assert_eq!(plan.over_cap[0].session_id, "s-attach");
        assert_eq!(plan.over_cap[0].bytes_after_deletions, 600);
        assert_eq!(plan.bytes_after, 600);
    }

    #[test]
    fn plan_is_deterministic_and_does_not_touch_disk() {
        let temp = TempDir::new("plan-pure");
        temp.write("s-1/seg-1.jsonl", b"old");
        temp.write("s-1/seg-2.jsonl", b"new");

        let sessions = vec![SessionUsage::new(
            "s-1",
            vec![
                segment("seg-1", "s-1/seg-1.jsonl", 3, 120),
                segment("seg-2", "s-1/seg-2.jsonl", 3, 1),
            ],
            0,
        )];
        let policy = RetentionPolicy::default();
        let first = plan_retention(&sessions, &policy, NOW);
        let second = plan_retention(&sessions, &policy, NOW);
        assert_eq!(first, second);
        assert!(!first.is_empty());
        // plan 不删文件。
        assert!(temp.join("s-1/seg-1.jsonl").is_file());
        assert!(temp.join("s-1/seg-2.jsonl").is_file());
    }

    #[test]
    fn apply_deletes_only_planned_files() {
        let temp = TempDir::new("apply");
        temp.write("s-2/seg-1.jsonl", &[b'x'; 10]);
        temp.write("s-2/seg-2.jsonl", &[b'y'; 20]);

        let sessions = vec![SessionUsage::new(
            "s-2",
            vec![
                segment("seg-1", "s-2/seg-1.jsonl", 10, 100),
                segment("seg-2", "s-2/seg-2.jsonl", 20, 1),
            ],
            0,
        )];
        let plan = plan_retention(&sessions, &RetentionPolicy::default(), NOW);
        assert_eq!(plan.deletions.len(), 1);
        assert_eq!(plan.deletions[0].reason, DeleteReason::Age);

        let report = apply_retention(temp.path(), &plan).expect("apply");
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(report.deleted_files, 1);
        assert_eq!(report.bytes_freed, 10);
        assert!(!temp.join("s-2/seg-1.jsonl").exists(), "计划内文件应被删除");
        assert!(temp.join("s-2/seg-2.jsonl").is_file(), "未计划文件必须保留");
    }

    #[test]
    fn apply_rejects_path_escape() {
        let temp = TempDir::new("escape");
        let outside = temp.write("outside.txt", b"keep me");
        temp.write("data/s-1/seg-1.jsonl", b"x");

        let plan = RetentionPlan {
            deletions: vec![SegmentDeletion {
                session_id: "s-1".to_owned(),
                segment_id: "seg-1".to_owned(),
                relative_path: "../outside.txt".to_owned(),
                size_bytes: 7,
                reason: DeleteReason::Age,
            }],
            over_cap: Vec::new(),
            bytes_before: 7,
            bytes_after: 0,
        };
        let report = apply_retention(temp.join("data").as_path(), &plan).expect("apply");
        assert!(!report.is_clean());
        assert_eq!(report.deleted_files, 0);
        assert!(outside.is_file(), "root 外的文件绝不能被删除");

        let err = apply_retention(&temp.join("does-not-exist"), &plan).expect_err("bad root");
        assert!(matches!(err, RetentionError::InvalidRoot(_)));
    }

    #[test]
    fn same_age_segments_are_ordered_by_segment_id() {
        let session = SessionUsage::new(
            "s-tie",
            vec![
                segment("seg-b", "s-tie/seg-b.jsonl", 100, 100),
                segment("seg-a", "s-tie/seg-a.jsonl", 100, 100),
                segment("seg-c", "s-tie/seg-c.jsonl", 100, 1),
            ],
            0,
        );
        let plan = plan_retention(&[session], &RetentionPolicy::default(), NOW);
        assert_eq!(plan.deletions.len(), 2, "{plan:?}");
        assert_eq!(plan.deletions[0].segment_id, "seg-a");
        assert_eq!(plan.deletions[1].segment_id, "seg-b");
    }
}
