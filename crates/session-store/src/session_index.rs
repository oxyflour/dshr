//! 会话索引：把目录里的 `*.jsonl` 映射成侧栏可用的元数据。
//!
//! 只做**目录级索引**，不解析事件模型（那是 `SessionLog` 的事）：
//! - 文件名的 stem 就是 `session_id`；
//! - 标题优先取 `.meta.json` 里用户重命名的值，否则取第一条 `message.user` 的文本；
//! - 更新时间用文件 mtime（UI 只显示相对时间，避免引入日期库）。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
            let (derived_title, events) = summarize(&content);
            let renamed = read_title(&self.meta_path(&id));
            sessions.push(SessionMeta {
                title: renamed.unwrap_or(derived_title),
                id,
                updated_at: modified_at(&path),
                path,
                events,
            });
        }
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
        Ok(sessions)
    }

    /// 创建一个空会话文件（已存在则复用）。
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
        })
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

/// 从事件文本推导标题与事件数。
fn summarize(content: &str) -> (String, usize) {
    let mut events = 0usize;
    let mut title = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        events += 1;
        if !title.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if value.get("type").and_then(|v| v.as_str()) != Some("message.user") {
            continue;
        }
        if let Some(text) = value
            .get("payload")
            .and_then(|payload| payload.get("text"))
            .and_then(|text| text.as_str())
        {
            title = truncate(text, 40);
        }
    }
    if title.is_empty() {
        title = String::from("空会话");
    }
    (title, events)
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
}
