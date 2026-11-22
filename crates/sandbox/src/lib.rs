//! 工作区沙箱 seam（M2）：路径归一化、写前快照、大小限制。
//!
//! 这不是 DSH 的完整 sandbox service，而是我们自己的最小安全底座：
//! - **路径根**：所有相对路径都落在工作区内，绝对路径和 `..` 一律拒绝；
//! - **写前快照**：覆盖已有文件前先复制到 `.dshr/snapshots/`，可恢复；
//! - **大小限制**：单次写入 ≤ 1 MiB，防止模型一次写爆磁盘；
//! - **原子写**：先写同目录临时文件再 rename，避免半截文件。
//!
//! 官方 `write`/`edit`/`bash` 仍走 DSH 的 sandbox service（暂未接），
//! 但原生工具可以先复用这个 seam，保证"能写"与"写得安全"同时成立。

#![forbid(unsafe_code)]

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

/// 单次写入上限（1 MiB）。
pub const MAX_WRITE_BYTES: u64 = 1_048_576;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SandboxError {
    #[error("路径必须相对工作区，拒绝绝对路径: {0}")]
    AbsolutePath(String),
    #[error("路径逃逸工作区: {0}")]
    Escape(String),
    #[error("路径包含非法组件: {0}")]
    InvalidPath(String),
    #[error("写入超过大小限制: {size} > {limit}")]
    TooLarge { size: u64, limit: u64 },
    #[error("父目录不存在: {0}")]
    MissingParent(String),
    #[error("文件已存在: {0}")]
    AlreadyExists(String),
    #[error("io: {0}")]
    Io(String),
}

/// 写前快照：原始文件 + `.dshr/snapshots/` 里的副本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRef {
    pub id: String,
    pub source: PathBuf,
    pub stored: PathBuf,
}

/// 一次成功写入的回执（工具返回值 / 审计用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteReceipt {
    pub path: PathBuf,
    pub bytes: u64,
    pub snapshot: Option<SnapshotRef>,
}

/// 工作区：路径根 + 快照目录。
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    /// 规范化后的根（符号链接/短路径），只用于逃逸检查。
    canonical_root: PathBuf,
    snapshots: PathBuf,
}

impl Workspace {
    /// 打开工作区；根目录不存在会创建。
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, SandboxError> {
        let requested = root.into();
        fs::create_dir_all(&requested).map_err(io)?;
        let root = std::path::absolute(&requested).map_err(io)?;
        let canonical_root = fs::canonicalize(&root).map_err(io)?;
        let snapshots = root.join(".dshr").join("snapshots");
        fs::create_dir_all(&snapshots).map_err(io)?;
        Ok(Self {
            root,
            canonical_root,
            snapshots,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn snapshots_dir(&self) -> &Path {
        &self.snapshots
    }

    /// 把模型给的相对路径解析为工作区内路径；拒绝绝对路径与 `..`。
    pub fn resolve(&self, requested: &str) -> Result<PathBuf, SandboxError> {
        if requested.trim().is_empty() {
            return Err(SandboxError::InvalidPath(String::from("<empty>")));
        }
        let candidate = Path::new(requested);
        if candidate.is_absolute() {
            return Err(SandboxError::AbsolutePath(requested.to_owned()));
        }
        let mut resolved = self.root.clone();
        for component in candidate.components() {
            match component {
                Component::Normal(part) => resolved.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(SandboxError::Escape(requested.to_owned()));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(SandboxError::AbsolutePath(requested.to_owned()));
                }
            }
        }
        // 父目录必须存在，并且（规范化后）仍在工作区内：防符号链接逃逸。
        let parent = resolved
            .parent()
            .ok_or_else(|| SandboxError::InvalidPath(requested.to_owned()))?;
        if !parent.exists() {
            return Err(SandboxError::MissingParent(parent.display().to_string()));
        }
        let canonical_parent = fs::canonicalize(parent).map_err(io)?;
        if !canonical_parent.starts_with(&self.canonical_root) {
            return Err(SandboxError::Escape(requested.to_owned()));
        }
        Ok(resolved)
    }

    fn resolve_creating_parents(&self, requested: &str) -> Result<PathBuf, SandboxError> {
        if requested.trim().is_empty() {
            return Err(SandboxError::InvalidPath(String::from("<empty>")));
        }
        let mut relative = PathBuf::new();
        for component in Path::new(requested).components() {
            match component {
                Component::Normal(part) if valid_component(part) => relative.push(part),
                Component::Normal(_) => {
                    return Err(SandboxError::InvalidPath(requested.to_owned()))
                }
                Component::CurDir => {}
                Component::ParentDir => return Err(SandboxError::Escape(requested.to_owned())),
                Component::RootDir | Component::Prefix(_) => {
                    return Err(SandboxError::AbsolutePath(requested.to_owned()))
                }
            }
        }
        let parent = relative
            .parent()
            .ok_or_else(|| SandboxError::InvalidPath(requested.to_owned()))?;
        if fs::canonicalize(&self.root).map_err(io)? != self.canonical_root {
            return Err(SandboxError::Escape(requested.to_owned()));
        }

        let path = self.root.join(&relative);
        let mut directory = self.root.clone();
        for part in parent {
            directory.push(part);
            let metadata = match fs::symlink_metadata(&directory) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    match fs::create_dir(&directory) {
                        Ok(()) => {}
                        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(io(error)),
                    }
                    fs::symlink_metadata(&directory).map_err(io)?
                }
                Err(error) => return Err(io(error)),
            };
            if is_link(&metadata)
                || !fs::canonicalize(&directory)
                    .map_err(io)?
                    .starts_with(&self.canonical_root)
            {
                return Err(SandboxError::Escape(requested.to_owned()));
            }
            if !metadata.is_dir() {
                return Err(SandboxError::InvalidPath(directory.display().to_string()));
            }
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) if is_link(&metadata) => Err(SandboxError::Escape(requested.to_owned())),
            Ok(metadata) if !metadata.is_file() && !metadata.is_dir() => {
                Err(SandboxError::InvalidPath(requested.to_owned()))
            }
            Ok(_) => Ok(path),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(path),
            Err(error) => Err(io(error)),
        }
    }

    /// 写入文本：路径校验 → 大小限制 → 写前快照 → 原子写。
    pub fn write_text(&self, requested: &str, content: &str) -> Result<WriteReceipt, SandboxError> {
        self.write_text_inner(requested, content, false)
    }

    pub fn create_text(
        &self,
        requested: &str,
        content: &str,
    ) -> Result<WriteReceipt, SandboxError> {
        let bytes = content.len() as u64;
        if bytes > MAX_WRITE_BYTES {
            return Err(SandboxError::TooLarge {
                size: bytes,
                limit: MAX_WRITE_BYTES,
            });
        }
        let path = self.resolve(requested)?;
        let parent = path.parent().expect("resolved path has a parent");
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o666));
        }
        let mut temporary = builder
            .prefix(".dshr-create-")
            .tempfile_in(parent)
            .map_err(io)?;
        temporary.write_all(content.as_bytes()).map_err(io)?;
        temporary.persist_noclobber(&path).map_err(|error| {
            if error.error.kind() == ErrorKind::AlreadyExists {
                SandboxError::AlreadyExists(path.display().to_string())
            } else {
                io(error.error)
            }
        })?;
        Ok(WriteReceipt {
            path,
            bytes,
            snapshot: None,
        })
    }

    /// 原生写入入口。调用方批准写入后，可创建缺失的父目录。
    pub fn write_text_creating_parents(
        &self,
        requested: &str,
        content: &str,
    ) -> Result<WriteReceipt, SandboxError> {
        self.write_text_inner(requested, content, true)
    }

    fn write_text_inner(
        &self,
        requested: &str,
        content: &str,
        create_parents: bool,
    ) -> Result<WriteReceipt, SandboxError> {
        let bytes = content.len() as u64;
        if bytes > MAX_WRITE_BYTES {
            return Err(SandboxError::TooLarge {
                size: bytes,
                limit: MAX_WRITE_BYTES,
            });
        }
        let path = if create_parents {
            self.resolve_creating_parents(requested)?
        } else {
            self.resolve(requested)?
        };
        let snapshot = if path.exists() {
            Some(self.snapshot(&path)?)
        } else {
            None
        };

        // 同目录临时文件 + rename：避免半截文件。
        let tmp = path.with_extension(format!("dshr-tmp-{}", unix_millis()));
        fs::write(&tmp, content).map_err(io)?;
        fs::rename(&tmp, &path).map_err(io)?;

        Ok(WriteReceipt {
            path,
            bytes,
            snapshot,
        })
    }

    /// 把已有文件复制到快照目录。
    pub fn snapshot(&self, path: &Path) -> Result<SnapshotRef, SandboxError> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| SandboxError::InvalidPath(path.display().to_string()))?;
        let id = format!("{}-{}", unix_millis(), sanitize(name));
        let stored = self.snapshots.join(&id);
        fs::copy(path, &stored).map_err(io)?;
        Ok(SnapshotRef {
            id,
            source: path.to_owned(),
            stored,
        })
    }

    /// 用快照恢复原文件（失败返回 io 错误）。
    pub fn restore(&self, snapshot: &SnapshotRef) -> Result<(), SandboxError> {
        fs::copy(&snapshot.stored, &snapshot.source).map_err(io)?;
        Ok(())
    }
}

#[cfg(windows)]
fn valid_component(part: &std::ffi::OsStr) -> bool {
    let name = part.to_string_lossy();
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    !name.ends_with(['.', ' '])
        && !name.chars().any(|ch| {
            ch.is_ascii_control() || matches!(ch, ':' | '"' | '<' | '>' | '|' | '?' | '*')
        })
        && !matches!(
            stem.as_str(),
            "NUL" | "CON" | "PRN" | "AUX" | "CONIN$" | "CONOUT$"
        )
        && !matches!(
            stem.strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT")),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
        )
}

#[cfg(not(windows))]
fn valid_component(part: &std::ffi::OsStr) -> bool {
    !part.as_encoded_bytes().contains(&0)
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn io(err: std::io::Error) -> SandboxError {
    SandboxError::Io(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(name: &str) -> Workspace {
        let dir = std::env::temp_dir().join(format!(
            "dshr-sandbox-{name}-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        Workspace::new(dir).expect("workspace")
    }

    #[test]
    fn writes_inside_workspace_and_reports_receipt() {
        let ws = temp_workspace("write");
        let receipt = ws.write_text("notes.txt", "hello").expect("write");
        assert_eq!(receipt.bytes, 5);
        assert!(receipt.snapshot.is_none());
        assert_eq!(
            fs::read_to_string(ws.root().join("notes.txt")).unwrap(),
            "hello"
        );
        fs::remove_dir_all(ws.root()).ok();
    }

    #[test]
    fn overwrite_creates_snapshot_and_restore_works() {
        let ws = temp_workspace("snapshot");
        ws.write_text("a.txt", "v1").unwrap();
        let receipt = ws.write_text("a.txt", "v2").unwrap();
        let snapshot = receipt.snapshot.expect("snapshot");
        assert!(snapshot.stored.exists());
        assert_eq!(fs::read_to_string(ws.root().join("a.txt")).unwrap(), "v2");
        ws.restore(&snapshot).unwrap();
        assert_eq!(fs::read_to_string(ws.root().join("a.txt")).unwrap(), "v1");
        fs::remove_dir_all(ws.root()).ok();
    }

    #[test]
    fn rejects_absolute_and_escaping_paths() {
        let ws = temp_workspace("escape");
        let absolute = if cfg!(windows) {
            "C:\\Windows\\evil.txt"
        } else {
            "/etc/evil.txt"
        };
        assert!(matches!(
            ws.write_text(absolute, "x"),
            Err(SandboxError::AbsolutePath(_))
        ));
        assert!(matches!(
            ws.write_text("../outside.txt", "x"),
            Err(SandboxError::Escape(_))
        ));
        assert!(matches!(
            ws.write_text("sub/../../outside.txt", "x"),
            Err(SandboxError::Escape(_))
        ));
        fs::remove_dir_all(ws.root()).ok();
    }

    #[test]
    fn strict_and_oversized_writes_create_no_directories() {
        let ws = temp_workspace("limits");
        assert!(matches!(
            ws.write_text("no-such-dir/file.txt", "x"),
            Err(SandboxError::MissingParent(_))
        ));
        let big = "x".repeat((MAX_WRITE_BYTES + 1) as usize);
        assert!(matches!(
            ws.write_text_creating_parents("no-such-dir/big.txt", &big),
            Err(SandboxError::TooLarge { .. })
        ));
        assert!(!ws.root().join("no-such-dir").exists());
        fs::remove_dir_all(ws.root()).ok();
    }

    #[cfg(windows)]
    #[test]
    fn rejects_invalid_components_before_creating_any_parents() {
        let ws = temp_workspace("invalid-component");
        for path in [
            "new/src/NUL.txt",
            "new/src/file:stream",
            "new/src/file?.txt",
        ] {
            assert!(matches!(
                ws.write_text_creating_parents(path, "x"),
                Err(SandboxError::InvalidPath(_))
            ));
        }
        assert!(!ws.root().join("new").exists());
        fs::remove_dir_all(ws.root()).ok();
    }

    #[test]
    fn writes_into_new_nested_directories() {
        let ws = temp_workspace("nested");
        let receipt = ws
            .write_text_creating_parents("plugin/src/main.rs", "nested")
            .unwrap();
        assert_eq!(receipt.bytes, 6);
        assert_eq!(
            fs::read_to_string(ws.root().join("plugin/src/main.rs")).unwrap(),
            "nested"
        );
        fs::remove_dir_all(ws.root()).ok();
    }
}
