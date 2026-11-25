use super::{adc_tool_views::valid_readonly_tree, ChatState};
use crate::dsh_deliverables;
use dshr_conversation_model::{build_from_log, Item};
use dshr_plugin_host::{SlotTarget, VIEWER_TAB};
use dshr_session_store::ToolState;
use serde_json::Value;
use std::path::Path;
use std::sync::Mutex;

pub(crate) fn render_delivered_file(
    state: &Mutex<ChatState>,
    session: &str,
    call_seq: u64,
    selected: &Path,
    target: SlotTarget,
) -> Result<Value, String> {
    let inactive = || "文件预览已失效".to_owned();
    let file_type = dsh_deliverables::file_type(selected).ok_or_else(inactive)?;
    let workspace = {
        let state = state.lock().unwrap_or_else(|error| error.into_inner());
        if state.session_id != session || state.file_view_is_trial(&file_type, target).is_none()
            || !dsh_deliverables::replay(&state.log)
                .get(&call_seq)
                .is_some_and(|files| files.iter().any(|file| file.path == selected))
            || !build_from_log(&state.log).into_iter().any(|item| {
                matches!(item, Item::Tool(tool) if tool.call_seq == call_seq && tool.tool == "present"
                    && tool.origin == "sidecar" && matches!(tool.state, ToolState::Settled { is_error: false }))
            })
        {
            return Err(inactive());
        }
        state.workspace.clone()
    };
    let context =
        dsh_deliverables::file_preview(&workspace, selected).map_err(|error| error.message)?;
    let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
    if state.session_id != session || state.workspace != workspace {
        return Err(inactive());
    }
    let host = if state
        .file_view_is_trial(&file_type, target)
        .ok_or_else(inactive)?
    {
        &mut state
            .creator_trials
            .get_mut(session)
            .ok_or_else(inactive)?
            .plugins
    } else {
        &mut state.plugins
    };
    let tree = host
        .render_file_slot(target, &context)
        .map_err(|_| "插件未能生成文件预览".to_owned())?;
    if !valid_readonly_tree(&tree) {
        return Err("插件返回了无效的文件预览".to_owned());
    }
    Ok(tree)
}

impl ChatState {
    fn file_view_is_trial(&self, file_type: &str, target: SlotTarget) -> Option<bool> {
        let view = self.adc_panels.iter().find(|view| {
            view.slot.as_ref().is_some_and(|record| {
                record.slot_id == VIEWER_TAB
                    && record.key.as_deref() == Some(file_type)
                    && SlotTarget::from(record) == target
            })
        })?;
        let host = if view.trial {
            &self.creator_trials.get(&self.session_id)?.plugins
        } else {
            &self.plugins
        };
        host.active_slot(target).map(|_| view.trial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatRuntime, DesktopModel, DesktopSettings};
    use dshr_plugin_api::{Plugin, PluginContext, PluginError};
    use dshr_session_store::{EventSpec, SessionIndex};
    use dshr_tool_runtime::ApprovalBroker;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn requires_a_delivered_file_from_the_current_session_and_live_registration() {
        struct Viewer(Arc<AtomicUsize>);

        impl Plugin for Viewer {
            fn id(&self) -> &str {
                "fixture.viewer"
            }

            fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
                ctx.register_slots([json!({"slot_id": VIEWER_TAB, "plugin_id": self.id(), "key": "txt", "view": "plain"})]);
                Ok(())
            }

            fn call(&mut self, _: &str, _: &Value) -> Result<Value, PluginError> {
                unreachable!()
            }

            fn render_file_view(
                &mut self,
                name: &str,
                context: &Value,
            ) -> Result<Value, PluginError> {
                assert_eq!(name, "plain");
                assert_eq!(context["name"], "report.TXT");
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(json!({"type": "Text", "props": {"value": context["text"]}}))
            }
        }

        let root = tempfile::tempdir().unwrap();
        let store = SessionIndex::new(root.path().join("sessions"));
        store
            .create_root("preview", &root.path().to_string_lossy(), "1")
            .unwrap();
        let mut state = ChatState::open_in(
            store.path_for("preview"),
            "preview",
            &Arc::new(AtomicBool::new(false)),
            ApprovalBroker::noninteractive(),
            DesktopSettings {
                model: Arc::new(DesktopModel::Mock),
                permission: None,
                reasoning: None,
            },
            ChatRuntime::Desktop {
                allow_spawn: false,
                sidecar: false,
                diagnostics: false,
            },
        );
        std::fs::write(root.path().join("report.TXT"), "delivered content").unwrap();
        std::fs::write(root.path().join("unshared.txt"), "private in workspace").unwrap();
        state
            .log
            .append(EventSpec::durable(
                "turn.started",
                "1",
                json!({"turn_id": "turn-1"}),
            ))
            .unwrap();
        let call_seq = state.log.len() as u64 + 1;
        state
            .log
            .append(EventSpec::durable(
                "tool.call",
                "1",
                json!({"call_id":"a", "call_seq":call_seq, "tool":"present", "origin":"sidecar", "arguments":{}}),
            ))
            .unwrap();
        state
            .log
            .append(EventSpec::durable(
                dsh_deliverables::PRESENTED_EVENT,
                "1",
                json!({"turn":1,"callId":"a","files":[{"path":"report.TXT"}]}),
            ))
            .unwrap();
        state.log.append(EventSpec::durable("tool.result", "1", json!({"call_id":"a", "call_seq":call_seq,"is_error":false,"content":[],"value":null}))).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        state.plugins.load(Box::new(Viewer(calls.clone()))).unwrap();
        state.refresh_adc_panels();
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let slot = state.plugins.slot_registrations(VIEWER_TAB)[0];
        let target = SlotTarget::from(slot);
        let state = Mutex::new(state);
        let report = Path::new("report.TXT");
        assert!(render_delivered_file(&state, "other", call_seq, report, target).is_err());
        assert!(render_delivered_file(
            &state,
            "preview",
            call_seq,
            Path::new("unshared.txt"),
            target
        )
        .is_err());
        let tree = render_delivered_file(&state, "preview", call_seq, report, target).unwrap();
        assert_eq!(tree["props"]["value"], "delivered content");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        state
            .lock()
            .unwrap()
            .plugins
            .unload("fixture.viewer")
            .unwrap();
        assert!(render_delivered_file(&state, "preview", call_seq, report, target).is_err());
    }
}
