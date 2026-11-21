//! 上下文外部化（spill）：大工具结果落盘为内容寻址引用，模型只看到引用 + 开头预览。
//!
//! 设计约束（PLAN §6.4、spec/session-log.md §5）：
//! - **内容寻址**：`sha256(content 的规范 JSON 字节)` 是唯一地址，`size` 用于快速校验；
//!   同一内容重复外部化只落一份（去重）。
//! - **留痕**：每次外部化写 `spill.created`（id/hash/size/mime/retention）与
//!   `tool.result.pruned`（call_id + 原始大小 + spill_ref），回放只依赖事件、不依赖内存。
//! - **确定性回放**：引用文本是 `(spill 元数据, 内容开头 N 字符)` 的纯函数，没有时间戳、
//!   绝对路径或随机量；同一份 JSONL 重建出的模型历史逐字节一致。
//! - **无损**：原始内容（事件里的 `content` JSON）完整落盘，可按 hash 取回并校验。
//!
//! 阈值策略与已有 head/tail 裁剪是两层：
//! - 本模块只处理"超过 spill 阈值"的超大结果（默认 16 KiB），把它们整个换成引用；
//! - `crate::prune_tool_result` 的头尾裁剪保持原样，仍作用于未外部化的长结果，
//!   `plan_with_window` 的行为与引入 spill 前完全一致。

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use dshr_agent_core::{AttachmentFormat, ChatMessage};
use dshr_session_store::{Durability, EventSpec, LogError, SessionLog, ToolCallKey};
use serde_json::{json, Value};

use crate::sha256::digest_hex;

/// 默认 spill 阈值：工具结果的规范 JSON 字节数 **超过** 该值才外部化（16 KiB）。
pub const DEFAULT_SPILL_THRESHOLD_BYTES: usize = 16 * 1024;

/// 默认预览字符数：模型可见的开头预览长度。
pub const DEFAULT_SPILL_PREVIEW_CHARS: usize = 512;

/// 内容寻址对象目录名（位于 spill root 下）。
pub const OBJECTS_DIR: &str = "objects";

/// 事件类型：外部化记录（durable）。
pub const EVENT_SPILL_CREATED: &str = "spill.created";

/// 事件类型：工具结果被裁剪/外部化（durable）。
pub const EVENT_TOOL_RESULT_PRUNED: &str = "tool.result.pruned";

/// 附件保留期。`Session` 表示随会话删除；`Days(n)` 表示事故后可保留 n 天。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Retention {
    Session,
    Days(u32),
}

impl Retention {
    /// 默认保留期（spec §8：附件 90 天）。
    pub const DEFAULT: Retention = Retention::Days(90);

    /// 事件里的字符串形式：`"session"` 或 `"90d"`。
    pub fn as_str(self) -> String {
        match self {
            Retention::Session => String::from("session"),
            Retention::Days(days) => format!("{days}d"),
        }
    }

    /// 解析 `"session"` / `"90d"`；无法识别返回 `None`。
    pub fn parse(text: &str) -> Option<Retention> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("session") {
            return Some(Retention::Session);
        }
        let days = text.strip_suffix('d')?.parse::<u32>().ok()?;
        Some(Retention::Days(days))
    }

    /// 从事件载荷解析，缺省/非法时返回默认值。
    pub fn from_payload(payload: &Value) -> Retention {
        payload
            .get("retention")
            .and_then(Value::as_str)
            .and_then(Retention::parse)
            .unwrap_or(Retention::DEFAULT)
    }
}

impl Default for Retention {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// 外部化策略。`threshold_bytes == usize::MAX` 等价于关闭（`SpillPolicy::disabled()`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillPolicy {
    /// 规范 JSON 字节数严格大于该值才外部化。
    pub threshold_bytes: usize,
    /// 模型可见预览的字符数。
    pub preview_chars: usize,
    /// 附件保留期（写进 `spill.created.retention`）。
    pub retention: Retention,
}

impl Default for SpillPolicy {
    fn default() -> Self {
        Self {
            threshold_bytes: DEFAULT_SPILL_THRESHOLD_BYTES,
            preview_chars: DEFAULT_SPILL_PREVIEW_CHARS,
            retention: Retention::DEFAULT,
        }
    }
}

impl SpillPolicy {
    /// 永不外部化（保持既有裁剪行为用）。
    pub fn disabled() -> Self {
        Self {
            threshold_bytes: usize::MAX,
            preview_chars: DEFAULT_SPILL_PREVIEW_CHARS,
            retention: Retention::DEFAULT,
        }
    }

    /// 从环境变量覆盖默认值：
    /// `DSHR_SPILL_THRESHOLD_BYTES`、`DSHR_SPILL_PREVIEW_CHARS`、`DSHR_SPILL_RETENTION`。
    pub fn from_env() -> Self {
        let mut policy = Self::default();
        if let Ok(text) = std::env::var("DSHR_SPILL_THRESHOLD_BYTES") {
            if let Ok(value) = text.trim().parse::<usize>() {
                policy.threshold_bytes = value;
            }
        }
        if let Ok(text) = std::env::var("DSHR_SPILL_PREVIEW_CHARS") {
            if let Ok(value) = text.trim().parse::<usize>() {
                policy.preview_chars = value;
            }
        }
        if let Ok(text) = std::env::var("DSHR_SPILL_RETENTION") {
            if let Some(retention) = Retention::parse(&text) {
                policy.retention = retention;
            }
        }
        policy
    }

    /// 判断一份工具结果 `content`（`tool.result.content` 的 JSON 值）是否该外部化。
    /// 返回引用元数据与要落盘的规范 JSON 字节。
    pub fn classify(&self, content: &Value) -> Option<(SpillRef, Vec<u8>)> {
        if self.threshold_bytes == usize::MAX {
            return None;
        }
        let (spill, payload) = spill_content(content, self.retention);
        (payload.len() > self.threshold_bytes).then_some((spill, payload))
    }
}

/// 内容寻址引用。`id` 是 `$attachment` 引用目标，`hash` 是完整 sha256。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpillRef {
    pub id: String,
    pub hash: String,
    pub size: u64,
    pub mime: String,
    pub retention: Retention,
}

impl SpillRef {
    /// 由已落盘字节构造引用（hash 由内容计算）。
    pub fn from_payload(bytes: &[u8], mime: &str, retention: Retention) -> Self {
        let hash = digest_hex(bytes);
        Self {
            id: format!("spill:{hash}"),
            hash,
            size: bytes.len() as u64,
            mime: mime.to_owned(),
            retention,
        }
    }

    /// 由 `tool.result.content` 构造引用（不落盘）。
    pub fn from_content(content: &Value, retention: Retention) -> Self {
        let (spill, _) = spill_content(content, retention);
        spill
    }

    /// 相对 spill root 的对象路径：`objects/<hash 前 2 位>/<hash>`。
    pub fn relative_object_path(&self) -> PathBuf {
        let prefix = &self.hash[..2];
        Path::new(OBJECTS_DIR).join(prefix).join(&self.hash)
    }

    /// 写进 `spill.created` 事件的载荷。`call_id` / `preview_chars` 是本实现新增的可选字段。
    pub fn event_payload(&self, call_id: &str, preview_chars: usize) -> Value {
        json!({
            "id": self.id,
            "hash": self.hash,
            "size": self.size,
            "mime": self.mime,
            "retention": self.retention.as_str(),
            "preview_chars": preview_chars,
            "call_id": call_id,
            "kind": "tool.result",
        })
    }

    pub fn user_attachment_payload(&self) -> Value {
        json!({
            "id": self.id,
            "hash": self.hash,
            "size": self.size,
            "mime": self.mime,
            "retention": self.retention.as_str(),
            "kind": "message.user",
        })
    }

    /// 从 `spill.created` 载荷解析；字段缺失或 hash 非法返回 `None`。
    pub fn from_event_payload(payload: &Value) -> Option<Self> {
        let hash = payload.get("hash").and_then(Value::as_str)?.to_owned();
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let size = payload.get("size").and_then(Value::as_u64)?;
        let id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("spill:{hash}"));
        let mime = payload
            .get("mime")
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream")
            .to_owned();
        Some(Self {
            id,
            hash,
            size,
            mime,
            retention: Retention::from_payload(payload),
        })
    }
}

/// 待落盘的溢出结果：工具调用、引用元数据、原始内容字节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSpill {
    pub call_id: String,
    pub call_seq: Option<u64>,
    pub spill: SpillRef,
    pub payload: Vec<u8>,
    pub preview_chars: usize,
}

/// 日志里已留痕的外部化记录（回放时用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedSpill {
    pub spill: SpillRef,
    pub preview_chars: usize,
}

/// 一份 `tool.result.content` 的规范 JSON 字节（落盘/校验的唯一输入）。
pub fn spill_payload(content: &Value) -> Vec<u8> {
    serde_json::to_vec(content).expect("serde_json::Value 永远可序列化")
}

/// 内容对应的 MIME：字符串按文本，其余按 JSON。
pub fn mime_for_content(content: &Value) -> &'static str {
    if content.is_string() {
        "text/plain; charset=utf-8"
    } else {
        "application/json"
    }
}

/// 一次性构造 `(引用, 规范 JSON 字节)`。
pub fn spill_content(content: &Value, retention: Retention) -> (SpillRef, Vec<u8>) {
    let payload = spill_payload(content);
    (
        SpillRef::from_payload(&payload, mime_for_content(content), retention),
        payload,
    )
}

/// 模型可见的开头预览：取前 `preview_chars` 个字符，截断时补 `…`。
pub fn preview_of(text: &str, preview_chars: usize) -> String {
    let mut chars = text.chars();
    let mut preview: String = chars.by_ref().take(preview_chars).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    preview
}

/// 引用 + 预览的模型可见文本。纯函数：同一组输入永远得到同一字符串。
pub fn render_reference(spill: &SpillRef, preview: &str) -> String {
    format!(
        "[工具结果已外部化] id={} sha256={} size={}B mime={} retention={}\n\
         完整内容已落盘（会话附件/导出可离线取回），模型只看到引用与开头预览：\n\
         {preview}",
        spill.id,
        spill.hash,
        spill.size,
        spill.mime,
        spill.retention.as_str(),
    )
}

/// 从日志恢复 `call_id -> 已外部化记录` 映射（确定性回放用）。
///
/// 关联规则：
/// 1. `spill.created` 自带 `call_id` 时直接关联（本实现的写法）；
/// 2. 否则通过 `tool.result.pruned.spill_ref` 关联到同 id 的 `spill.created`。
///
/// 找不到 `spill.created` 的 `pruned` 事件会被忽略（退化为完整内容，不阻塞回放）。
pub fn recorded_spills(log: &SessionLog) -> HashMap<ToolCallKey, RecordedSpill> {
    let mut created: HashMap<String, RecordedSpill> = HashMap::new();
    let mut map = HashMap::new();
    let mut edges = Vec::new();

    for event in log.events() {
        match event.event_type.as_str() {
            EVENT_SPILL_CREATED => {
                let Some(spill) = SpillRef::from_event_payload(&event.payload) else {
                    continue;
                };
                let preview_chars = event
                    .payload
                    .get("preview_chars")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize)
                    .unwrap_or(DEFAULT_SPILL_PREVIEW_CHARS);
                let recorded = RecordedSpill {
                    spill: spill.clone(),
                    preview_chars,
                };
                if let Some(call_id) = event.payload.get("call_id").and_then(Value::as_str) {
                    if !call_id.is_empty() {
                        map.entry(ToolCallKey::from_payload(&event.payload))
                            .or_insert(recorded.clone());
                    }
                }
                created.entry(spill.id.clone()).or_insert(recorded);
            }
            EVENT_TOOL_RESULT_PRUNED => {
                let call_id = event
                    .payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let spill_ref = event
                    .payload
                    .get("spill_ref")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if !call_id.is_empty() && !spill_ref.is_empty() {
                    edges.push((ToolCallKey::from_payload(&event.payload), spill_ref));
                }
            }
            _ => {}
        }
    }

    for (key, spill_ref) in edges {
        if let Some(recorded) = created.get(&spill_ref) {
            map.entry(key).or_insert_with(|| recorded.clone());
        }
    }
    map
}

/// 日志里出现过的全部 spill 引用（按首次出现顺序、按 hash 去重）。
pub fn spill_refs_in_log(log: &SessionLog) -> Vec<SpillRef> {
    let mut seen = std::collections::HashSet::new();
    let mut refs = Vec::new();
    for event in log.events() {
        if event.event_type != EVENT_SPILL_CREATED {
            continue;
        }
        let Some(spill) = SpillRef::from_event_payload(&event.payload) else {
            continue;
        };
        if seen.insert(spill.hash.clone()) {
            refs.push(spill);
        }
    }
    refs
}

/// 把一次外部化写进日志：`spill.created` + `tool.result.pruned`。
///
/// 调用方应先 `SpillStore::put_pending` 成功再写事件，保证"事件可见时附件已落盘"。
/// 返回两条事件分配到的 `seq`。
pub fn record_spill(
    log: &mut SessionLog,
    pending: &PendingSpill,
    ts: &str,
) -> Result<(u64, u64), LogError> {
    let mut created = pending
        .spill
        .event_payload(&pending.call_id, pending.preview_chars);
    let mut pruned = json!({
        "call_id": pending.call_id,
        "original_size": pending.spill.size,
        "spill_ref": pending.spill.id,
        "mode": "spill",
        "preview_chars": pending.preview_chars,
    });
    if let Some(call_seq) = pending.call_seq {
        created["call_seq"] = json!(call_seq);
        pruned["call_seq"] = json!(call_seq);
    }
    let created_seq = log.append(EventSpec::durable(EVENT_SPILL_CREATED, ts, created))?;
    let pruned_seq = log.append(EventSpec::durable(EVENT_TOOL_RESULT_PRUNED, ts, pruned))?;
    Ok((created_seq, pruned_seq))
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 内容寻址的 spill 存储：`<root>/objects/<hash[..2]>/<hash>`。
#[derive(Debug, Clone)]
pub struct SpillStore {
    root: PathBuf,
    policy: SpillPolicy,
}

impl SpillStore {
    /// 使用默认策略创建（目录按需创建）。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            policy: SpillPolicy::default(),
        }
    }

    /// 指定策略创建。
    pub fn with_policy(root: impl Into<PathBuf>, policy: SpillPolicy) -> Self {
        Self {
            root: root.into(),
            policy,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn policy(&self) -> &SpillPolicy {
        &self.policy
    }

    /// 某个引用的绝对对象路径。
    pub fn object_path(&self, spill: &SpillRef) -> PathBuf {
        self.root.join(spill.relative_object_path())
    }

    pub fn contains(&self, spill: &SpillRef) -> bool {
        self.object_path(spill).is_file()
    }

    /// 落盘并按 hash 去重：对象已存在且大小一致时返回 `Ok(false)`，否则写入返回 `Ok(true)`。
    ///
    /// 内容寻址意味着"同一内容只存一份"；写入用同目录临时文件 + rename，避免半截对象。
    pub fn put(&self, spill: &SpillRef, bytes: &[u8]) -> io::Result<bool> {
        let path = self.object_path(spill);
        if let Ok(metadata) = fs::metadata(&path) {
            if metadata.len() == bytes.len() as u64 {
                return Ok(false);
            }
            // 长度不一致说明对象损坏；内容寻址目录里不该出现，直接重写修复。
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_dir = self.root.join("tmp");
        fs::create_dir_all(&tmp_dir)?;
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = tmp_dir.join(format!("{}.{}.{}", spill.hash, std::process::id(), counter));
        fs::write(&tmp, bytes)?;
        if let Err(err) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
        Ok(true)
    }

    /// 落盘一条待处理的溢出结果（去重）。
    pub fn put_pending(&self, pending: &PendingSpill) -> io::Result<bool> {
        self.put(&pending.spill, &pending.payload)
    }

    /// 按引用读取对象并校验大小。
    pub fn get(&self, spill: &SpillRef) -> io::Result<Vec<u8>> {
        let file = fs::File::open(self.object_path(spill))?;
        let mut bytes = Vec::new();
        file.take(spill.size.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != spill.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "spill {} 大小不符：磁盘 {} 字节 != 记录 {} 字节",
                    spill.id,
                    bytes.len(),
                    spill.size
                ),
            ));
        }
        Ok(bytes)
    }

    /// 读取并校验 sha256（无损取回入口）。
    pub fn get_verified(&self, spill: &SpillRef) -> io::Result<Vec<u8>> {
        let bytes = self.get(spill)?;
        let actual = digest_hex(&bytes);
        if actual != spill.hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "spill {} sha256 不符：期望 {}，实际 {actual}",
                    spill.id, spill.hash
                ),
            ));
        }
        Ok(bytes)
    }

    /// 读取并还原原始 `content` JSON（无损）。
    pub fn restore_json(&self, spill: &SpillRef) -> io::Result<Value> {
        let bytes = self.get_verified(spill)?;
        serde_json::from_slice(&bytes).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("spill {} 不是合法 JSON: {err}", spill.id),
            )
        })
    }
}

/// Durable attachment records keyed by the reference used by user messages.
pub fn user_attachment_refs(log: &SessionLog) -> HashMap<String, SpillRef> {
    log.events()
        .iter()
        .filter(|event| {
            event.event_type == EVENT_SPILL_CREATED
                && event.durability == Durability::Durable
                && event.payload["kind"] == "message.user"
        })
        .filter_map(|event| SpillRef::from_event_payload(&event.payload))
        .map(|spill| (spill.id.clone(), spill))
        .collect()
}

/// Resolve retained user attachments newest-first using the same objects as session export.
pub fn restore_user_attachments(
    store: &SpillStore,
    log: &SessionLog,
    messages: &mut [ChatMessage],
    supports: impl Fn(AttachmentFormat) -> bool,
) {
    if !messages
        .iter()
        .any(|message| matches!(message, ChatMessage::UserWithAttachments { .. }))
    {
        return;
    }
    let refs = user_attachment_refs(log);
    let mut remaining = 24 * 1024 * 1024u64;
    let mut count = 20;
    for message in messages.iter_mut().rev() {
        let ChatMessage::UserWithAttachments { text, attachments } = message else {
            continue;
        };
        attachments.retain_mut(|attachment| {
            let result = if !supports(attachment.format) {
                Err("当前模型不支持")
            } else if let Some(spill) = refs
                .get(&attachment.reference)
                .filter(|spill| spill.mime == attachment.format.mime())
            {
                if count == 0 || spill.size > remaining {
                    Err("超出历史附件预算")
                } else {
                    store
                        .get_verified(spill)
                        .map(|bytes| {
                            remaining -= spill.size;
                            count -= 1;
                            attachment.bytes = Some(bytes.into());
                        })
                        .map_err(|_| "本地文件缺失或已损坏")
                }
            } else {
                Err("文件引用无效")
            };
            if let Err(reason) = result {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("附件 {}（{reason}）", attachment.name));
                false
            } else {
                true
            }
        });
        if attachments.is_empty() {
            *message = ChatMessage::User(std::mem::take(text));
        }
    }
}

/// 计算任意字节的 sha256 小写十六进制串（导出 manifest 复用）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    digest_hex(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshr_agent_core::ChatMessage;
    use dshr_session_store::EventSpec;
    use serde_json::json;

    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "dshr-spill-{}-{}-{}",
                tag,
                std::process::id(),
                counter
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create temp root");
            Self { path }
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn big_text(bytes: usize) -> String {
        "A".repeat(bytes)
    }

    fn log_with_tool_result(content: Value) -> SessionLog {
        let mut log = SessionLog::new("s-spill");
        log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": "u1", "text": "跑一下" }),
        ))
        .expect("append user");
        log.append(EventSpec::durable(
            "tool.call",
            "t0",
            json!({ "call_id": "c1", "tool": "bash", "arguments": {} }),
        ))
        .expect("append call");
        log.append(EventSpec::durable(
            "tool.result",
            "t0",
            json!({ "call_id": "c1", "is_error": false, "content": content }),
        ))
        .expect("append result");
        log
    }

    /// 两个只会外部化大结果的策略（阈值刻意调小，便于测试）。
    fn tiny_policy() -> SpillPolicy {
        SpillPolicy {
            threshold_bytes: 256,
            preview_chars: 32,
            retention: Retention::Days(90),
        }
    }

    #[test]
    fn sha256_hex_is_stable_and_verified() {
        let root = TempRoot::new("sha");
        let store = SpillStore::new(&root.path);
        let bytes = b"hello spill".to_vec();
        let spill = SpillRef::from_payload(&bytes, "application/json", Retention::Days(7));
        assert_eq!(spill.id, format!("spill:{}", spill.hash));
        assert_eq!(spill.size, bytes.len() as u64);
        assert!(store.put(&spill, &bytes).expect("put"));
        assert_eq!(store.get_verified(&spill).expect("get"), bytes);
        assert_eq!(sha256_hex(&bytes), spill.hash);
    }

    #[test]
    fn over_threshold_result_becomes_reference_with_head_preview() {
        let content = json!([{ "type": "text", "text": big_text(4_000) }]);
        let log = log_with_tool_result(content.clone());
        let plan = crate::plan_with_window_and_spill(&log, "", 64_000, &tiny_policy());

        assert_eq!(plan.spilled.len(), 1, "{plan:?}");
        let pending = &plan.spilled[0];
        assert_eq!(pending.call_id, "c1");
        assert_eq!(pending.spill.size, spill_payload(&content).len() as u64);
        assert!(pending.spill.size > 256);

        let ChatMessage::Tool { content, .. } = plan
            .messages
            .iter()
            .find(|message| matches!(message, ChatMessage::Tool { .. }))
            .expect("tool message")
        else {
            unreachable!()
        };
        assert!(content.contains("[工具结果已外部化]"), "{content}");
        assert!(content.contains(&pending.spill.hash), "{content}");
        assert!(content.contains("开头预览"), "{content}");
        // 预览只有开头 32 字符，不包含完整正文。
        assert!(!content.contains(&"A".repeat(64)), "预览必须被截断");
        assert!(plan.tokens_before > plan.tokens_after, "{plan:?}");
        // 未超预算：不裁剪、不丢弃，即原有裁剪行为没有额外触发。
        assert_eq!(plan.pruned_tool_results, 0);
        assert_eq!(plan.dropped_messages, 0);
    }

    #[test]
    fn small_result_is_left_untouched() {
        let log = log_with_tool_result(json!([{ "type": "text", "text": "ok" }]));
        let policy = tiny_policy();
        let plan = crate::plan_with_window_and_spill(&log, "", 64_000, &policy);
        assert!(plan.spilled.is_empty(), "{plan:?}");
        assert_eq!(
            plan.messages,
            crate::plan_with_window(&log, "", 64_000).messages
        );
    }

    #[test]
    fn disabled_policy_and_default_plan_are_identical_without_events() {
        let log = log_with_tool_result(json!([{ "type": "text", "text": big_text(4_000) }]));
        let plain = crate::plan_with_window(&log, "继续", 64_000);
        let disabled =
            crate::plan_with_window_and_spill(&log, "继续", 64_000, &SpillPolicy::disabled());
        assert_eq!(plain, disabled);
        assert!(plain.spilled.is_empty());
    }

    #[test]
    fn existing_pruning_still_applies_below_spill_threshold() {
        // 结果长度在 800 token 以上但低于 spill 阈值时，仍走原有的头尾裁剪。
        let long = big_text(4_000);
        let log = log_with_tool_result(json!([{ "type": "text", "text": long }]));
        let plan = crate::plan_with_window(&log, "", 1_000);
        assert_eq!(plan.pruned_tool_results, 1, "{plan:?}");
        assert!(plan.spilled.is_empty());
    }

    #[test]
    fn store_deduplicates_identical_content() {
        let root = TempRoot::new("dedup");
        let store = SpillStore::new(&root.path);
        let content = json!([{ "type": "text", "text": big_text(2_000) }]);
        let (spill, payload) = spill_content(&content, Retention::Days(90));

        assert!(store.put(&spill, &payload).expect("first put"), "首次写入");
        assert!(
            !store.put(&spill, &payload).expect("second put"),
            "重复写入必须去重"
        );

        let mut files = Vec::new();
        for entry in fs::read_dir(root.path.join(OBJECTS_DIR).join(&spill.hash[..2])).expect("dir")
        {
            files.push(entry.expect("entry").file_name());
        }
        assert_eq!(files.len(), 1, "同一内容只应有一个对象文件: {files:?}");

        // 另一份内容不同 -> 不同地址、不同文件。
        let other = json!([{ "type": "text", "text": "other" }]);
        let (other_spill, other_payload) = spill_content(&other, Retention::Days(90));
        assert!(other_spill.hash != spill.hash);
        assert!(store.put(&other_spill, &other_payload).expect("other put"));
        assert_ne!(store.object_path(&spill), store.object_path(&other_spill));
    }

    #[test]
    fn replay_after_recording_events_is_byte_identical() {
        let content = json!([{ "type": "text", "text": big_text(4_000) }]);
        let mut log = log_with_tool_result(content.clone());
        let policy = tiny_policy();

        let first = crate::plan_with_window_and_spill(&log, "", 64_000, &policy);
        assert_eq!(first.spilled.len(), 1);
        for pending in &first.spilled {
            let (created, pruned) = record_spill(&mut log, pending, "t1").expect("record spill");
            assert_eq!(created, log.len() as u64 - 1);
            assert_eq!(pruned, log.len() as u64);
        }

        let replay = crate::plan_with_window_and_spill(&log, "", 64_000, &policy);
        assert!(replay.spilled.is_empty(), "已留痕的结果不应重复外部化");
        assert_eq!(replay.messages, first.messages, "回放必须逐字节一致");

        // 旧入口 `plan_with_window` 也按事件还原引用（不新增外部化）。
        let legacy = crate::plan_with_window(&log, "", 64_000);
        assert_eq!(legacy.messages, first.messages);

        // 引用预览与首次生成时相同。
        let ChatMessage::Tool { content: text, .. } = replay
            .messages
            .iter()
            .find(|message| matches!(message, ChatMessage::Tool { .. }))
            .expect("tool message")
        else {
            unreachable!()
        };
        let expected_preview = preview_of(&"A".repeat(4_000), policy.preview_chars);
        assert!(text.contains(expected_preview.as_str()), "{text}");
    }

    #[test]
    fn spilled_content_is_losslessly_retrievable() {
        let root = TempRoot::new("lossless");
        let store = SpillStore::new(&root.path);
        let content = json!([{ "type": "text", "text": big_text(4_000) }]);
        let log = log_with_tool_result(content.clone());
        let plan = crate::plan_with_window_and_spill(&log, "", 64_000, &tiny_policy());
        let pending = plan.spilled.first().expect("pending spill");

        assert!(
            store.put_pending(pending).expect("put pending"),
            "首次落盘应写文件"
        );
        assert_eq!(
            store.get_verified(&pending.spill).expect("read"),
            pending.payload
        );
        assert_eq!(
            store.restore_json(&pending.spill).expect("restore json"),
            content
        );

        // 损坏对象必须被校验拒绝（先改磁盘内容，再按原引用读）。
        let path = store.object_path(&pending.spill);
        fs::write(&path, b"corrupted").expect("corrupt");
        assert!(store.get_verified(&pending.spill).is_err());
    }

    #[test]
    fn reference_is_deterministic_and_path_free() {
        let (spill, _) = spill_content(
            &json!([{ "type": "text", "text": "x" }]),
            Retention::Days(90),
        );
        let first = render_reference(&spill, &preview_of("hello world", 5));
        let second = render_reference(&spill, &preview_of("hello world", 5));
        assert_eq!(first, second);
        assert!(first.contains("hello…"), "{first}");
        assert!(!first.contains("C:\\"), "引用不得包含绝对路径");
        assert_eq!(Retention::parse("90d"), Some(Retention::Days(90)));
        assert_eq!(Retention::parse("session"), Some(Retention::Session));
        assert_eq!(Retention::parse("nonsense"), None);
    }

    #[test]
    fn recorded_spills_falls_back_to_pruned_edges() {
        let mut log = SessionLog::new("s-edge");
        let content = json!([{ "type": "text", "text": "data" }]);
        let (spill, _) = spill_content(&content, Retention::Days(90));
        log.append(EventSpec::durable(
            EVENT_SPILL_CREATED,
            "t0",
            spill.event_payload("", 64),
        ))
        .expect("created");
        log.append(EventSpec::durable(
            EVENT_TOOL_RESULT_PRUNED,
            "t0",
            json!({ "call_id": "c9", "original_size": 4, "spill_ref": spill.id }),
        ))
        .expect("pruned");

        let recorded = recorded_spills(&log);
        let entry = recorded
            .get(&ToolCallKey::Id("c9".to_owned()))
            .expect("edge 关联");
        assert_eq!(entry.spill, spill);
        assert_eq!(entry.preview_chars, 64);
        assert_eq!(spill_refs_in_log(&log), vec![spill]);
    }
}
