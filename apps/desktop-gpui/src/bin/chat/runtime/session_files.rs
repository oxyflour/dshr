use crate::workspace_root;
use dshr_session_store::{
    quarantine_corrupt, recover_backup, save_atomic, try_lock_session, SessionCreated, SessionLog,
};
use std::path::{Path, PathBuf};

pub(crate) fn now_ts() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("t{nanos}")
}

/// 上下文外部化对象目录：默认在会话目录旁的 `spill/`，可用 `DSHR_SPILL_DIR` 覆盖。
pub(crate) fn spill_dir_for(session_path: &std::path::Path) -> PathBuf {
    spill_dir_in(
        session_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".tmp")),
    )
}

pub(crate) fn spill_dir_in(sessions_dir: &std::path::Path) -> PathBuf {
    if let Ok(dir) = std::env::var("DSHR_SPILL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    sessions_dir.join("spill")
}

/// 自检重置会话夹具：主文件 + `.bak` + `.corrupt-*` 全部清掉。
///
/// 只删主文件的话，`recover_backup()` 会在下一次自检启动时把上次的备份恢复回来，
/// 造成“新会话里混入旧事件”的测试污染（P1-1 恢复机制的副作用）。
pub(crate) fn reset_session_file(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}.bak", path.display()));
    if let (Some(parent), Some(name)) = (
        path.parent(),
        path.file_name().and_then(|name| name.to_str()),
    ) {
        let prefix = format!("{name}.corrupt-");
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

pub(crate) struct OpenedSessionLog {
    pub(crate) log: SessionLog,
    pub(crate) errors: Vec<String>,
    pub(crate) save_blocked: Option<String>,
    pub(crate) wrote_created: bool,
    pub(crate) recovered: bool,
}

pub(crate) fn open_session_log(path: &std::path::Path, session_id: &str) -> OpenedSessionLog {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let _writer = match std::fs::create_dir_all(parent).and_then(|_| try_lock_session(path)) {
        Ok(writer) => writer,
        Err(error) => {
            let message = format!("无法取得会话写入权：{error}");
            let log = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| SessionLog::from_jsonl(session_id, &text).ok())
                .unwrap_or_else(|| SessionLog::new(session_id));
            return OpenedSessionLog {
                log,
                errors: vec![message.clone()],
                save_blocked: Some(message),
                wrote_created: false,
                recovered: false,
            };
        }
    };
    let mut errors = Vec::new();
    let mut recovered = false;
    // 上次保存若在 Windows remove→rename 窗口崩溃，主文件会缺失而 .bak 完整：
    // 先尝试恢复，避免把可恢复的历史当成“新会话”重写。
    if !path.exists() {
        match recover_backup(path) {
            Ok(true) => {
                recovered = true;
                eprintln!("[chat] 已从备份恢复会话日志 {}", path.display());
                errors.push(format!(
                    "已恢复：会话从备份恢复（{}），可继续使用",
                    path.display()
                ));
            }
            Ok(false) => {}
            Err(err) => errors.push(format!("会话备份恢复失败：{err}")),
        }
    }
    let mut save_blocked: Option<String> = None;
    let mut log = if path.exists() {
        match std::fs::read_to_string(path) {
            Ok(text) => match SessionLog::from_jsonl(session_id, &text) {
                Ok(log) => log,
                // 损坏：隔离原文件（字节保留供导出排查），绝不覆盖。
                Err(err) => {
                    match quarantine_corrupt(path) {
                        Ok(quarantined) => {
                            let message = format!(
                                "会话日志损坏，已隔离为 {}（原文件保留，可导出排查）: {err}",
                                quarantined.display()
                            );
                            eprintln!("[chat] {message}");
                            errors.push(message);
                        }
                        Err(quarantine_err) => {
                            let message = format!(
                                "会话日志损坏且隔离失败（为避免覆盖，已停止写入）: {err}; {quarantine_err}"
                            );
                            eprintln!("[chat] {message}");
                            errors.push(message);
                            save_blocked = Some(String::from("会话日志损坏且无法隔离"));
                        }
                    }
                    SessionLog::new(session_id)
                }
            },
            Err(err) => {
                // 文件存在但读失败（锁定/权限）：绝不能当空会话重写。
                let message = format!("读取会话日志失败（为避免覆盖，已停止写入）: {err}");
                eprintln!("[chat] {message}");
                errors.push(message);
                save_blocked = Some(format!("读取会话日志失败: {err}"));
                SessionLog::new(session_id)
            }
        }
    } else {
        SessionLog::new(session_id)
    };
    // 新会话：把 `session.created` 作为第一条事件（父链/来源/工作区的权威来源）。
    let mut wrote_created = false;
    if log.is_empty() {
        let workspace = workspace_root().display().to_string();
        match log.append_created(now_ts(), &SessionCreated::root(workspace)) {
            Ok(_) => wrote_created = true,
            Err(err) => eprintln!("[chat] 写入 session.created 失败: {err}"),
        }
    }

    if wrote_created && save_blocked.is_none() {
        if let Err(error) = save_atomic(path, log.to_jsonl().as_bytes()) {
            let message = format!("保存会话失败：{error}");
            errors.push(message.clone());
            save_blocked = Some(message);
        }
    }
    OpenedSessionLog {
        log,
        errors,
        save_blocked,
        wrote_created,
        recovered,
    }
}
/// 旧日志可能包含别的 ID 格式。只恢复此处生成的消息计数。
pub(crate) fn last_message_seq(log: &SessionLog) -> u64 {
    log.events()
        .iter()
        .filter_map(|event| {
            let id = event.payload.get("message_id")?.as_str()?;
            let seq = id
                .strip_prefix("m-u")
                .or_else(|| id.strip_prefix("m-a"))
                .or_else(|| id.strip_prefix("m-sum"))?;
            seq.parse::<u64>().ok()
        })
        .max()
        .unwrap_or(0)
}

pub(crate) fn session_workspace(log: &SessionLog) -> PathBuf {
    if let Some(workspace) = log
        .created()
        .map(|created| created.workspace_id.trim())
        .filter(|workspace| !workspace.is_empty())
    {
        let path = Path::new(workspace);
        if path.is_absolute() {
            return path.to_path_buf();
        }
        let path = workspace_root().join(path);
        return std::path::absolute(&path).unwrap_or(path);
    }
    workspace_root()
}
