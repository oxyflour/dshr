use crate::*;

pub(crate) fn sessions_dir_from_env() -> PathBuf {
    std::env::var("DSHR_SESSIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".tmp/sessions"))
}

/// `dshr --export-session <id> [--export-dir <dir>]`：
/// 导出 JSONL + 外部化附件 + manifest（sha256），并立刻自校验。
pub(crate) fn export_session_cli(id: &str, out: Option<PathBuf>) -> i32 {
    let store = SessionIndex::new(sessions_dir_from_env());
    let path = store.path_for(id);
    if !path.exists() {
        eprintln!("[export] 会话不存在: {}", path.display());
        return 1;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("[export] 读取会话失败: {err}");
            return 1;
        }
    };
    let log = match SessionLog::from_jsonl(id, &text) {
        Ok(log) => log,
        Err(err) => {
            eprintln!("[export] 会话日志损坏: {err}");
            return 1;
        }
    };
    let spill = SpillStore::new(spill_dir_for(&path));
    let out_dir = out.unwrap_or_else(|| PathBuf::from(".tmp/exports").join(id));
    let manifest = match export_session(
        &log,
        &SpillStoreSource::new(&spill),
        &out_dir,
        &ExportOptions::new(now_ts()),
    ) {
        Ok(manifest) => manifest,
        Err(err) => {
            eprintln!("[export] 导出失败: {err}");
            return 1;
        }
    };
    println!(
        "[export] session={} events={} attachments={} missing={} bytes={} out={}",
        manifest.session_id,
        manifest.event_count,
        manifest.attachments.len(),
        manifest.missing_blobs.len(),
        manifest.total_bytes,
        out_dir.display()
    );
    match verify_export(&out_dir, id) {
        Ok(report) if report.is_ok() => {
            println!(
                "[export] verify=ok jsonl_ok={} attachments_checked={}",
                report.jsonl_ok, report.attachments_checked
            );
            0
        }
        Ok(report) => {
            eprintln!("[export] 自校验失败: {:?}", report.problems);
            1
        }
        Err(err) => {
            eprintln!("[export] 自校验失败: {err}");
            1
        }
    }
}

/// `dshr --retention-plan`：只计算、不删除（每个会话一个 JSONL 段）。
pub(crate) fn retention_plan_cli() -> i32 {
    use dshr_session_export::{plan_retention, RetentionPolicy, SegmentUsage, SessionUsage};
    let store = SessionIndex::new(sessions_dir_from_env());
    let metas = match store.scan() {
        Ok(metas) => metas,
        Err(err) => {
            eprintln!("[retention] 扫描会话失败: {err}");
            return 1;
        }
    };
    let usages: Vec<SessionUsage> = metas
        .iter()
        .map(|meta| {
            let size = std::fs::metadata(&meta.path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            SessionUsage::new(
                meta.id.clone(),
                vec![SegmentUsage {
                    segment_id: String::from("main"),
                    relative_path: meta
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    size_bytes: size,
                    last_modified_secs: meta.updated_at,
                }],
                0,
            )
        })
        .collect();
    let policy = RetentionPolicy::new(2 * 1024 * 1024 * 1024, 90, 1);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let plan = plan_retention(&usages, &policy, now);
    println!(
        "[retention] sessions={} bytes_before={} bytes_after={} deletions={} over_cap={} reclaimed={}",
        usages.len(),
        plan.bytes_before,
        plan.bytes_after,
        plan.deletions.len(),
        plan.over_cap.len(),
        plan.bytes_reclaimed()
    );
    for deletion in &plan.deletions {
        println!(
            "[retention] delete session={} segment={} size={} reason={:?}",
            deletion.session_id, deletion.segment_id, deletion.size_bytes, deletion.reason
        );
    }
    // 只出计划，不调用 apply_retention：删除必须由用户显式触发。
    println!("[retention] plan-only（未删除任何文件）");
    0
}
