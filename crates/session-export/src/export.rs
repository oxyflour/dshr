//! 会话导出：JSONL + `<session>.attachments/` + `<session>.manifest.json`（带 sha256）。
//!
//! 导出是**只读**操作：不修改 Session Log，不请求 LLM，不执行工具。产物可离线使用：
//! - `events.jsonl` 等价于 `SessionLog::from_jsonl` 的输入（默认排除 ephemeral）；
//! - `attachments/<sha256>` 是 `spill.created` 引用到的外部化 blob，内容寻址；
//! - `manifest.json` 记录每个文件的 size + sha256，用于完整性校验。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dshr_agent_context::spill::{sha256_hex, spill_refs_in_log, SpillRef, SpillStore};
use dshr_session_store::{Durability, SessionLog};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 导出目录格式标识（写进 manifest，升级时改这里）。
pub const EXPORT_FORMAT: &str = "dshr-session-export/v1";

/// manifest schema 版本。
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// 会话日志文件名后缀：`<session_id>.jsonl`。
pub const JSONL_SUFFIX: &str = ".jsonl";

/// 外部化附件目录后缀：`<session_id>.attachments/`。
pub const ATTACHMENTS_SUFFIX: &str = ".attachments";

/// manifest 文件名后缀：`<session_id>.manifest.json`。
pub const MANIFEST_SUFFIX: &str = ".manifest.json";

/// 导出时可用的 blob 来源。缺附件不阻塞导出（记入 `missing_blobs`），除非显式要求失败。
pub trait BlobSource {
    /// 读取某个 spill 的原始内容；不存在返回 `Ok(None)`。
    fn read(&self, spill: &SpillRef) -> io::Result<Option<Vec<u8>>>;
}

/// 从本地 [`SpillStore`] 读 blob（读取时校验 size + sha256）。
#[derive(Debug, Clone, Copy)]
pub struct SpillStoreSource<'a> {
    store: &'a SpillStore,
}

impl<'a> SpillStoreSource<'a> {
    pub fn new(store: &'a SpillStore) -> Self {
        Self { store }
    }
}

impl BlobSource for SpillStoreSource<'_> {
    fn read(&self, spill: &SpillRef) -> io::Result<Option<Vec<u8>>> {
        if !self.store.contains(spill) {
            return Ok(None);
        }
        self.store.get_verified(spill).map(Some)
    }
}

/// 没有 blob 来源（只导出日志与 manifest，附件全部记为缺失）。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoBlobs;

impl BlobSource for NoBlobs {
    fn read(&self, _spill: &SpillRef) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

/// 导出选项。`exported_at` 由调用方提供（RFC 3339），保证导出可复现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOptions {
    pub exported_at: String,
    /// 是否导出 ephemeral 事件（默认否；回放不依赖它们）。
    pub include_ephemeral: bool,
    /// 有 spill 引用但 blob 缺失时是否直接失败（默认记录后继续）。
    pub fail_on_missing_blob: bool,
}

impl ExportOptions {
    pub fn new(exported_at: impl Into<String>) -> Self {
        Self {
            exported_at: exported_at.into(),
            include_ephemeral: false,
            fail_on_missing_blob: false,
        }
    }

    pub fn include_ephemeral(mut self, include: bool) -> Self {
        self.include_ephemeral = include;
        self
    }

    pub fn fail_on_missing_blob(mut self, fail: bool) -> Self {
        self.fail_on_missing_blob = fail;
        self
    }
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self::new("")
    }
}

/// 一份导出文件的完整性信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// manifest 里的一个外部化附件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentEntry {
    pub id: String,
    pub hash: String,
    pub size: u64,
    pub mime: String,
    pub retention: String,
    pub sha256: String,
    pub path: String,
}

/// 日志引用到、但本机取不到的 blob（记录而不阻塞导出）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingBlob {
    pub id: String,
    pub hash: String,
    pub size: u64,
    pub reason: String,
}

/// 导出 manifest（`<session_id>.manifest.json`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportManifest {
    pub format: String,
    pub schema_version: u32,
    pub session_id: String,
    pub exported_at: String,
    pub event_count: usize,
    pub durable_event_count: usize,
    pub ephemeral_event_count: usize,
    pub jsonl: FileEntry,
    pub attachments: Vec<AttachmentEntry>,
    pub missing_blobs: Vec<MissingBlob>,
    /// JSONL + 附件字节数（不含 manifest 自身）。
    pub total_bytes: u64,
}

/// JSONL 导出计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExportCounts {
    /// 实际写进 JSONL 的事件数。
    pub events: usize,
    pub durable: usize,
    /// 日志里的 ephemeral 事件数（即使未导出也计数）。
    pub ephemeral: usize,
}

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("会话 ID 为空，拒绝导出")]
    EmptySessionId,
    #[error("缺少 spill 附件: {id}")]
    MissingBlob { id: String },
    #[error(
        "spill 校验失败: {id}（期望 sha256 {expected} / {expected_size}B，实际 {actual} / {actual_size}B）"
    )]
    BlobIntegrity {
        id: String,
        expected: String,
        actual: String,
        expected_size: u64,
        actual_size: u64,
    },
    #[error("manifest 会话不符: 期望 {expected}，实际 {actual}")]
    ManifestSessionMismatch { expected: String, actual: String },
}

/// `<out_dir>/<session_id>.manifest.json`。
pub fn manifest_path(out_dir: &Path, session_id: &str) -> PathBuf {
    out_dir.join(format!("{session_id}{MANIFEST_SUFFIX}"))
}

/// 把日志序列化成导出用 JSONL（默认排除 ephemeral）。
pub fn jsonl_for_export(log: &SessionLog, include_ephemeral: bool) -> (String, ExportCounts) {
    let mut jsonl = String::new();
    let mut counts = ExportCounts::default();
    for event in log.events() {
        if event.durability == Durability::Ephemeral {
            counts.ephemeral += 1;
            if !include_ephemeral {
                continue;
            }
        } else {
            counts.durable += 1;
        }
        jsonl.push_str(&serde_json::to_string(event).expect("event is always serializable"));
        jsonl.push('\n');
        counts.events += 1;
    }
    (jsonl, counts)
}

/// 导出会话到 `out_dir`，返回写入的 manifest。
///
/// 目录布局：
/// ```text
/// <out_dir>/<session_id>.jsonl
/// <out_dir>/<session_id>.attachments/<sha256>
/// <out_dir>/<session_id>.manifest.json
/// ```
///
/// 已存在的同 hash 附件会复用并校验 sha256；blob 缺失默认记入 `missing_blobs`。
pub fn export_session(
    log: &SessionLog,
    blobs: &dyn BlobSource,
    out_dir: &Path,
    options: &ExportOptions,
) -> Result<ExportManifest, ExportError> {
    let session_id = log.session_id().to_owned();
    if session_id.is_empty() {
        return Err(ExportError::EmptySessionId);
    }
    fs::create_dir_all(out_dir)?;

    // 1) JSONL（先写盘，manifest 引用的 sha256 从同一份字节计算）。
    let (jsonl, counts) = jsonl_for_export(log, options.include_ephemeral);
    let jsonl_bytes = jsonl.into_bytes();
    let jsonl_rel = format!("{session_id}{JSONL_SUFFIX}");
    fs::write(out_dir.join(&jsonl_rel), &jsonl_bytes)?;
    let jsonl_entry = FileEntry {
        path: jsonl_rel,
        size: jsonl_bytes.len() as u64,
        sha256: sha256_hex(&jsonl_bytes),
    };

    // 2) 外部化 blob：按 hash 去重写入 `<session>.attachments/`。
    let attachments_rel_dir = format!("{session_id}{ATTACHMENTS_SUFFIX}");
    let attachments_dir = out_dir.join(&attachments_rel_dir);
    fs::create_dir_all(&attachments_dir)?;

    let mut attachments = Vec::new();
    let mut missing_blobs = Vec::new();
    for spill in spill_refs_in_log(log) {
        let Some(bytes) = blobs.read(&spill)? else {
            if options.fail_on_missing_blob {
                return Err(ExportError::MissingBlob {
                    id: spill.id.clone(),
                });
            }
            missing_blobs.push(MissingBlob {
                id: spill.id.clone(),
                hash: spill.hash.clone(),
                size: spill.size,
                reason: "blob 不在本机 spill 存储（可能已被保留策略清理）".to_owned(),
            });
            continue;
        };

        let actual_sha = sha256_hex(&bytes);
        if actual_sha != spill.hash || bytes.len() as u64 != spill.size {
            return Err(ExportError::BlobIntegrity {
                id: spill.id.clone(),
                expected: spill.hash.clone(),
                actual: actual_sha,
                expected_size: spill.size,
                actual_size: bytes.len() as u64,
            });
        }

        let relative_path = format!("{attachments_rel_dir}/{}", spill.hash);
        let object_path = out_dir.join(&relative_path);
        if object_path.exists() {
            // 同一目录重复导出：复用前先校验，避免悄悄覆盖坏文件。
            let existing = fs::read(&object_path)?;
            if sha256_hex(&existing) != spill.hash {
                return Err(ExportError::BlobIntegrity {
                    id: spill.id.clone(),
                    expected: spill.hash.clone(),
                    actual: sha256_hex(&existing),
                    expected_size: spill.size,
                    actual_size: existing.len() as u64,
                });
            }
        } else {
            fs::write(&object_path, &bytes)?;
        }

        attachments.push(AttachmentEntry {
            id: spill.id.clone(),
            hash: spill.hash.clone(),
            size: spill.size,
            mime: spill.mime.clone(),
            retention: spill.retention.as_str(),
            sha256: spill.hash.clone(),
            path: relative_path,
        });
    }

    let attachment_bytes: u64 = attachments.iter().map(|entry| entry.size).sum();
    let manifest = ExportManifest {
        format: EXPORT_FORMAT.to_owned(),
        schema_version: MANIFEST_SCHEMA_VERSION,
        session_id: session_id.clone(),
        exported_at: options.exported_at.clone(),
        event_count: counts.events,
        durable_event_count: counts.durable,
        ephemeral_event_count: counts.ephemeral,
        total_bytes: jsonl_entry.size + attachment_bytes,
        jsonl: jsonl_entry,
        attachments,
        missing_blobs,
    };

    // 3) manifest 最后写：内容里的 sha256 都已落盘。
    let text = serde_json::to_string_pretty(&manifest)?;
    fs::write(manifest_path(out_dir, &session_id), format!("{text}\n"))?;
    Ok(manifest)
}

/// 导出自校验结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub session_id: String,
    pub jsonl_ok: bool,
    pub attachments_checked: usize,
    pub attachments_ok: bool,
    /// manifest 里记录的缺失 blob 数。
    pub missing_blobs: usize,
    pub problems: Vec<String>,
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        self.jsonl_ok && self.attachments_ok && self.problems.is_empty()
    }
}

/// 按 manifest 校验一份导出：文件存在、大小与 sha256 一致。不修改任何文件。
pub fn verify_export(out_dir: &Path, session_id: &str) -> Result<VerifyReport, ExportError> {
    let path = manifest_path(out_dir, session_id);
    let text = fs::read_to_string(&path)?;
    let manifest: ExportManifest = serde_json::from_str(&text)?;
    if manifest.session_id != session_id {
        return Err(ExportError::ManifestSessionMismatch {
            expected: session_id.to_owned(),
            actual: manifest.session_id,
        });
    }

    let mut report = VerifyReport {
        session_id: session_id.to_owned(),
        jsonl_ok: false,
        attachments_checked: 0,
        attachments_ok: true,
        missing_blobs: manifest.missing_blobs.len(),
        problems: Vec::new(),
    };

    let jsonl_path = out_dir.join(&manifest.jsonl.path);
    match fs::read(&jsonl_path) {
        Ok(bytes) => {
            report.jsonl_ok = bytes.len() as u64 == manifest.jsonl.size
                && sha256_hex(&bytes) == manifest.jsonl.sha256;
            if !report.jsonl_ok {
                report
                    .problems
                    .push(format!("jsonl 校验失败: {}", manifest.jsonl.path));
            }
        }
        Err(err) => report
            .problems
            .push(format!("jsonl 读取失败: {} ({err})", manifest.jsonl.path)),
    }

    for attachment in &manifest.attachments {
        report.attachments_checked += 1;
        let path = out_dir.join(&attachment.path);
        match fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() as u64 != attachment.size || sha256_hex(&bytes) != attachment.sha256
                {
                    report.attachments_ok = false;
                    report
                        .problems
                        .push(format!("附件校验失败: {}", attachment.path));
                }
            }
            Err(err) => {
                report.attachments_ok = false;
                report
                    .problems
                    .push(format!("附件读取失败: {} ({err})", attachment.path));
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use dshr_agent_context::spill::{record_spill, Retention};
    use dshr_agent_context::PendingSpill;
    use dshr_session_store::EventSpec;
    use serde_json::json;

    fn spill_setup(tag: &str) -> (TempDir, SpillStore, SessionLog, SpillRef) {
        let temp = TempDir::new(tag);
        let store = SpillStore::new(temp.join("spill"));
        let content = json!([{ "type": "text", "text": "A".repeat(40_000) }]);
        let (spill, payload) =
            dshr_agent_context::spill::spill_content(&content, Retention::Days(90));
        store.put(&spill, &payload).expect("put spill");

        let pending = PendingSpill {
            call_id: "c1".to_owned(),
            spill: spill.clone(),
            payload,
            preview_chars: 64,
        };
        let mut log = SessionLog::new("s-export");
        log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": "u1", "text": "跑一下" }),
        ))
        .expect("user");
        log.append(EventSpec::durable(
            "tool.call",
            "t0",
            json!({ "call_id": "c1", "tool": "bash", "arguments": {} }),
        ))
        .expect("call");
        log.append(EventSpec::durable(
            "tool.result",
            "t0",
            json!({ "call_id": "c1", "is_error": false, "content": content.clone() }),
        ))
        .expect("result");
        record_spill(&mut log, &pending, "t1").expect("record spill");
        log.append(EventSpec::durable(
            "message.assistant.settled",
            "t1",
            json!({ "message_id": "a1", "text": "完成" }),
        ))
        .expect("assistant");
        (temp, store, log, spill)
    }

    #[test]
    fn export_writes_jsonl_attachments_and_manifest() {
        let (temp, store, log, spill) = spill_setup("full");
        let out_dir = temp.join("out");
        let options = ExportOptions::new("2026-11-11T12:00:00.000Z");
        let manifest = export_session(&log, &SpillStoreSource::new(&store), &out_dir, &options)
            .expect("export");

        assert_eq!(manifest.format, EXPORT_FORMAT);
        assert_eq!(manifest.session_id, "s-export");
        assert_eq!(manifest.event_count, log.len());
        assert_eq!(manifest.durable_event_count, log.len());
        assert_eq!(manifest.ephemeral_event_count, 0);
        assert_eq!(manifest.attachments.len(), 1);
        let attachment = &manifest.attachments[0];
        assert_eq!(attachment.hash, spill.hash);
        assert_eq!(attachment.sha256, spill.hash);
        assert_eq!(attachment.size, spill.size);
        assert_eq!(attachment.retention, "90d");
        assert!(out_dir.join(&attachment.path).is_file());
        assert!(manifest_path(&out_dir, "s-export").is_file());
        assert!(out_dir.join("s-export.jsonl").is_file());
        assert_eq!(
            manifest.total_bytes,
            manifest.jsonl.size + manifest.attachments[0].size
        );

        // JSONL 能原样反序列化回日志。
        let jsonl = fs::read_to_string(out_dir.join("s-export.jsonl")).expect("jsonl");
        let restored = SessionLog::from_jsonl("s-export", &jsonl).expect("restore");
        assert_eq!(restored.events(), log.events());

        let verify = verify_export(&out_dir, "s-export").expect("verify");
        assert!(verify.is_ok(), "{verify:?}");
        assert_eq!(verify.attachments_checked, 1);
        assert_eq!(verify.missing_blobs, 0);

        // 导出是只读的：日志本身没有变化。
        assert_eq!(log.len(), 6);
    }

    #[test]
    fn missing_blob_is_recorded_without_blocking() {
        let (temp, _store, log, spill) = spill_setup("missing");
        let out_dir = temp.join("out");
        let manifest = export_session(
            &log,
            &NoBlobs,
            &out_dir,
            &ExportOptions::new("2026-11-11T12:00:00.000Z"),
        )
        .expect("export");

        assert!(manifest.attachments.is_empty());
        assert_eq!(manifest.missing_blobs.len(), 1);
        assert_eq!(manifest.missing_blobs[0].id, spill.id);
        assert_eq!(manifest.missing_blobs[0].size, spill.size);
        let verify = verify_export(&out_dir, "s-export").expect("verify");
        assert!(verify.is_ok(), "{verify:?}");
        assert_eq!(verify.missing_blobs, 1);

        let err = export_session(
            &log,
            &NoBlobs,
            &out_dir,
            &ExportOptions::new("t").fail_on_missing_blob(true),
        )
        .expect_err("must fail");
        assert!(matches!(err, ExportError::MissingBlob { .. }), "{err:?}");
    }

    #[test]
    fn duplicate_spill_hash_exports_one_attachment() {
        let (temp, store, mut log, spill) = spill_setup("dedup");
        // 第二个工具结果内容完全相同：内容寻址只应导出一份附件。
        let (spill_again, payload_again) = dshr_agent_context::spill::spill_content(
            &json!([{ "type": "text", "text": "A".repeat(40_000) }]),
            Retention::Days(90),
        );
        assert_eq!(spill_again.hash, spill.hash);
        let pending = PendingSpill {
            call_id: "c2".to_owned(),
            spill: spill_again,
            payload: payload_again,
            preview_chars: 64,
        };
        record_spill(&mut log, &pending, "t2").expect("record");

        let out_dir = temp.join("out");
        let manifest = export_session(
            &log,
            &SpillStoreSource::new(&store),
            &out_dir,
            &ExportOptions::new("t"),
        )
        .expect("export");
        assert_eq!(manifest.attachments.len(), 1, "{manifest:?}");
        let mut attachment_files = 0;
        for entry in fs::read_dir(out_dir.join("s-export.attachments")).expect("dir") {
            let _ = entry.expect("entry");
            attachment_files += 1;
        }
        assert_eq!(attachment_files, 1);
    }

    #[test]
    fn ephemeral_events_are_excluded_by_default() {
        let (temp, store, mut log, _spill) = spill_setup("ephemeral");
        log.append(EventSpec::ephemeral(
            "message.assistant.delta",
            "t2",
            json!({ "message_id": "a1", "text": "流式片段" }),
        ))
        .expect("delta");
        let out_dir = temp.join("out");

        let manifest = export_session(
            &log,
            &SpillStoreSource::new(&store),
            &out_dir,
            &ExportOptions::new("t"),
        )
        .expect("export");
        assert_eq!(manifest.ephemeral_event_count, 1);
        assert_eq!(manifest.event_count, log.len() - 1);
        let jsonl = fs::read_to_string(out_dir.join("s-export.jsonl")).expect("jsonl");
        assert!(!jsonl.contains("message.assistant.delta"));

        // 显式包含时写进 JSONL，但 from_jsonl 需要完整 seq：ephemeral 只出现在末尾，安全。
        let out_dir_all = temp.join("out-all");
        let manifest_all = export_session(
            &log,
            &SpillStoreSource::new(&store),
            &out_dir_all,
            &ExportOptions::new("t").include_ephemeral(true),
        )
        .expect("export all");
        assert_eq!(manifest_all.event_count, log.len());
        assert_eq!(manifest_all.ephemeral_event_count, 1);
    }

    #[test]
    fn tampered_attachment_is_detected_by_verify() {
        let (temp, store, log, spill) = spill_setup("tamper");
        let out_dir = temp.join("out");
        export_session(
            &log,
            &SpillStoreSource::new(&store),
            &out_dir,
            &ExportOptions::new("t"),
        )
        .expect("export");

        let path = out_dir.join("s-export.attachments").join(&spill.hash);
        fs::write(&path, b"tampered").expect("tamper");
        let verify = verify_export(&out_dir, "s-export").expect("verify");
        assert!(!verify.is_ok(), "{verify:?}");
        assert!(!verify.attachments_ok);
        assert!(!verify.problems.is_empty());
    }

    #[test]
    fn wrong_blob_source_content_is_rejected() {
        struct BadSource;
        impl BlobSource for BadSource {
            fn read(&self, spill: &SpillRef) -> io::Result<Option<Vec<u8>>> {
                Ok(Some(vec![b'x'; spill.size as usize]))
            }
        }

        let (temp, _store, log, _spill) = spill_setup("bad");
        let err = export_session(
            &log,
            &BadSource,
            &temp.join("out"),
            &ExportOptions::new("t"),
        )
        .expect_err("integrity error");
        assert!(matches!(err, ExportError::BlobIntegrity { .. }), "{err:?}");
    }

    #[test]
    fn re_export_reuses_verified_attachments() {
        let (temp, store, log, spill) = spill_setup("reexport");
        let out_dir = temp.join("out");
        let options = ExportOptions::new("t1");
        let first = export_session(&log, &SpillStoreSource::new(&store), &out_dir, &options)
            .expect("first export");
        let second = export_session(&log, &SpillStoreSource::new(&store), &out_dir, &options)
            .expect("second export");
        assert_eq!(first, second);
        assert_eq!(second.attachments[0].hash, spill.hash);
    }
}
