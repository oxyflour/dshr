#![cfg(test)]

use super::*;
use crate::EventSpec;

fn temp_index(name: &str) -> SessionIndex {
    let dir = std::env::temp_dir().join(format!(
        "dshr-sessions-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    SessionIndex::new(dir)
}

fn seed(index: &SessionIndex, id: &str, lines: &[&str]) {
    index.ensure_dir().unwrap();
    fs::write(index.path_for(id), lines.join("\n") + "\n").unwrap();
}

fn user_event(text: &str) -> String {
    serde_json::json!({
        "session_id": "ignored-by-index",
        "seq": 1,
        "type": "message.user",
        "durability": "durable",
        "ts": "t0",
        "actor": { "kind": "user" },
        "trace_id": "t-local",
        "schema_version": 1,
        "payload": { "message_id": "m-1", "text": text }
    })
    .to_string()
}

#[test]
fn scan_derives_title_and_counts_events() {
    let index = temp_index("derive");
    seed(&index, "s-1", &[&user_event("帮我修一下窗口尺寸"), "{}"]);
    let sessions = index.scan().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "s-1");
    assert_eq!(sessions[0].title, "帮我修一下窗口尺寸");
    assert_eq!(sessions[0].events, 2);
    assert_eq!(sessions[0].workspace_id, None);
    fs::remove_dir_all(index.dir()).ok();
}

#[test]
fn scan_ignores_missing_dir() {
    let index = temp_index("missing");
    assert!(index.scan().unwrap().is_empty());
}

#[test]
fn rename_overrides_derived_title() {
    let index = temp_index("rename");
    seed(&index, "s-1", &[&user_event("原始标题")]);
    index.rename("s-1", "  自定义标题  ").unwrap();
    let sessions = index.scan().unwrap();
    assert_eq!(sessions[0].title, "自定义标题");
    // 事件文件保持 append-only，不受 rename 影响
    assert!(fs::read_to_string(index.path_for("s-1"))
        .unwrap()
        .contains("原始标题"));
    fs::remove_dir_all(index.dir()).ok();
}

#[test]
fn archive_and_rename_preserve_each_other_and_the_log() {
    let index = temp_index("archive");
    seed(&index, "s-1", &[&user_event("原始标题")]);
    let path = index.path_for("s-1");
    let original = fs::read(&path).unwrap();
    assert!(!index.scan().unwrap()[0].archived);
    index.rename("s-1", "原有自定义标题").unwrap();
    let writer = try_lock_session(&path).unwrap();
    assert_eq!(
        index.set_archived("s-1", true).unwrap_err().kind(),
        io::ErrorKind::WouldBlock,
    );
    drop(writer);
    index.set_archived("s-1", true).unwrap();
    let archived = SessionIndex::new(index.dir()).scan().unwrap().remove(0);
    assert!(archived.archived);
    assert_eq!(archived.title, "原有自定义标题");
    index.rename("s-1", "改名后仍保留归档").unwrap();
    assert!(index.scan().unwrap()[0].archived);
    index.set_archived("s-1", false).unwrap();
    let restored = index.scan().unwrap().remove(0);
    assert!(!restored.archived);
    assert_eq!(restored.title, "改名后仍保留归档");
    assert_eq!(fs::read(&path).unwrap(), original);
    index.delete("s-1").unwrap();
    assert!(index.scan().unwrap().is_empty());
    fs::remove_dir_all(index.dir()).ok();
}

#[test]
fn create_and_delete_session() {
    let index = temp_index("create");
    let meta = index.create("s-new").unwrap();
    assert_eq!(meta.id, "s-new");
    assert!(index.path_for("s-new").exists());
    assert_eq!(index.scan().unwrap().len(), 1);
    index.delete("s-new").unwrap();
    assert!(index.scan().unwrap().is_empty());
    fs::remove_dir_all(index.dir()).ok();
}

#[test]
fn scan_surfaces_orphan_backup_when_main_is_missing() {
    let index = temp_index("orphan-backup");
    seed(&index, "s-1", &[&user_event("before crash")]);
    let main = index.path_for("s-1");
    let backup = backup_path_for(&main);
    fs::copy(&main, &backup).unwrap();
    fs::remove_file(&main).unwrap();

    let sessions = index.scan().unwrap();
    assert_eq!(sessions.len(), 1, "孤儿 .bak 必须作为可恢复会话出现");
    assert_eq!(sessions[0].id, "s-1");
    assert_eq!(sessions[0].title, "before crash");
    assert_eq!(sessions[0].path, backup, "主文件缺失时从 .bak 读取");
    assert!(!main.exists(), "scan 只暴露候选，不负责恢复主文件");
    fs::remove_dir_all(index.dir()).ok();
}

#[test]
fn scan_ignores_backup_when_main_exists() {
    let index = temp_index("backup-with-main");
    seed(&index, "s-1", &[&user_event("current")]);
    fs::copy(
        index.path_for("s-1"),
        backup_path_for(&index.path_for("s-1")),
    )
    .unwrap();

    let sessions = index.scan().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].path, index.path_for("s-1"), "主文件优先于 .bak");
    assert_eq!(sessions[0].title, "current");
    fs::remove_dir_all(index.dir()).ok();
}

#[test]
fn delete_removes_backup_so_a_session_does_not_resurrect() {
    let index = temp_index("delete-backup");
    seed(&index, "s-1", &[&user_event("delete me")]);
    let main = index.path_for("s-1");
    let backup = backup_path_for(&main);
    fs::copy(&main, &backup).unwrap();

    index.delete("s-1").unwrap();

    assert!(!main.exists());
    assert!(!backup.exists(), "删除会话必须同时清掉 .bak");
    assert!(
        index.scan().unwrap().is_empty(),
        "删除后不能让会话从备份复活"
    );
    fs::remove_dir_all(index.dir()).ok();
}

#[test]
fn new_ids_do_not_collide_with_existing_files() {
    let index = temp_index("ids");
    let first = index.new_id();
    index.create(&first).unwrap();
    let second = index.new_id();
    assert_ne!(first, second);
    assert!(!index.path_for(&second).exists());
    fs::remove_dir_all(index.dir()).ok();
}

#[test]
fn scan_sorts_newest_first() {
    let index = temp_index("sort");
    seed(&index, "s-old", &[&user_event("old")]);
    // modified_at 以秒为单位；必须跨过一秒，否则 CI 上同一秒内排序不稳定。
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    seed(&index, "s-new", &[&user_event("new")]);
    let sessions = index.scan().unwrap();
    assert_eq!(sessions[0].id, "s-new");
    fs::remove_dir_all(index.dir()).ok();
}

// ------------------------------------------------------------------
// 会话树（PLAN §6.5）
// ------------------------------------------------------------------

fn seed_log(index: &SessionIndex, log: &SessionLog) {
    index.ensure_dir().unwrap();
    fs::write(index.path_for(log.session_id()), log.to_jsonl()).unwrap();
}

fn root_log(id: &str, user_text: &str) -> SessionLog {
    let mut log = SessionLog::new(id);
    log.append_created("t0", &SessionCreated::root("ws-1"))
        .unwrap();
    log.append(EventSpec::durable(
        "message.user",
        "t0",
        serde_json::json!({ "message_id": "m-1", "text": user_text }),
    ))
    .unwrap();
    log
}

#[test]
fn scan_exposes_parent_and_spawn_source_from_session_created() {
    let index = temp_index("tree-fields");
    seed_log(&index, &root_log("s-root", "根会话"));
    index
        .create_child("s-child", "s-root", "ws-1", "t0")
        .unwrap();

    let sessions = index.scan().unwrap();
    let root = sessions.iter().find(|meta| meta.id == "s-root").unwrap();
    let child = sessions.iter().find(|meta| meta.id == "s-child").unwrap();
    assert_eq!(root.parent_session_id, None);
    assert_eq!(root.spawn_source, None);
    assert!(!root.is_child());
    assert_eq!(child.parent_session_id.as_deref(), Some("s-root"));
    assert_eq!(child.spawn_source, Some(SpawnSource::Agent));
    assert!(child.is_child());
    fs::remove_dir_all(index.dir()).ok();
}

#[test]
fn legacy_session_without_created_event_is_still_a_root() {
    let index = temp_index("legacy");
    seed(&index, "s-legacy", &[&user_event("旧会话")]);
    let sessions = index.scan().unwrap();
    assert_eq!(sessions[0].parent_session_id, None);
    assert_eq!(sessions[0].spawn_source, None);
    fs::remove_dir_all(index.dir()).ok();
}

#[test]
fn create_child_writes_created_event_and_tree_queries_work() {
    let index = temp_index("tree-create");
    let root = index.create_root("s-root", "ws-1", "t0").unwrap();
    assert_eq!(root.workspace_id.as_deref(), Some("ws-1"));
    let child = index
        .create_child("s-child", "s-root", "ws-1", "t1")
        .unwrap();
    assert_eq!(child.workspace_id.as_deref(), Some("ws-1"));
    index
        .create_child("s-grand", "s-child", "ws-1", "t2")
        .unwrap();
    assert!(index
        .scan()
        .unwrap()
        .iter()
        .all(|session| session.workspace_id.as_deref() == Some("ws-1")));

    // 子会话的日志第一条事件必须是 session.created，且父链接可回放。
    let child_text = fs::read_to_string(index.path_for("s-child")).unwrap();
    let child_log = SessionLog::from_jsonl("s-child", &child_text).unwrap();
    assert_eq!(child_log.parent_session_id(), Some("s-root"));

    assert_eq!(
        index
            .children_of("s-root")
            .unwrap()
            .into_iter()
            .map(|meta| meta.id)
            .collect::<Vec<_>>(),
        vec!["s-child".to_owned()]
    );
    assert_eq!(
        index
            .descendants_of("s-root")
            .unwrap()
            .into_iter()
            .map(|meta| meta.id)
            .collect::<Vec<_>>(),
        vec!["s-child".to_owned(), "s-grand".to_owned()]
    );
    assert_eq!(index.depth_of("s-root").unwrap(), Some(0));
    assert_eq!(index.depth_of("s-grand").unwrap(), Some(2));
    assert_eq!(
        index
            .root_sessions()
            .unwrap()
            .into_iter()
            .map(|meta| meta.id)
            .collect::<Vec<_>>(),
        vec!["s-root".to_owned()]
    );
    let tree = index.tree().unwrap();
    assert_eq!(tree.len(), 3);
    assert_eq!(tree.max_depth(), 2);

    // 已存在的会话不能被覆盖。
    let err = index.create_root("s-root", "ws-1", "t3").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    fs::remove_dir_all(index.dir()).ok();
}

#[test]
fn write_log_round_trips_a_parent_linked_child_log() {
    let index = temp_index("tree-write");
    let mut log = SessionLog::new("s-child");
    log.append_created(
        "t0",
        &SessionCreated::child("ws-1", "s-root", SpawnSource::Agent),
    )
    .unwrap();
    log.append(EventSpec::durable(
        "turn.started",
        "t0",
        serde_json::json!({}),
    ))
    .unwrap();

    let path = index.write_log(&log).unwrap();
    assert_eq!(path, index.path_for("s-child"));
    let restored = SessionLog::from_jsonl("s-child", &fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(restored.parent_session_id(), Some("s-root"));
    assert_eq!(restored.len(), 2);
    fs::remove_dir_all(index.dir()).ok();
}

// ------------------------------------------------------------------
// 崩溃安全落盘（M2 review P1-1）
// ------------------------------------------------------------------

/// 测试用临时目录：路径 = `std::env::temp_dir()/dshr-session-store-<label>-<pid>-<纳秒>`，
/// `Drop` 时递归清理（不引入 tempfile 依赖）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "dshr-session-store-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("创建测试临时目录");
        Self { path }
    }

    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    fn entries(&self) -> Vec<String> {
        fs::read_dir(&self.path)
            .expect("读取测试临时目录")
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .collect()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).ok();
    }
}

#[test]
fn save_atomic_creates_missing_parents_and_writes_bytes() {
    let dir = TempDir::new("atomic-create");
    let path = dir.path.join("nested").join("deeper").join("s-1.jsonl");

    save_atomic(&path, b"first").unwrap();

    assert_eq!(fs::read(&path).unwrap(), b"first".to_vec());
    assert!(
        !backup_path_for(&path).exists(),
        "首次写入没有旧版本，不应生成 .bak"
    );
    assert!(
        dir.entries().iter().all(|name| !name.contains(".tmp-")),
        "成功路径不应残留临时文件: {:?}",
        dir.entries()
    );
}

#[test]
fn save_atomic_overwrite_keeps_previous_version_in_backup() {
    let dir = TempDir::new("atomic-overwrite");
    let path = dir.join("s-1.jsonl");

    save_atomic(&path, b"v1").unwrap();
    save_atomic(&path, b"v2").unwrap();

    assert_eq!(fs::read(&path).unwrap(), b"v2".to_vec());
    assert_eq!(
        fs::read(backup_path_for(&path)).unwrap(),
        b"v1".to_vec(),
        ".bak 必须保留上一版内容"
    );

    save_atomic(&path, b"v3").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"v3".to_vec());
    assert_eq!(
        fs::read(backup_path_for(&path)).unwrap(),
        b"v2".to_vec(),
        ".bak 每次保存刷新为紧邻的上一版"
    );
    assert!(
        dir.entries().iter().all(|name| !name.contains(".tmp-")),
        "成功路径不应残留临时文件: {:?}",
        dir.entries()
    );
}

#[test]
fn quarantine_corrupt_moves_file_and_preserves_exact_bytes() {
    let dir = TempDir::new("quarantine");
    let path = dir.join("s-1.jsonl");
    // 含非法 UTF-8 / 非法 JSON 的字节序列：隔离必须原字节搬运，不做解码。
    let corrupt: Vec<u8> = (0u8..=255).rev().take(97).collect();
    fs::write(&path, &corrupt).unwrap();

    let moved = quarantine_corrupt(&path).unwrap();

    assert!(!path.exists(), "隔离后原路径必须消失，防止后续保存覆盖");
    assert_eq!(fs::read(&moved).unwrap(), corrupt, "隔离文件必须原字节一致");
    let name = moved
        .file_name()
        .and_then(|name| name.to_str())
        .expect("隔离文件名可读");
    assert!(
        name.starts_with("s-1.jsonl.corrupt-"),
        "隔离名应为 <path>.corrupt-<秒>，实际: {name}"
    );

    // 二次隔离：同一秒内也不能覆盖上一个隔离文件。
    fs::write(&path, b"second-corrupt").unwrap();
    let moved_again = quarantine_corrupt(&path).unwrap();
    assert_ne!(moved, moved_again);
    assert!(moved.exists() && moved_again.exists());
    assert_eq!(fs::read(&moved_again).unwrap(), b"second-corrupt".to_vec());
    assert_eq!(fs::read(&moved).unwrap(), corrupt);
}

#[test]
fn recover_backup_only_restores_when_target_is_missing() {
    let dir = TempDir::new("recover");
    let path = dir.join("s-1.jsonl");
    let backup = backup_path_for(&path);

    // 目标与备份都不存在：什么都不做。
    assert!(!recover_backup(&path).unwrap());
    assert!(!path.exists());

    // 只有备份（模拟 Windows 替换窗口崩溃）：原子恢复，备份保留供取证。
    fs::write(&backup, b"backup-content").unwrap();
    assert!(recover_backup(&path).unwrap());
    assert_eq!(fs::read(&path).unwrap(), b"backup-content".to_vec());
    assert!(backup.exists(), ".bak 恢复后应保留，方便用户再次取证");

    // 目标存在：绝不覆盖（哪怕备份内容不同）。
    fs::write(&path, b"current-content").unwrap();
    assert!(!recover_backup(&path).unwrap());
    assert_eq!(fs::read(&path).unwrap(), b"current-content".to_vec());

    // 目标缺失且无备份：返回 false，不报错。
    fs::remove_file(&path).unwrap();
    fs::remove_file(&backup).unwrap();
    assert!(!recover_backup(&path).unwrap());
}

#[test]
fn save_atomic_returns_err_when_parent_cannot_be_created() {
    let dir = TempDir::new("atomic-bad-parent");
    let blocker = dir.join("blocker");
    fs::write(&blocker, b"i am a regular file").unwrap();
    // 父路径是普通文件：create_dir_all 必然失败 → 返回 Err 且不 panic。
    let path = blocker.join("s-1.jsonl");

    let err = save_atomic(&path, b"new").expect_err("父目录不可创建时必须返回 Err");

    assert!(!err.to_string().is_empty());
    assert!(!path.exists());
    assert_eq!(
        fs::read(&blocker).unwrap(),
        b"i am a regular file".to_vec(),
        "失败路径不能破坏已有文件"
    );
}

#[test]
fn save_atomic_cleans_temp_when_replace_fails() {
    let dir = TempDir::new("atomic-replace-fail");
    let path = dir.join("s-1.jsonl");
    // 目标位置被同名目录占用：临时文件写入成功，但替换必然失败。
    fs::create_dir(&path).unwrap();

    let err = save_atomic(&path, b"new").expect_err("目标被目录占用时替换必须失败");

    assert!(!err.to_string().is_empty());
    assert!(path.is_dir(), "失败时不能破坏原目标");
    let leftovers: Vec<String> = dir
        .entries()
        .into_iter()
        .filter(|name| name.contains(".tmp-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "失败必须清理临时文件，残留: {leftovers:?}"
    );
}

#[test]
fn cleanup_removes_only_stale_own_temp_files() {
    let dir = TempDir::new("temp-cleanup");
    let path = dir.join("s-1.jsonl");
    fs::write(&path, b"current").unwrap();
    let ours = dir.join(".s-1.jsonl.tmp-999-1");
    let foreign = dir.join("s-1.jsonl.tmp-999-1"); // 少一个点：不是本函数的命名
    let other_session = dir.join(".s-2.jsonl.tmp-999-1");
    fs::write(&ours, b"leftover").unwrap();
    fs::write(&foreign, b"not-ours").unwrap();
    fs::write(&other_session, b"not-ours").unwrap();
    std::thread::sleep(Duration::from_millis(5));

    // 年轻阈值：刚创建的文件可能属于并发写入者，不能删。
    assert_eq!(
        cleanup_stale_temps_older_than(&path, Duration::from_secs(3600)).unwrap(),
        0
    );
    assert!(ours.exists());

    // 年龄阈值为 0：只删本会话自己的遗留临时文件。
    assert_eq!(
        cleanup_stale_temps_older_than(&path, Duration::ZERO).unwrap(),
        1
    );
    assert!(!ours.exists());
    assert!(foreign.exists(), "不能删非本命名的文件");
    assert!(other_session.exists(), "不能删其它会话的临时文件");
}

/// Windows 文件名含 `:` 时 `File::create` 必然失败：验证错误传播 + 不 panic。
#[cfg(windows)]
#[test]
fn save_atomic_reports_invalid_windows_path_without_panicking() {
    let dir = TempDir::new("atomic-invalid-name");
    let path = dir.join("bad:name.jsonl");
    let err = save_atomic(&path, b"x").expect_err("非法 Windows 文件名必须返回 Err");
    assert!(!err.to_string().is_empty());
    assert!(
        dir.entries().iter().all(|name| !name.contains(".tmp-")),
        "失败时必须清理临时文件: {:?}",
        dir.entries()
    );
}

/// P1-1 回归：损坏日志 → 隔离 → 新会话落盘，历史字节必须仍在。
///
/// 旧行为（`chat.rs` 直接 `fs::write`）会把截断文件改写成 1 行新日志；这里锁定
/// 新流程：隔离文件保留**截断时的原始字节**，可导出取证，新文件是干净新会话。
#[test]
fn quarantine_then_save_never_destroys_history() {
    let dir = TempDir::new("p11-regression");
    let path = dir.join("s-1.jsonl");

    // 1) 正常写入一段历史（对应 review 里 5830B 的有效日志）。
    save_atomic(&path, root_log("s-1", "原始历史").to_jsonl().as_bytes()).unwrap();
    let original = fs::read(&path).unwrap();
    assert!(original.len() > 100);

    // 2) 模拟崩溃产生的截断文件：回放失败。
    let truncated = original[..original.len() / 2].to_vec();
    fs::write(&path, &truncated).unwrap();
    assert!(SessionLog::from_jsonl("s-1", &fs::read_to_string(&path).unwrap()).is_err());

    // 3) 隔离损坏文件，再用新空会话落盘（接线后的 chat.rs 流程）。
    let quarantined = quarantine_corrupt(&path).unwrap();
    assert_eq!(fs::read(&quarantined).unwrap(), truncated);
    let mut fresh = SessionLog::new("s-1");
    fresh
        .append_created("t1", &SessionCreated::root("ws-1"))
        .unwrap();
    save_atomic(&path, fresh.to_jsonl().as_bytes()).unwrap();

    // 4) 历史字节可取证；当前文件是干净的新会话。
    assert_eq!(fs::read(&quarantined).unwrap(), truncated);
    let reloaded = SessionLog::from_jsonl("s-1", &fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(reloaded.len(), 1);
    assert!(
        SessionLog::from_jsonl("s-1", &fs::read_to_string(&path).unwrap())
            .unwrap()
            .created()
            .is_some()
    );
}
