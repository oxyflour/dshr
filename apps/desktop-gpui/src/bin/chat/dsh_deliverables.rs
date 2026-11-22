use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dshr_compat_dsh_host::ServiceError;
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
