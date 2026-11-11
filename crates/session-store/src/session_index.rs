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
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::session_tree::{SessionTree, TreeError};
use crate::{SessionCreated, SessionLog, SpawnSource};

/// 侧栏一行所需的会话元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    /// Unix 秒；UI 负责格式化成"刚刚 / N 分钟前"。
    pub updated_at: u64,
    pub path: PathBuf,
    /// 事件条数（用于展示"多少条"）。
    pub events: usize,
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
    pub fn scan(&self) -> io::Result<Vec<SessionMeta>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };

        let mut sessions = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let content = fs::read_to_string(&path).unwrap_or_default();
            let Summary {
                title,
                events,
                created,
            } = summarize(&content);
            let renamed = read_title(&self.meta_path(&id));
            let parent_session_id = created
                .as_ref()
                .and_then(|created| created.parent_session_id.clone());
            let spawn_source = created.as_ref().and_then(|created| created.spawn_source);
            sessions.push(SessionMeta {
                title: renamed.unwrap_or(title),
                id,
                updated_at: modified_at(&path),
                path,
                events,
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
        if !path.exists() {
            fs::write(&path, b"")?;
        }
        Ok(SessionMeta {
            id: id.to_owned(),
            title: String::from("新会话"),
            updated_at: modified_at(&path),
            path,
            events: 0,
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
        if path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("会话 `{id}` 已存在，拒绝覆盖事件日志"),
            ));
        }
        let mut log = SessionLog::new(id);
        log.append_created(ts, created)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        let jsonl = log.to_jsonl();
        fs::write(&path, &jsonl)?;
        let summary = summarize(&jsonl);
        Ok(SessionMeta {
            id: id.to_owned(),
            title: summary.title,
            updated_at: modified_at(&path),
            path,
            events: summary.events,
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
        fs::write(&path, log.to_jsonl())?;
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
        remove_if_exists(&self.path_for(id))?;
        remove_if_exists(&self.meta_path(id))
    }

    /// 重命名：写 sidecar meta，不改动 append-only 的事件文件。
    pub fn rename(&self, id: &str, title: &str) -> io::Result<()> {
        let title = title.trim();
        if title.is_empty() {
            return Ok(());
        }
        self.ensure_dir()?;
        let meta = serde_json::json!({ "title": title });
        fs::write(
            self.meta_path(id),
            serde_json::to_vec_pretty(&meta).unwrap_or_default(),
        )
    }

    /// 生成不重复的新会话 ID（毫秒时间戳，足够本地使用）。
    pub fn new_id(&self) -> String {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let mut candidate = format!("s-{millis}");
        let mut suffix = 1;
        while self.path_for(&candidate).exists() {
            candidate = format!("s-{millis}-{suffix}");
            suffix += 1;
        }
        candidate
    }
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
            if let Some(text) = value
                .get("payload")
                .and_then(|payload| payload.get("text"))
                .and_then(|text| text.as_str())
            {
                summary.title = truncate(text, 40);
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

fn read_title(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("title")
        .and_then(|title| title.as_str())
        .map(str::to_owned)
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
        std::thread::sleep(std::time::Duration::from_millis(10));
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
        index.create_root("s-root", "ws-1", "t0").unwrap();
        index
            .create_child("s-child", "s-root", "ws-1", "t1")
            .unwrap();
        index
            .create_child("s-grand", "s-child", "ws-1", "t2")
            .unwrap();

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
}
