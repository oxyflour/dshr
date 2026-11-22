//! 会话索引：把目录里的 `*.jsonl` 映射成侧栏可用的元数据。
//!
//! 只做**目录级索引**，不解析事件模型（那是 `SessionLog` 的事）：
//! - 文件名的 stem 就是 `session_id`；
//! - 标题优先取 `.meta.json` 里用户重命名的值，否则取第一条 `message.user` 的文本；
//! - 更新时间用文件 mtime（UI 只显示相对时间，避免引入日期库）；
//! - 会话树的父链接/来源从第一条 `session.created` 事件读取（规范 §7），
//!   `*.meta.json` 不参与树结构，避免出现第二份真相。

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::session_tree::{SessionTree, TreeError};
use crate::{SessionCreated, SessionLog, SpawnSource};

/// 侧栏一行所需的会话元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    /// Local sidebar membership. Missing metadata means the session is active.
    pub archived: bool,
    /// Unix 秒；UI 负责格式化成"刚刚 / N 分钟前"。
    pub updated_at: u64,
    pub path: PathBuf,
    /// 事件条数（用于展示"多少条"）。
    pub events: usize,
    /// 来自 `session.created.workspace_id`；早期会话可能没有该事件。
    pub workspace_id: Option<String>,
    /// 来自 `session.created.parent_session_id`；根会话（或旧日志）为 None。
    pub parent_session_id: Option<String>,
    /// 来自 `session.created.spawn_source`；根会话（或旧日志）为 None。
    pub spawn_source: Option<SpawnSource>,
}

impl SessionMeta {
    pub fn is_child(&self) -> bool {
        self.parent_session_id.is_some()
    }
}

/// 会话目录索引。
#[derive(Debug, Clone)]
pub struct SessionIndex {
    dir: PathBuf,
}

impl SessionIndex {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn ensure_dir(&self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)
    }

    pub fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.meta.json"))
    }

    /// 扫描目录，按更新时间倒序。目录不存在时返回空列表（首次启动）。
    ///
    /// 若主文件缺失、只剩 `<id>.jsonl.bak`（Windows 替换窗口崩溃后的形态），
    /// 也把 `.bak` 作为该会话的读取来源返回；启动路径随后用
    /// [`recover_backup`] 把主文件恢复出来，用户不会因为一次崩溃丢掉整个会话。
    pub fn scan(&self) -> io::Result<Vec<SessionMeta>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };

        let mut sessions = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let extension = path.extension().and_then(|ext| ext.to_str());
            // 主文件缺失时，`.jsonl.bak` 是唯一可读的历史；把它当作会话候选，
            // 让启动路径能选中并 recover_backup，而不是凭空新建一个空会话。
            let (id, read_path) = match extension {
                Some("jsonl") => {
                    let Some(id) = path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    (id, path.clone())
                }
                Some("bak") => {
                    let Some(id) = path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .and_then(|stem| stem.strip_suffix(".jsonl"))
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    let main = self.path_for(&id);
                    if main.exists() {
                        continue;
                    }
                    (id, path.clone())
                }
                _ => continue,
            };
            let content = fs::read_to_string(&read_path).unwrap_or_default();
            let Summary {
                title,
                events,
                created,
            } = summarize(&content);
            let metadata = read_metadata(&self.meta_path(&id)).unwrap_or_default();
            let renamed = metadata.get("title").and_then(serde_json::Value::as_str);
            let parent_session_id = created
                .as_ref()
                .and_then(|created| created.parent_session_id.clone());
            let spawn_source = created.as_ref().and_then(|created| created.spawn_source);
            let workspace_id = created.map(|created| created.workspace_id);
            sessions.push(SessionMeta {
                title: renamed.map(str::to_owned).unwrap_or(title),
                archived: metadata
                    .get("archived")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true),
                id,
                updated_at: modified_at(&read_path),
                path: read_path,
                events,
                workspace_id,
                parent_session_id,
                spawn_source,
            });
        }
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
        Ok(sessions)
    }

    /// 创建一个空会话文件（已存在则复用）。这是 M1/M2 的旧路径，不写 `session.created`；
    /// 新会话请用 [`SessionIndex::create_root`] / [`SessionIndex::create_child`]。
    pub fn create(&self, id: &str) -> io::Result<SessionMeta> {
        self.ensure_dir()?;
        let path = self.path_for(id);
        let _writer = try_lock_session(&path)?;
        if !path.exists() {
            fs::write(&path, b"")?;
        }
        Ok(SessionMeta {
            id: id.to_owned(),
            title: String::from("新会话"),
            archived: false,
            updated_at: modified_at(&path),
            path,
            events: 0,
            workspace_id: None,
            parent_session_id: None,
            spawn_source: None,
        })
    }

    /// 创建根会话：写入 `session.created`（`spawn_source = user`，无父）。
    ///
    /// 与 [`SessionIndex::create`] 不同，它会建立可回放的会话元数据；已存在时返回
    /// `AlreadyExists`，不覆盖历史。
    pub fn create_root(&self, id: &str, workspace_id: &str, ts: &str) -> io::Result<SessionMeta> {
        self.create_with_created(id, &SessionCreated::root(workspace_id), ts)
    }

    /// 创建子会话：写入 `session.created`（父链接 + `spawn_source = agent`）。
    ///
    /// `workspace_id` 应由父会话的 `session.created.workspace_id` 继承而来；
    /// 子会话是**独立日志文件**，与父会话的 `seq` 空间互不共享。
    pub fn create_child(
        &self,
        id: &str,
        parent_session_id: &str,
        workspace_id: &str,
        ts: &str,
    ) -> io::Result<SessionMeta> {
        self.create_with_created(
            id,
            &SessionCreated::child(workspace_id, parent_session_id, SpawnSource::Agent),
            ts,
        )
    }

    /// 用给定 `session.created` 创建会话文件（通用入口）。
    pub fn create_with_created(
        &self,
        id: &str,
        created: &SessionCreated,
        ts: &str,
    ) -> io::Result<SessionMeta> {
        self.ensure_dir()?;
        let path = self.path_for(id);
        let _writer = try_lock_session(&path)?;
        if path.try_exists()? || backup_path_for(&path).try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("会话 `{id}` 已存在，拒绝覆盖事件日志"),
            ));
        }
        let mut log = SessionLog::new(id);
        log.append_created(ts, created)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        let jsonl = log.to_jsonl();
        save_atomic(&path, jsonl.as_bytes())?;
        let summary = summarize(&jsonl);
        Ok(SessionMeta {
            id: id.to_owned(),
            title: summary.title,
            archived: false,
            updated_at: modified_at(&path),
            path,
            events: summary.events,
            workspace_id: Some(created.workspace_id.clone()),
            parent_session_id: created.parent_session_id.clone(),
            spawn_source: created.spawn_source,
        })
    }

    /// 把一份 Event Log 整体写回会话文件（子会话由 `agent-core` 生成后落盘）。
    ///
    /// 注意：这是**全量重写**，只在创建/迁移场景使用；运行期追加仍走 `SessionLog::append`
    /// 的单向语义（I1 append-only 由提交路径保证）。
    pub fn write_log(&self, log: &SessionLog) -> io::Result<PathBuf> {
        self.ensure_dir()?;
        let path = self.path_for(log.session_id());
        // 原子落盘 + 保留 `.bak`：全量重写也必须可回滚（M2 review P1-1）。
        save_atomic(&path, log.to_jsonl().as_bytes())?;
        Ok(path)
    }

    // ------------------------------------------------------------------
    // 会话树查询（PLAN §6.5）：薄封装，纯计算在 `session_tree` 模块。
    // ------------------------------------------------------------------

    /// 扫描并构造会话树；成环/重复 ID 视为索引损坏。
    pub fn tree(&self) -> io::Result<SessionTree> {
        let metas = self.scan()?;
        SessionTree::from_metas(&metas).map_err(tree_io)
    }

    /// 根会话（含父已删除的孤儿）；同层按更新时间倒序。
    pub fn root_sessions(&self) -> io::Result<Vec<SessionMeta>> {
        self.tree_sessions(|tree| {
            tree.roots()
                .into_iter()
                .map(|node| node.id.clone())
                .collect()
        })
    }

    /// 直接子会话（同层按更新时间倒序）。
    pub fn children_of(&self, id: &str) -> io::Result<Vec<SessionMeta>> {
        self.tree_sessions(|tree| {
            tree.children(id)
                .into_iter()
                .map(|node| node.id.clone())
                .collect()
        })
    }

    /// 全部后代（层序，不含 `id` 自己）。
    pub fn descendants_of(&self, id: &str) -> io::Result<Vec<SessionMeta>> {
        self.tree_sessions(|tree| {
            tree.descendants(id)
                .into_iter()
                .map(|node| node.id.clone())
                .collect()
        })
    }

    /// 会话深度（根 0）；未知 id 返回 None。
    pub fn depth_of(&self, id: &str) -> io::Result<Option<u32>> {
        Ok(self.tree()?.depth(id))
    }

    /// 扫描目录后按树顺序取回元数据（一次扫描 + 一次建树）。
    fn tree_sessions(
        &self,
        pick: impl FnOnce(&SessionTree) -> Vec<String>,
    ) -> io::Result<Vec<SessionMeta>> {
        let metas = self.scan()?;
        let tree = SessionTree::from_metas(&metas).map_err(tree_io)?;
        let by_id: BTreeMap<&str, &SessionMeta> =
            metas.iter().map(|meta| (meta.id.as_str(), meta)).collect();
        Ok(pick(&tree)
            .into_iter()
            .filter_map(|id| by_id.get(id.as_str()).map(|meta| (*meta).clone()))
            .collect())
    }

    pub fn delete(&self, id: &str) -> io::Result<()> {
        let path = self.path_for(id);
        if !self.dir.try_exists()? {
            return Ok(());
        }
        let _writer = try_lock_session(&path)?;
        remove_if_exists(&path)?;
        // 只删主文件会留下 `.bak`，而 scan 会把孤儿 `.bak` 重新当成会话；
        // 删除必须连备份一起清掉，否则下一次启动会话“复活”。
        remove_if_exists(&backup_path_for(&path))?;
        remove_if_exists(&self.meta_path(id))
    }

    /// 重命名：写 sidecar meta，不改动 append-only 的事件文件。
    pub fn rename(&self, id: &str, title: &str) -> io::Result<()> {
        let title = title.trim();
        if title.is_empty() {
            return Ok(());
        }
        self.update_metadata(id, "title", title.into())
    }

    /// Move a session into or out of the desktop archive without rewriting its log.
    pub fn set_archived(&self, id: &str, archived: bool) -> io::Result<()> {
        self.update_metadata(id, "archived", archived.into())
    }

    fn update_metadata(&self, id: &str, key: &str, value: serde_json::Value) -> io::Result<()> {
        let session_path = self.path_for(id);
        let _writer = try_lock_session(&session_path)?;
        if !session_path.try_exists()? && !backup_path_for(&session_path).try_exists()? {
            return Err(io::ErrorKind::NotFound.into());
        }
        let path = self.meta_path(id);
        let mut metadata = read_metadata(&path)?;
        metadata.insert(key.to_owned(), value);
        let contents = serde_json::to_vec_pretty(&metadata).map_err(io::Error::other)?;
        let temporary = temp_path_for(&path)?;
        let result = write_synced(&temporary, &contents).and_then(|()| fs::rename(&temporary, &path));
        if result.is_err() {
            let _ = remove_if_exists(&temporary);
        }
        result
    }

    /// 生成不重复的新会话 ID（毫秒时间戳，足够本地使用）。
    pub fn new_id(&self) -> String {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let mut candidate = format!("s-{millis}");
        let mut suffix = 1;
        while self.path_for(&candidate).exists()
            || backup_path_for(&self.path_for(&candidate)).exists()
        {
            candidate = format!("s-{millis}-{suffix}");
            suffix += 1;
        }
        candidate
    }
}

/// Hold the returned handle across load, mutation, and commit. Keep the lock
/// file itself in place so all processes acquire the same underlying lock.
pub fn try_lock_session(path: &Path) -> io::Result<fs::File> {
    let writer = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(with_suffix(path, ".lock"))?;
    match writer.try_lock() {
        Ok(()) => Ok(writer),
        Err(std::fs::TryLockError::WouldBlock) => Err(io::ErrorKind::WouldBlock.into()),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

// ---------------------------------------------------------------------------
// 崩溃安全落盘原语（M2 review P1-1）
//
// 背景：`std::fs::write` 全量重写**不是原子操作**，断电/崩溃恰好发生在写入中间
// 会留下截断文件；而加载端若把损坏日志当成空会话、随后再全量保存，就会用一行新
// 文件覆盖掉历史（实测 5830B → 222B，见 `docs/review-m2-implementation.md` P1-1）。
//
// 这里提供三个原语，供 `chat.rs` 接线：
// - [`save_atomic`]：临时文件 + `sync_all` + rename，并保留上一版 `.bak`；
// - [`quarantine_corrupt`]：把损坏日志原字节隔离到 `.corrupt-<unix 秒>`，绝不覆盖；
// - [`recover_backup`]：仅在目标缺失时用 `.bak` 恢复（覆盖 Windows 替换窗口）。
//
// 约束：不引入新依赖、不使用 unsafe（crate 级 `#![forbid(unsafe_code)]`）。
// ---------------------------------------------------------------------------

/// 备份后缀：`<path>.bak` 保存**上一次**成功落盘的内容。
const BACKUP_SUFFIX: &str = ".bak";

/// 损坏文件隔离名的前缀：`<path>.corrupt-<unix 秒>`。
const CORRUPT_SUFFIX_PREFIX: &str = ".corrupt-";

/// 判定"陈旧临时文件"的年龄阈值；比它新的 `.tmp-*` 可能属于并发写入者，不能删。
const STALE_TEMP_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// 原子落盘：同目录临时文件 + `sync_all` + rename 替换，并保留上一版 `.bak`。
///
/// 步骤：
/// 1. 确保父目录存在（首次启动/新建子目录）；
/// 2. 写同目录临时文件 `.<file>.tmp-<pid>-<nanos>`（同目录保证 rename 不跨卷）；
/// 3. `sync_all`：数据真正落盘后才替换，避免崩溃后得到"空/半截"的新文件；
/// 4. 目标已存在时 best-effort 复制为 `<path>.bak`（保留上一版；复制失败不阻止保存，
///    因为 rename 本身不会把旧文件写坏）；
/// 5. 用临时文件替换目标；成功后清理陈旧的 `.tmp-*` 残留。任何一步失败都返回
///    `Err`（不 panic），并清理本次临时文件。
///
/// **Windows 替换窗口**：`std::fs::rename` 在 Windows 上底层是
/// `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)`，正常情况可覆盖已有目标；但它仍可能
/// 因目标被其它进程占用（杀毒/索引器/编辑器持有句柄）而失败。此时退化为
/// `remove_file(dest)` + `rename`：两步之间**不是严格原子替换**，若崩溃恰好落在窗口里，
/// 目标文件会短暂缺失——由 `<path>.bak` 兜底（启动时调用 [`recover_backup`]）。
/// 之所以不直接调 `ReplaceFileW`/`MoveFileExW`，是因为需要 unsafe FFI 且不允许新增依赖。
pub fn save_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    let tmp = temp_path_for(path)?;
    if let Err(err) = write_synced(&tmp, contents) {
        let _ = remove_if_exists(&tmp);
        return Err(err);
    }

    if path.exists() {
        // 备份是止损手段：失败不阻断保存（`rename` 不会破坏旧文件）。
        let _ = fs::copy(path, backup_path_for(path));
    }

    match replace(&tmp, path) {
        Ok(()) => {
            cleanup_stale_temps(path);
            Ok(())
        }
        Err(err) => {
            let _ = remove_if_exists(&tmp);
            Err(err)
        }
    }
}

/// 隔离损坏日志：把原文件**原字节**改名到同目录 `<path>.corrupt-<unix 秒>`。
///
/// - `rename` 不读写内容，损坏文件一个字节都不会变，方便用户事后导出/取证；
/// - 目标已存在（同一秒内重复隔离）时追加序号，**绝不覆盖**已有隔离文件；
/// - 返回隔离后的路径，调用方（`chat.rs`）应提示用户该文件位置，而不是静默继续
///   从空会话开始。
///
/// 注意：本函数不校验内容；调用方应在 `SessionLog::from_jsonl` 失败后调用。
pub fn quarantine_corrupt(path: &Path) -> io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let mut target = with_suffix(path, &format!("{CORRUPT_SUFFIX_PREFIX}{stamp}"));
    let mut seq = 0u32;
    while target.exists() {
        seq += 1;
        target = with_suffix(path, &format!("{CORRUPT_SUFFIX_PREFIX}{stamp}-{seq}"));
    }
    fs::rename(path, &target)?;
    Ok(target)
}

/// 从 `<path>.bak` 恢复：**仅当** `path` 不存在且备份存在时执行，返回是否恢复。
///
/// 用于 [`save_atomic`] 的 Windows `remove_file` → `rename` 窗口崩溃之后：启动时先
/// 调用本函数，`true` 说明发生过"目标缺失"，备份已**原子**恢复为新文件（备份本身
/// 保留）。目标存在时一律返回 `false`，绝不覆盖当前文件（避免把新内容降级成旧备份）。
pub fn recover_backup(path: &Path) -> io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    let backup = backup_path_for(path);
    if !backup.exists() {
        return Ok(false);
    }
    let contents = fs::read(&backup)?;
    // 复用 `save_atomic`：恢复过程本身也要 fsync + rename，避免恢复时再被截断。
    save_atomic(path, &contents)?;
    Ok(true)
}

/// 把 `contents` 写进 `path` 并 `sync_all`。
fn write_synced(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(contents)?;
    // 先 fsync 数据与元数据，再让调用方 rename：崩溃后要么是旧版，要么是完整新版。
    file.sync_all()
}

/// 用临时文件替换目标（平台相关）。
///
/// - Unix：`rename(2)` 对同一文件系统内的已存在目标是**原子覆盖**，直接改名；
/// - Windows：优先 `rename`（`MoveFileExW` 带替换语义）；失败且目标存在时退化为
///   先删后改名（非严格原子，窗口由 `.bak` 兜底）。改名仍失败则尽力把 `.bak`
///   复制回目标，避免留下"目标缺失"状态。
#[cfg(not(windows))]
fn replace(tmp: &Path, dest: &Path) -> io::Result<()> {
    fs::rename(tmp, dest)
}

#[cfg(windows)]
fn replace(tmp: &Path, dest: &Path) -> io::Result<()> {
    match fs::rename(tmp, dest) {
        Ok(()) => Ok(()),
        Err(_) if dest.exists() => {
            fs::remove_file(dest)?;
            match fs::rename(tmp, dest) {
                Ok(()) => Ok(()),
                Err(err) => {
                    let backup = backup_path_for(dest);
                    if backup.exists() {
                        let _ = fs::copy(&backup, dest);
                    }
                    Err(err)
                }
            }
        }
        Err(err) => Err(err),
    }
}

/// 同目录临时文件名：`.<file>.tmp-<pid>-<纳秒>`。
fn temp_path_for(path: &Path) -> io::Result<PathBuf> {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("路径缺少可用文件名: {}", path.display()),
        ));
    };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    Ok(parent.join(format!(".{file_name}.tmp-{pid}-{nanos}")))
}

/// `<path>.bak`。
fn backup_path_for(path: &Path) -> PathBuf {
    with_suffix(path, BACKUP_SUFFIX)
}

/// 在完整路径后追加后缀（保留扩展名，如 `s-1.jsonl` → `s-1.jsonl.bak`）。
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// 清理同目录遗留的 `.<file>.tmp-*`（只清陈旧的，见 [`STALE_TEMP_MAX_AGE`]）。
///
/// 清理是卫生工作，失败一律忽略，不能影响保存结果。
fn cleanup_stale_temps(path: &Path) {
    let _ = cleanup_stale_temps_older_than(path, STALE_TEMP_MAX_AGE);
}

/// [`cleanup_stale_temps`] 的可测试实现：删除年龄不小于 `max_age` 的遗留临时文件，
/// 返回删除个数。
fn cleanup_stale_temps_older_than(path: &Path, max_age: Duration) -> io::Result<usize> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(0);
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(0);
    };
    let prefix = format!(".{file_name}.tmp-");
    let mut removed = 0;
    for entry in fs::read_dir(parent)?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        // 只清旧文件：刚创建的可能属于并发写入者，删掉会毁掉别人的保存。
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if stale && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// 一次扫描从 JSONL 内容里提取的派生信息。
#[derive(Debug, Default)]
struct Summary {
    title: String,
    events: usize,
    /// 第一条 `session.created` 的树字段；旧日志没有该事件。
    created: Option<SessionCreated>,
}

/// 从事件文本推导标题、事件数与会话树字段。
///
/// 性能注意：只解析**第一条**事件（`session.created` 必须是第一条）和第一条
/// `message.user`，其余行只计数——10k 事件的目录扫描不能退化成全量 JSON 解析。
fn summarize(content: &str) -> Summary {
    let mut summary = Summary::default();
    let mut parsed_first = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        summary.events += 1;
        if parsed_first && !summary.title.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            parsed_first = true;
            continue;
        };
        let event_type = value.get("type").and_then(|value| value.as_str());
        if !parsed_first {
            parsed_first = true;
            if event_type == Some(crate::event_type::SESSION_CREATED) {
                if let Some(payload) = value.get("payload") {
                    summary.created = SessionCreated::from_payload(payload).ok();
                }
            }
        }
        if summary.title.is_empty() && event_type == Some("message.user") {
            let payload = &value["payload"];
            let blocks = payload["content"].as_array();
            let title = payload["text"]
                .as_str()
                .or_else(|| blocks?.first()?["text"].as_str())
                .filter(|text| !text.trim().is_empty())
                .or_else(|| {
                    blocks?
                        .iter()
                        .find_map(|block| block["attachment_name"].as_str())
                });
            if let Some(title) = title {
                summary.title = truncate(title, 40);
            }
        }
    }
    if summary.title.is_empty() {
        summary.title = String::from("空会话");
    }
    summary
}

fn tree_io(err: TreeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err.to_string())
}

fn truncate(text: &str, max_chars: usize) -> String {
    let flattened: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flattened.chars().take(max_chars).collect();
    if flattened.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn read_metadata(path: &Path) -> io::Result<serde_json::Map<String, serde_json::Value>> {
    match fs::read(path) {
        Ok(contents) => serde_json::from_slice(&contents).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Default::default()),
        Err(error) => Err(error),
    }
}

fn modified_at(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
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
        let restored =
            SessionLog::from_jsonl("s-child", &fs::read_to_string(&path).unwrap()).unwrap();
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
}
