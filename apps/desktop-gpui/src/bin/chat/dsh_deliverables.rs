use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use dshr_compat_dsh_host::ServiceError;
use dshr_plugin_host::FileViewContext;
use dshr_session_store::SessionLog;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) const PRESENTED_EVENT: &str = "deliverables/presented";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeliveredFile {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Delivery {
    pub turn: u64,
    pub call_id: String,
    pub files: Vec<DeliveredFile>,
}

pub(super) fn checked_file(
    workspace: &Path,
    path: &Path,
) -> Result<(PathBuf, PathBuf), ServiceError> {
    let error = |message| ServiceError::new("E_SERVICE_DENIED", message);
    let workspace = workspace
        .canonicalize()
        .map_err(|_| error("file delivery workspace is unavailable"))?;
    let path = workspace
        .join(path)
        .canonicalize()
        .map_err(|_| error("delivered file is unavailable"))?;
    let relative = path
        .strip_prefix(&workspace)
        .map_err(|_| error("delivered file is outside the workspace"))?;
    if relative.as_os_str().is_empty() || !path.is_file() {
        return Err(error("delivery requires a regular file"));
    }
    let relative = relative.to_path_buf();
    Ok((path, relative))
}

pub(super) fn file_type(path: &Path) -> Option<String> {
    let kind = path.extension()?.to_str()?.to_ascii_lowercase();
    FileViewContext::valid_file_type(&kind).then_some(kind)
}

pub(super) fn file_preview(
    workspace: &Path,
    selected: &Path,
) -> Result<FileViewContext, ServiceError> {
    let unavailable = || ServiceError::new("E_SERVICE_DENIED", "交付文件暂时无法预览");
    let kind = file_type(selected).ok_or_else(unavailable)?;
    let name = selected
        .file_name()
        .ok_or_else(unavailable)?
        .to_string_lossy()
        .into_owned();
    let (path, _) = checked_file(workspace, selected)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(&path).map_err(|_| unavailable())?;
    let metadata = file.metadata().map_err(|_| unavailable())?;
    if !metadata.is_file() || opened_path(&file).ok().as_ref() != Some(&path) {
        return Err(unavailable());
    }
    let limit = FileViewContext::MAX_TEXT_BYTES;
    let mut prefix = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut prefix)
        .map_err(|_| unavailable())?;
    let truncated = prefix.len() > limit;
    prefix.truncate(limit);
    let text = match std::str::from_utf8(&prefix) {
        Ok(text) => Some(text.to_owned()),
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&prefix[..error.valid_up_to()])
                .ok()
                .map(str::to_owned)
        }
        Err(_) => None,
    };
    Ok(FileViewContext {
        name,
        file_type: kind,
        size_bytes: metadata.len(),
        text,
        truncated,
    })
}

#[cfg(windows)]
fn opened_path(file: &File) -> io::Result<PathBuf> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    let mut path = vec![0u16; 32768];
    // The buffer covers a long Win32 path and the File keeps its handle alive.
    let len = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            0,
        )
    } as usize;
    if len == 0 || len >= path.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(std::ffi::OsString::from_wide(&path[..len]).into())
}

#[cfg(target_os = "linux")]
fn opened_path(file: &File) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(target_os = "macos")]
fn opened_path(file: &File) -> io::Result<PathBuf> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let mut path = [0u8; libc::PATH_MAX as usize];
    // F_GETPATH writes a NUL-terminated path into a PATH_MAX-sized buffer.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, path.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let path = std::ffi::CStr::from_bytes_until_nul(&path).map_err(io::Error::other)?;
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn opened_path(_: &File) -> io::Result<PathBuf> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(super) fn normalize_event(workspace: &Path, data: Value) -> Result<Value, ServiceError> {
    let mut delivery: Delivery = serde_json::from_value(data)
        .map_err(|error| ServiceError::args("/data", error.to_string()))?;
    if !(1..=8).contains(&delivery.files.len()) {
        return Err(ServiceError::args(
            "/data/files",
            "delivery supports 1 to 8 files",
        ));
    }
    for file in &mut delivery.files {
        if !file.path.is_absolute() {
            return Err(ServiceError::args(
                "/data/files/path",
                "expected resolved path",
            ));
        }
        if file
            .description
            .as_ref()
            .is_some_and(|text| text.chars().count() > 512)
        {
            return Err(ServiceError::args(
                "/data/files/description",
                "description exceeds 512 characters",
            ));
        }
        let (_, relative) = checked_file(workspace, &file.path)?;
        file.path = relative;
    }
    serde_json::to_value(delivery)
        .map_err(|error| ServiceError::new("E_INTERNAL", error.to_string()))
}

pub(super) fn replay(log: &SessionLog) -> HashMap<u64, Vec<DeliveredFile>> {
    let mut finished = HashMap::new();
    let mut active = HashMap::new();
    let mut turn = 0;
    let mut in_turn = false;
    for event in log.events() {
        match event.event_type.as_str() {
            "turn.started" => {
                turn += 1;
                in_turn = true;
                active.clear();
            }
            "turn.ended" => in_turn = false,
            "tool.call" if in_turn && event.payload["tool"] == "present" => {
                if let Some(call_id) = event.payload["call_id"].as_str() {
                    active.insert(call_id, event.seq);
                }
            }
            PRESENTED_EVENT if in_turn => {
                if let Ok(delivery) = serde_json::from_value::<Delivery>(event.payload.clone()) {
                    if let Some(&seq) = active.get(delivery.call_id.as_str()) {
                        if delivery.turn == turn {
                            finished.entry(seq).or_insert(delivery.files);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    finished
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_checks_workspace_and_bounds_utf8_on_the_open_handle() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let limit = FileViewContext::MAX_TEXT_BYTES;
        let contents = format!("{}数据", "a".repeat(limit - 1));
        std::fs::write(root.path().join("report.TXT"), &contents).unwrap();
        std::fs::write(root.path().join("binary.txt"), [0xff]).unwrap();
        let other = outside.path().join("outside.txt");
        std::fs::write(&other, "outside").unwrap();
        let preview = file_preview(root.path(), Path::new("report.TXT")).unwrap();
        assert_eq!(preview.name, "report.TXT");
        assert_eq!(preview.file_type, "txt");
        assert_eq!(preview.size_bytes, contents.len() as u64);
        assert!(preview.truncated);
        assert_eq!(preview.text.unwrap().len(), limit - 1);
        assert!(file_preview(root.path(), Path::new("binary.txt"))
            .unwrap()
            .text
            .is_none());
        assert!(file_preview(root.path(), &other).is_err());
        assert!(file_preview(root.path(), Path::new("missing.txt")).is_err());
    }
}
