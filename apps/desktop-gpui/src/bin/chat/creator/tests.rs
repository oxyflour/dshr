#![cfg(test)]

use super::*;
use crate::{ChatRuntime, ChatState, DesktopModel, DesktopSettings, PermissionMode};
use dshr_agent_core::{CompletionRequest, ModelProvider, ModelReply, ModelToolCall};
use dshr_session_store::{EventSpec, SessionCreated, SessionIndex, SpawnSource};
use std::sync::Arc;
use std::time::Instant;

fn call(tool: &str, arguments: Value) -> ToolCallRequest {
    ToolCallRequest {
        call_id: format!("test-{tool}"),
        tool: tool.to_owned(),
        arguments,
    }
}

#[test]
fn trial_panel_follows_the_latest_success_even_when_call_ids_are_reused() {
    let mut log = SessionLog::new("creator");
    let mut settle = |tool: &str, plugin: &str, is_error: bool| {
        let call_seq = log.len() as u64 + 1;
        log.append(EventSpec::durable(
            "tool.call",
            "1",
            json!({ "call_id": "reused", "call_seq": call_seq, "tool": tool, "origin": "native" }),
        ))
        .unwrap();
        log.append(EventSpec::durable(
            "tool.result",
            "1",
            json!({
                "call_id": "reused",
                "call_seq": call_seq,
                "is_error": is_error,
                "value": { "plugin": plugin, "panels": ["Panel"] }
            }),
        ))
        .unwrap();
        call_seq
    };

    settle(TRY_PLUGIN_TOOL, "demo.first", false);
    let other = settle(TRY_PLUGIN_TOOL, "demo.second", false);
    let replacement = settle(TRY_PLUGIN_TOOL, "demo.first", false);
    settle(TRY_PLUGIN_TOOL, "demo.first", true);
    settle("unrelated", "demo.first", false);

    assert_eq!(
        trial_panel_calls(&log),
        HashMap::from([("demo.first", replacement), ("demo.second", other)])
    );
    assert!(crate::rows_snapshot(&log, None)[0].has_tool_call(replacement));
}

fn state(root: &Path, approvals: Arc<ApprovalBroker>) -> ChatState {
    let store = SessionIndex::new(root.join("sessions"));
    let created = SessionCreated::root(root.to_string_lossy().into_owned()).with_preset(PRESET);
    store.create_with_created("creator", &created, "1").unwrap();
    ChatState::open_with_settings(
        store.path_for("creator"),
        "creator",
        &Arc::new(AtomicBool::new(false)),
        approvals,
        false,
        false,
        DesktopSettings {
            model: Arc::new(DesktopModel::Mock),
            permission: Some(PermissionMode::DangerFullAccess),
            reasoning: None,
        },
    )
}

// CPP-CREATOR-001
#[test]
fn creator_preset_is_restored_and_kept_out_of_other_sessions() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path(), ApprovalBroker::noninteractive());
    let inspect = call(INSPECT_TOOL, json!({}));
    assert_eq!(state.execute_tool(&inspect).value["preset"], PRESET);

    let mut child = SessionLog::new("child");
    let created = SessionCreated::child("ws", "creator", SpawnSource::Agent).with_preset(PRESET);
    child.append_created("1", &created).unwrap();
    assert!(!is_creator_session(&child));

    let mut headless = ChatState::from_log(
        root.path().join("headless.jsonl"),
        state.log.clone(),
        root.path().to_owned(),
        &Arc::new(AtomicBool::new(false)),
        ApprovalBroker::noninteractive(),
        state.settings.clone(),
        ChatRuntime::Headless,
    );
    let write = call(
        WRITE_FILE_TOOL,
        json!({ "path": "test.txt", "content": "test" }),
    );
    assert!(headless.execute_tool(&inspect).is_error);
    assert!(headless.execute_tool(&write).is_error);
    let build = call(BUILD_PLUGIN_TOOL, json!({ "path": "plugin" }));
    assert!(headless.execute_tool(&build).is_error);

    let store = SessionIndex::new(root.path().join("sessions"));
    store
        .create_root("standard", &root.path().to_string_lossy(), "1")
        .unwrap();
    state.switch_session(store.path_for("standard"), "standard");
    assert!(state.execute_tool(&inspect).is_error);
    assert!(state.execute_tool(&write).is_error);
    assert!(state.execute_tool(&build).is_error);
    assert!(!root.path().join("test.txt").exists());
    state.switch_session(store.path_for("creator"), "creator");
    assert_eq!(state.execute_tool(&inspect).value["preset"], PRESET);
}

// M3-PRESET-006
#[test]
fn blank_mode_changes_in_place_and_started_session_refuses_changes() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path(), ApprovalBroker::noninteractive());
    let path = state.path.clone();
    let id = state.session_id.clone();
    let created_ts = state.log.events()[0].ts.clone();
    let inspect = call(INSPECT_TOOL, json!({}));

    let writer = dshr_session_store::try_lock_session(&path).unwrap();
    assert!(state.select_blank_preset(None).is_err());
    drop(writer);

    state.select_blank_preset(None).unwrap();
    assert!(state.execute_tool(&inspect).is_error);
    state.select_blank_preset(Some(PRESET.to_owned())).unwrap();
    assert_eq!(state.execute_tool(&inspect).value["preset"], PRESET);

    let restored = SessionLog::from_jsonl(&id, &fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(restored.events().len(), 1);
    assert_eq!(restored.events()[0].ts, created_ts);
    assert_eq!(
        restored.created().unwrap().workspace_id,
        root.path().to_string_lossy()
    );
    assert_eq!(restored.created().unwrap().preset.as_deref(), Some(PRESET));

    state.stream_turn("你好", &mut |_| {}).unwrap();
    let before = fs::read_to_string(&path).unwrap();
    assert!(state.select_blank_preset(None).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
}

// CPP-CREATOR-010
#[test]
fn creator_guidance_matches_the_manifest_parser_and_trial_errors() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path(), ApprovalBroker::noninteractive());
    let result = state.execute_tool(&call(INSPECT_TOOL, json!({})));
    let supported = result.value["adc_slots"]["supported"].as_array().unwrap();
    assert_eq!(supported.len(), dshr_plugin_host::desktop_slots().count());
    let catalog = dshr_ui_contributions::SlotCatalog::builtin();
    let available: std::collections::BTreeSet<_> = supported
        .iter()
        .map(|descriptor| descriptor["slot_id"].as_str().unwrap())
        .chain([result.value["panel_slot"].as_str().unwrap()])
        .collect();
    assert_eq!(
        available,
        catalog
            .all()
            .iter()
            .map(|spec| spec.slot_id.as_str())
            .collect()
    );
    assert_eq!(
        result.value["adc_slots"]["menu_keys"],
        json!(dshr_plugin_host::DESKTOP_MENU_KEYS)
    );
    let mut slots = dshr_ui_contributions::ContributionRegistry::new();
    slots
        .set_closed_key_domain(
            dshr_plugin_host::MENU_ITEM_SLOT,
            dshr_plugin_host::DESKTOP_MENU_KEYS
                .iter()
                .map(|key| (*key).to_owned()),
        )
        .unwrap();
    for descriptor in supported {
        assert!(descriptor["location"].is_string());
        assert_eq!(descriptor["slot_id"], descriptor["example"]["slot_id"]);
        slots.register(&descriptor["example"], 1).unwrap();
    }
    let guidance = &result.value["manifest"];
    let external = guidance["examples"]["external-process"].as_str().unwrap();
    let wasm = guidance["examples"]["wasm-component"].as_str().unwrap();
    for (example, kind, entry) in [
        (
            external,
            RuntimeKind::ExternalProcess,
            if cfg!(windows) {
                "bin/hello-world.exe"
            } else {
                "bin/hello-world"
            },
        ),
        (wasm, RuntimeKind::WasmComponent, "plugin.wasm"),
    ] {
        let manifest = Manifest::from_toml_str(example).unwrap();
        manifest.validate(&crate::host_sdk_version()).unwrap();
        assert_eq!(manifest.runtime.kind, kind);
        assert_eq!(manifest.runtime.entry.as_deref(), Some(entry));
    }

    let package = root.path().join("plugin");
    fs::create_dir(&package).unwrap();
    let source = package.join("plugin.toml");
    let trial = call(TRY_PLUGIN_TOOL, json!({ "path": "plugin" }));
    fs::write(&source, r#"id = "hello-world""#).unwrap();
    let error = state.execute_tool(&trial);
    assert!(error.is_error);
    let detail = error.content[0]["text"].as_str().unwrap();
    assert!(detail.contains(
        &Path::new("plugin")
            .join("plugin.toml")
            .display()
            .to_string()
    ));
    assert!(detail.contains("unknown field `id`"));
    assert!(detail.contains(INSPECT_TOOL));
    assert!(!detail.contains(".creator-trials"));

    fs::write(&source, external).unwrap();
    let error = state.execute_tool(&trial);
    assert!(error.is_error);
    let detail = error.content[0]["text"].as_str().unwrap();
    assert!(detail.contains("缺少构建产物") && detail.contains(BUILD_PLUGIN_TOOL));
}

// CPP-CREATOR-016
#[test]
fn creator_inspect_exact_contracts_match_discovery_and_runtime() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path(), ApprovalBroker::noninteractive());
    let discovery = state.execute_tool(&call(INSPECT_TOOL, json!({})));
    let queries = discovery.value["queries"].as_object().unwrap();
    for (section, names) in queries {
        for name in names.as_array().unwrap() {
            let result = state.execute_tool(&call(
                INSPECT_TOOL,
                json!({"section": section, "name": name}),
            ));
            assert!(!result.is_error, "{section} {name}: {:?}", result.content);
            assert_eq!(result.value["section"], section.as_str());
            assert_eq!(&result.value["name"], name);
            assert!(result.value["contract"].is_object());
            assert!(result.value.get("workspace_panel").is_none());
        }
    }
    assert_eq!(
        queries["slot"].as_array().unwrap().len(),
        dshr_ui_contributions::SlotCatalog::builtin().len()
    );
    let mut lookup = |section: &str, name: &str| {
        let result = state.execute_tool(&call(
            INSPECT_TOOL,
            json!({"section": section, "name": name}),
        ));
        assert!(!result.is_error, "{:?}", result.content);
        result.value["contract"].clone()
    };
    let file = lookup("slot", "viewer.tab");
    assert_eq!(
        file["registration"]["required"],
        json!(["slot_id", "plugin_id", "key", "view"])
    );
    assert_eq!(file["callback"]["context"], "FileViewContext");
    assert_eq!(
        file["callback"]["context_contract"],
        lookup("sdk", "FileViewContext")
    );
    assert_eq!(
        file["registration"]["fields"]["key"],
        file["callback"]["context_contract"]["schema"]["properties"]["file_type"]
    );
    assert_eq!(file["actions"], json!(["copy", "openExternal"]));
    assert_eq!(file["validation"], "registration");
    let menu = lookup("slot", "menu.item");
    assert_eq!(
        menu["registration"]["fields"]["key"]["enum"],
        json!(dshr_plugin_host::DESKTOP_MENU_KEYS)
    );
    assert_eq!(menu["callback"]["action"], "command");
    assert_eq!(
        lookup("slot", "workspace.panel")["registration"]["sdk"],
        "PanelDecl"
    );
    let slider = lookup("element", "Slider");
    assert_eq!(slider["schema"]["properties"]["type"]["const"], "Slider");
    assert_eq!(
        slider["references"]["#/$defs/props/Slider"]["required"],
        json!(["value"])
    );
    assert_eq!(slider["event"]["call_args"]["value"]["type"], "number");
    assert!(!slider["references"]["#/$defs/action"].is_null());
    assert!(lookup("element", "Switch")["event"]["call_args"].is_string());
    assert_eq!(lookup("element", "List")["event"]["dispatch"], false);
    let stack = lookup("element", "Stack");
    assert_eq!(
        stack["references"]["#/$defs/children"]["items"]["$ref"],
        "#/$defs/node"
    );
    assert!(stack["references"].get("#/$defs/node").is_none());

    for input in [
        json!({"section":"sdk"}),
        json!({"name":"ToolDecl"}),
        json!({"section":"slot","name":""}),
        json!({"section":"slot","name":"   "}),
        json!({"section":"unknown","name":"x"}),
        json!({"section":"sdk","name":"ToolDecl","extra":true}),
    ] {
        assert!(state.execute_tool(&call(INSPECT_TOOL, input)).is_error);
    }
    let unknown = state.execute_tool(&call(
        INSPECT_TOOL,
        json!({"section":"element","name":"slider"}),
    ));
    assert!(unknown.is_error && unknown.content.to_string().contains("queries.element"));
}

// CPP-CREATOR-017
#[test]
#[ignore = "requires native Cargo with the offline SDK dependencies"]
fn creator_inspect_drives_external_slot_callback() {
    let root = tempfile::tempdir().unwrap();
    let approvals = ApprovalBroker::new();
    let mut state = state(root.path(), approvals.clone());
    let discovery = state.execute_tool(&call(INSPECT_TOOL, json!({})));
    assert_eq!(
        discovery.value["build"]["cargo_found"], true,
        "{:?}",
        discovery.content
    );
    let template = &discovery.value["workspace_panel"];
    let mut lookup = |section, name| {
        let result = state.execute_tool(&call(
            INSPECT_TOOL,
            json!({"section": section, "name": name}),
        ));
        assert!(!result.is_error, "{:?}", result.content);
        result.value["contract"].clone()
    };
    let slot = lookup("slot", "workspace.sidebar.tab");
    let slider = lookup("element", "Slider");
    let action = lookup("sdk", "ActionHandler");
    let registration = &slot["registration"]["example"];
    let event = slider["event"]["prop"].as_str().unwrap();
    assert_eq!(slider["event"]["call_args"]["value"]["type"], "number");
    assert_eq!(
        slider["references"]["#/$defs/props/Slider"]["properties"]["value"]["type"],
        "number"
    );
    let method = action["examples"]["call"]["payload"]["method"]
        .as_str()
        .unwrap();
    let tree = json!({"type": slider["schema"]["properties"]["type"]["const"], "props": {
        "a11yLabel": "Volume", "value": 2.0, "min": 0.0, "max": 10.0, "step": 0.5,
        (event): {"call": method, "args": {}}
    }});
    let source = r#"
use dshr_plugin_sdk::{ActionHandler, CancelFlag, ChildSession, ReadyDecls, SessionError, ToolCall, ToolError, ToolResult, ViewHandler};
use std::sync::{Arc, Mutex};

struct Slider(Arc<Mutex<f64>>);

impl ViewHandler for Slider {
    __RENDER__ {
        assert_eq!(view, __VIEW__);
        let _ = state_seq;
        let mut tree: serde_json::Value = serde_json::from_str(__TREE__).unwrap();
        tree["props"]["value"] = serde_json::json!(*self.0.lock().unwrap());
        Ok(tree)
    }
}

impl ActionHandler for Slider {
    __ACTION__ {
        assert_eq!(view, __VIEW__);
        assert_eq!(action, "call");
        assert_eq!(payload["method"], __METHOD__);
        *self.0.lock().unwrap() = payload["args"]["value"].as_f64()
            .ok_or_else(|| ToolError::invalid_arguments("value must be a number"))?;
        Ok(())
    }
}

fn main() -> Result<(), SessionError> {
    let state = Arc::new(Mutex::new(2.0));
    let registration = serde_json::from_str(__REGISTRATION__).unwrap();
    let decls = ReadyDecls::new(vec![], vec![]).with_slots(vec![registration]);
    ChildSession::serve_with_views_and_actions(
        decls, Slider(state.clone()), Slider(state),
        |_: &ToolCall, _: &CancelFlag| -> Result<ToolResult, ToolError> {
            Err(ToolError::invalid_arguments("no tools"))
        },
    )
}
"#
    .replace("__RENDER__", slot["callback"]["signature"].as_str().unwrap())
    .replace("__ACTION__", action["signatures"][0].as_str().unwrap())
    .replace("__VIEW__", &format!("{:?}", registration["view"].as_str().unwrap()))
    .replace("__METHOD__", &format!("{method:?}"))
    .replace("__TREE__", &format!("{:?}", tree.to_string()))
    .replace("__REGISTRATION__", &format!("{:?}", registration.to_string()));
    let package = root.path().join("slider-plugin");
    fs::create_dir_all(package.join("src")).unwrap();
    for file in ["plugin.toml", "Cargo.toml"] {
        fs::write(package.join(file), template[file].as_str().unwrap()).unwrap();
    }
    fs::write(package.join("src/main.rs"), source).unwrap();

    let mut execute_approved = |tool: &str| {
        let approvals = approvals.clone();
        let expected = tool.to_owned();
        let answerer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(pending) = approvals.pending().into_iter().next() {
                    let allow =
                        pending.tool == expected && pending.arguments["path"] == "slider-plugin";
                    approvals.resolve(
                        &pending.call_id,
                        if allow {
                            ApprovalOutcome::AllowOnce
                        } else {
                            ApprovalOutcome::Deny
                        },
                    );
                    return;
                }
                assert!(Instant::now() < deadline, "missing approval for {expected}");
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let result = state.execute_tool(&call(tool, json!({"path": "slider-plugin"})));
        answerer.join().unwrap();
        assert!(!result.is_error, "{tool}: {:?}", result.content);
        result.value
    };
    execute_approved(BUILD_PLUGIN_TOOL);
    let trial = execute_approved(TRY_PLUGIN_TOOL);
    assert_eq!(trial["slots"][0]["slot_id"], "workspace.sidebar.tab");
    assert_eq!(trial["slots"][0]["validation"], slot["validation"]);
    state.refresh_adc_panels();
    let find = |state: &ChatState| {
        let panel = state
            .adc_panels
            .iter()
            .find(|panel| {
                panel.trial
                    && panel
                        .slot
                        .as_ref()
                        .is_some_and(|slot| slot.slot_id == "workspace.sidebar.tab")
            })
            .unwrap();
        let tree = panel.tree.as_ref().unwrap();
        assert!(parse_tree(tree).diagnostics.is_empty());
        (
            dshr_plugin_host::SlotTarget::from(panel.slot.as_ref().unwrap()),
            tree["props"]["value"].clone(),
        )
    };
    let (target, initial) = find(&state);
    assert_eq!(initial, 2.0);
    let mut payload = action["examples"]["call"]["payload"].clone();
    payload["args"]["value"] = json!(7.5);
    state
        .apply_adc_slot_action(
            target,
            action["examples"]["call"]["action"].as_str().unwrap(),
            &payload,
        )
        .unwrap();
    assert_eq!(find(&state).1, 7.5);
}

// CPP-CREATOR-002
#[test]
fn creator_preview_checks_the_adc_tree_before_returning_it() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path(), ApprovalBroker::noninteractive());
    let element = json!({ "type": "Text", "props": { "value": "预览" } });
    let result = state.execute_tool(&call(PREVIEW_TOOL, json!({ "element": &element })));
    assert!(!result.is_error, "{:?}", result.content);
    assert_eq!(result.meta.unwrap()["ui"], element);

    let controls = json!({ "type": "Stack", "children": [
        { "type": "Input", "props": {"a11yLabel": "Name", "value": "", "onChange": {"call":"set_name"}} },
        { "type": "Button", "props": {"a11yLabel": "Greet", "label": "Greet", "onPress": {"call":"greet"}} }
    ] });
    let result = state.execute_tool(&call(PREVIEW_TOOL, json!({ "element": &controls })));
    assert!(!result.is_error, "{:?}", result.content);
    assert_eq!(result.value["interactive"], false);
    assert_eq!(result.meta.unwrap()["ui"], controls);

    for element in [
        json!({ "type": "Unknown" }),
        json!({ "type": "Button", "props": { "label": "Test" } }),
    ] {
        let result = state.execute_tool(&call(PREVIEW_TOOL, json!({ "element": element })));
        assert!(result.is_error);
        assert!(result.meta.is_none());
        let diagnostic = result.content[0]["text"].as_str().unwrap();
        assert!(diagnostic.contains(INSPECT_TOOL) && diagnostic.contains("element"));
    }
}

// CPP-CREATOR-003
#[test]
fn creator_preview_is_exposed_to_the_model_and_replayed_as_an_adc_card() {
    struct PreviewModel(Value);

    impl ModelProvider for PreviewModel {
        fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
            assert!(request
                .tool_specs
                .iter()
                .any(|spec| spec.name == PREVIEW_TOOL));
            if request.last_tool_content().is_some() {
                return Ok(ModelReply::text_only("已生成预览"));
            }
            Ok(ModelReply {
                reasoning: String::new(),
                text: String::new(),
                tool_calls: vec![ModelToolCall {
                    id: "preview".to_owned(),
                    name: PREVIEW_TOOL.to_owned(),
                    arguments: json!({ "element": &self.0 }),
                }],
            })
        }
    }

    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path(), ApprovalBroker::noninteractive());
    let tree = json!({ "type": "Text", "props": { "value": "示例" } });
    *state.agent.provider_mut() = Box::new(PreviewModel(tree.clone()));
    assert_eq!(
        state.stream_turn("预览面板", &mut |_| {}).as_deref(),
        Some("已生成预览")
    );
    let log = SessionLog::from_jsonl("creator", &fs::read_to_string(&state.path).unwrap()).unwrap();
    let items = dshr_conversation_model::build_from_log(&log);
    assert!(items.into_iter().any(|item| match item {
        dshr_conversation_model::Item::Tool(tool) => {
            tool.tool == PREVIEW_TOOL && tool.card.and_then(|card| card.extra) == Some(tree.clone())
        }
        _ => false,
    }));
}

fn create_echo_package(root: &Path) -> std::path::PathBuf {
    const GUEST: &[u8] = include_bytes!(
        "../../../../../../crates/plugin-wasm/tests/fixtures/cpp-component-guest.wasm"
    );
    let package = root.join("plugin");
    fs::create_dir(&package).unwrap();
    let guest = package.join("plugin.wasm");
    fs::write(&guest, GUEST).unwrap();
    fs::write(
        package.join("plugin.toml"),
        r#"
[plugin]
id = "creator.test"
version = "0.1.0"
api = "cordis-portable/v1"

[runtime]
kind = "wasm-component"
entry = "plugin.wasm"
"#,
    )
    .unwrap();

    guest
}

// CPP-CREATOR-004
#[test]
fn creator_trial_requires_approval_and_loads_the_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let guest = create_echo_package(root.path());
    let broker = ApprovalBroker::new();
    let mut state = state(root.path(), broker.clone());
    let try_plugin = call(TRY_PLUGIN_TOOL, json!({ "path": "plugin" }));
    state.permission.set(PermissionMode::ReadOnly);
    assert!(state.execute_tool(&try_plugin).is_error);
    assert!(broker.take_records().is_empty());
    state.permission.set(PermissionMode::DangerFullAccess);
    assert!(
        state
            .execute_tool(&call(TRY_PLUGIN_TOOL, json!({ "path": "../plugin" })))
            .is_error
    );
    assert!(broker.take_records().is_empty());

    for decision in [ApprovalOutcome::Deny, ApprovalOutcome::AllowOnce] {
        let answerer = broker.clone();
        let source = guest.clone();
        let answer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(pending) = answerer.pending().into_iter().next() {
                    let mutation = if decision == ApprovalOutcome::AllowOnce {
                        fs::write(&source, b"changed after snapshot")
                    } else {
                        Ok(())
                    };
                    answerer.resolve(&pending.call_id, decision);
                    mutation.unwrap();
                    assert_eq!(pending.arguments["plugin"], "creator.test");
                    return;
                }
                assert!(Instant::now() < deadline, "missing explicit trial approval");
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let outcome = state.execute_tool(&try_plugin);
        answer.join().unwrap();
        assert_eq!(
            outcome.is_error,
            decision == ApprovalOutcome::Deny,
            "{:?}",
            outcome.content
        );
        assert_eq!(
            state.creator_trials["creator"]
                .plugins
                .registry()
                .get("echo")
                .is_some(),
            decision == ApprovalOutcome::AllowOnce
        );
    }

    let echo = state.execute_tool(&call("echo", json!({ "text": "trial" })));
    assert_eq!(echo.value["content"][0]["text"], "{\"text\":\"trial\"}");
    assert_eq!(broker.take_records().len(), 2);
    let store = SessionIndex::new(root.path().join("sessions"));
    store
        .create_root("standard", &root.path().to_string_lossy(), "1")
        .unwrap();
    state.switch_session(store.path_for("standard"), "standard");
    assert!(
        state
            .execute_tool(&call("echo", json!({ "text": "gone" })))
            .is_error
    );
    assert_eq!(state.creator_trials["creator"].len(), 1);
    state.switch_session(store.path_for("creator"), "creator");
    assert!(
        !state
            .execute_tool(&call("echo", json!({ "text": "back" })))
            .is_error
    );
    state.drop_session_scope("creator");
    assert_eq!(
        fs::read_dir(store.dir().join(".creator-trials"))
            .unwrap()
            .count(),
        0
    );
}

// CPP-CREATOR-013
#[test]
fn creator_retrial_keeps_old_instance_until_the_candidate_passes() {
    let root = tempfile::tempdir().unwrap();
    create_echo_package(root.path());
    let broker = ApprovalBroker::new();
    let mut state = state(root.path(), broker.clone());
    let mut run_trial = |expect_panel, decision| {
        let answerer = broker.clone();
        let answer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(pending) = answerer.pending().into_iter().next() {
                    answerer.resolve(&pending.call_id, decision);
                    return;
                }
                assert!(Instant::now() < deadline, "missing trial approval");
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let result = state.execute_tool(&call(
            TRY_PLUGIN_TOOL,
            json!({"path":"plugin", "expect_panel":expect_panel}),
        ));
        answer.join().unwrap();
        let scope = &state.creator_trials["creator"];
        assert_eq!(scope.len(), 1);
        (
            result,
            scope.trials[0].directory.path().to_owned(),
            scope.revision,
        )
    };
    let (first, original, revision) = run_trial(false, ApprovalOutcome::AllowOnce);
    assert_eq!(first.value["replaced"], false);
    let (missing, unchanged, after_missing) = run_trial(true, ApprovalOutcome::AllowOnce);
    assert!(missing.is_error && missing.content.to_string().contains("ReadyDecls"));
    assert_eq!(unchanged, original);
    assert_eq!(after_missing, revision);
    let (denied, unchanged, _) = run_trial(false, ApprovalOutcome::Deny);
    assert!(denied.is_error);
    assert_eq!(unchanged, original);
    let (replaced, replacement, after_replacement) = run_trial(false, ApprovalOutcome::AllowOnce);
    assert_eq!(replaced.value["replaced"], true);
    assert!(replacement.is_dir() && !original.exists());
    assert_eq!(after_replacement, revision + 1);
    assert!(
        !state
            .execute_tool(&call("echo", json!({"text":"replacement"})))
            .is_error
    );
}

// CPP-CREATOR-005
#[test]
fn sessions_restore_their_own_tool_instances_and_panels() {
    use dshr_plugin_api::{PanelContribution, Plugin, PluginContext, PluginError};
    use dshr_tool_runtime::ToolDefinition;

    struct Counter(u64);
    impl Plugin for Counter {
        fn id(&self) -> &'static str {
            "creator.counter"
        }

        fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
            ctx.register_tool(ToolDefinition::new(
                "fixture_counter",
                "Session counter",
                ToolOrigin::Native {
                    plugin_id: self.id().to_owned(),
                },
            ))?;
            ctx.register_panel(PanelContribution::new(
                "counter.panel",
                "Counter",
                "workspace.panel",
            ));
            ctx.register_slots(
                [
                    "conversation.input.right",
                    "settings.section",
                    "workspace.sidebar.tab",
                    "workspace.page",
                    "status.bar.item",
                ]
                .map(|slot| {
                    json!({
                        "slot_id": slot, "plugin_id": self.id(),
                        "id": "counter.quick", "label": "Quick Counter", "view": "counter.quick"
                    })
                }),
            );
            ctx.register_slots([json!({
                "slot_id": dshr_plugin_host::TOOL_CALL_VIEW, "plugin_id": self.id(),
                "key": "fixture_counter", "view": "counter.result"
            })]);
            Ok(())
        }

        fn call(&mut self, _: &str, _: &Value) -> Result<Value, PluginError> {
            self.0 += 1;
            Ok(json!(self.0))
        }

        fn render_view(&mut self, _: &str) -> Result<Value, PluginError> {
            Ok(json!({ "type": "Text", "props": { "value": self.0.to_string() } }))
        }

        fn handle_action(
            &mut self,
            view: &str,
            action: &str,
            _: &Value,
        ) -> Result<(), PluginError> {
            assert_eq!(view, "counter.quick");
            assert_eq!(action, "call");
            self.0 += 1;
            Ok(())
        }
    }

    let root = tempfile::tempdir().unwrap();
    let broker = ApprovalBroker::noninteractive();
    let mut state = state(root.path(), broker.clone());
    let store = SessionIndex::new(root.path().join("sessions"));
    let second_workspace = root.path().join("second");
    fs::create_dir(&second_workspace).unwrap();
    let created = SessionCreated::root(second_workspace.to_string_lossy()).with_preset(PRESET);
    store.create_with_created("second", &created, "1").unwrap();
    for (id, count) in [("creator", 0), ("second", 100)] {
        let mut scope = TrialScope::new(
            state.permission.clone(),
            if id == "creator" {
                root.path()
            } else {
                &second_workspace
            },
        );
        scope.plugins.load(Box::new(Counter(count))).unwrap();
        let reported = validate_trial_slots(&mut scope.plugins, "creator.counter").unwrap();
        assert!(reported.iter().any(|slot| {
            slot["slot_id"] == dshr_plugin_host::TOOL_CALL_VIEW
                && slot["key"] == "fixture_counter"
                && slot["validation"] == "registration"
        }));
        for slot_id in [
            "settings.section",
            "workspace.sidebar.tab",
            "workspace.page",
            "status.bar.item",
        ] {
            let expected = dshr_plugin_host::DESKTOP_VIEW_SLOTS
                .iter()
                .find_map(|(slot, location)| (*slot == slot_id).then_some(*location))
                .unwrap();
            assert!(reported
                .iter()
                .any(|slot| slot["slot_id"] == slot_id && slot["location"] == expected));
        }
        state.creator_trials.insert(id.to_owned(), scope);
    }
    let request = call("fixture_counter", json!({}));
    assert_eq!(state.execute_tool(&request).value, 1);
    state.switch_session(store.path_for("second"), "second");
    assert!(state.adc_panels.iter().any(|panel| panel
        .tree
        .as_ref()
        .is_some_and(|tree| tree["props"]["value"] == "100")));
    let slot_target = |state: &ChatState| {
        state
            .adc_panels
            .iter()
            .filter_map(|panel| panel.slot.as_ref())
            .find(|slot| slot.slot_id == "conversation.input.right")
            .map(dshr_plugin_host::SlotTarget::from)
            .unwrap()
    };
    let second_slot = slot_target(&state);
    assert_eq!(state.execute_tool(&request).value, 101);
    state.switch_session(store.path_for("creator"), "creator");
    assert!(state
        .apply_adc_slot_action(second_slot, "call", &json!({}))
        .is_err());
    assert!(state.adc_panels.iter().any(|panel| panel
        .tree
        .as_ref()
        .is_some_and(|tree| tree["props"]["value"] == "1")));
    assert_eq!(state.execute_tool(&request).value, 2);
    state
        .apply_adc_slot_action(slot_target(&state), "call", &json!({}))
        .unwrap();
    assert!(state.adc_panels.iter().any(|panel| panel
        .slot
        .as_ref()
        .is_some_and(|slot| slot.slot_id == "conversation.input.right")
        && panel
            .tree
            .as_ref()
            .is_some_and(|tree| tree["props"]["value"] == "3")));
    assert!(broker.take_records().is_empty());

    store.delete("second").unwrap();
    state.drop_session_scope("second");
    assert!(!state.creator_trials.contains_key("second"));
    assert_eq!(state.execute_tool(&request).value, 4);
    store.delete("creator").unwrap();
    state.drop_session_scope("creator");
    assert!(state.execute_tool(&request).is_error);
    assert!(!state
        .adc_panels
        .iter()
        .any(|panel| panel.title == "Counter"));
}

// CPP-CREATOR-008
#[test]
fn creator_writes_into_a_new_package_directory() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path(), ApprovalBroker::noninteractive());
    let target = "hello-world/src/main.rs";
    assert!(!root.path().join("hello-world").exists());
    state.permission.set(PermissionMode::ReadOnly);
    let request = call(
        WRITE_FILE_TOOL,
        json!({ "path": target, "content": "fn main() {}" }),
    );
    assert!(state.execute_tool(&request).is_error);
    assert!(!root.path().join("hello-world").exists());
    state.permission.set(PermissionMode::DangerFullAccess);
    let result = state.execute_tool(&request);
    assert!(!result.is_error, "{:?}", result.content);
    assert_eq!(
        fs::read_to_string(root.path().join(target)).unwrap(),
        "fn main() {}"
    );
}

// CPP-CREATOR-009
#[test]
fn desktop_and_creator_exclude_diagnostic_tools() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path(), ApprovalBroker::noninteractive());
    assert!(state
        .plugins
        .registry()
        .list()
        .iter()
        .all(|tool| !tool.name.starts_with("probe_")));

    // Simulate a plugin trying to publish a name reserved for diagnostics.
    state
        .plugins
        .registry_mut()
        .register(crate::ToolDefinition::new(
            "probe_injected",
            "test fixture",
            crate::ToolOrigin::Native {
                plugin_id: "fixture".to_owned(),
            },
        ))
        .unwrap();
    let inspect = state.execute_tool(&call(INSPECT_TOOL, json!({})));
    let offered = inspect.value["tools"].as_array().unwrap();
    assert!(offered.iter().any(|tool| tool == WRITE_FILE_TOOL));
    assert!(offered
        .iter()
        .all(|tool| !tool.as_str().unwrap().starts_with("probe_")));

    let store = SessionIndex::new(root.path().join("sessions"));
    store
        .create_root("standard", &root.path().to_string_lossy(), "1")
        .unwrap();
    for session in ["creator", "standard"] {
        state.switch_session(store.path_for(session), session);
        for tool in ["probe_native", "probe_write", "probe_injected"] {
            let result = state.execute_tool(&call(tool, json!({})));
            assert!(result.is_error);
            assert!(format!("{:?}", result.content).contains("was not offered"));
        }
    }
}

// CPP-CREATOR-006
#[test]
fn model_step_uses_its_offered_tools_for_execution_and_audit() {
    struct TrialModel(u8);
    impl ModelProvider for TrialModel {
        fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
            let echo = ModelToolCall {
                id: format!("echo-{}", self.0),
                name: "echo".to_owned(),
                arguments: json!({ "text": "trial" }),
            };
            let has_echo = request.tool_specs.iter().any(|tool| tool.name == "echo");
            self.0 += 1;
            let tool_calls = match self.0 {
                1 => {
                    assert!(!has_echo);
                    vec![
                        ModelToolCall {
                            id: "try".to_owned(),
                            name: TRY_PLUGIN_TOOL.to_owned(),
                            arguments: json!({ "path": "plugin" }),
                        },
                        echo,
                    ]
                }
                2 => {
                    assert!(has_echo);
                    assert!(request
                        .last_tool_content()
                        .unwrap()
                        .contains("was not offered"));
                    vec![echo]
                }
                _ => {
                    assert!(request.last_tool_content().unwrap().contains("trial"));
                    return Ok(ModelReply::text_only("done"));
                }
            };
            Ok(ModelReply {
                reasoning: String::new(),
                text: String::new(),
                tool_calls,
            })
        }
    }

    let root = tempfile::tempdir().unwrap();
    create_echo_package(root.path());
    let broker = ApprovalBroker::new();
    let mut state = state(root.path(), broker.clone());
    *state.agent.provider_mut() = Box::new(TrialModel(0));
    let answer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(pending) = broker.pending().into_iter().next() {
                broker.resolve(&pending.call_id, ApprovalOutcome::AllowOnce);
                return;
            }
            assert!(Instant::now() < deadline, "missing explicit trial approval");
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    assert_eq!(
        state.stream_turn("trial", &mut |_| {}).as_deref(),
        Some("done")
    );
    answer.join().unwrap();
    let origins: Vec<_> = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "tool.call" && event.payload["tool"] == "echo")
        .map(|event| event.payload["origin"].as_str().unwrap())
        .collect();
    assert_eq!(origins, ["unknown", "native"]);
}

// M3-PRESET-004
#[test]
#[ignore = "requires the official sidecar's vendored Node dependencies"]
fn desktop_reloads_preset_files_and_spawns_children_on_the_parent_revision() {
    use crate::{app_root, ChatSessionAccess};
    use dshr_compat_dsh_host::{HostOptions, SidecarHost};

    let root = tempfile::tempdir().unwrap();
    let bundle = root.path().join("bundle.yml");
    let store = SessionIndex::new(root.path().join("sessions"));
    let created = SessionCreated::root(root.path().to_string_lossy()).with_preset(CORDIS_PRESET);
    for id in ["s-first", "s-second"] {
        store.create_with_created(id, &created, "1").unwrap();
    }
    let repo = app_root();
    let script = repo.join("sidecar/dsh-official/sidecar-dsh.mjs");
    let plugin = repo.join("crates/compat-dsh-host/tests/fixtures/preset-counter.mjs");
    let plugin_path = plugin.to_string_lossy().replace('\\', "/");
    let plugin_url = if plugin_path.starts_with('/') {
        format!("file://{plugin_path}")
    } else {
        format!("file:///{plugin_path}")
    };
    let write_bundle = |label: &str| {
        fs::write(&bundle, format!(
        "- name: '@deepseek-ai/dsh-agent-preset'\n  config:\n    id: cordis\n    plugins:\n      - name: '{plugin_url}'\n        config:\n          label: '{label}'\n"
    )).unwrap()
    };
    write_bundle("before");
    let approvals = ApprovalBroker::noninteractive();
    let mut state = ChatState::open_with_settings(
        store.path_for("s-first"),
        "s-first",
        &Arc::new(AtomicBool::new(false)),
        approvals,
        true,
        false,
        DesktopSettings {
            model: Arc::new(DesktopModel::Mock),
            permission: Some(PermissionMode::DangerFullAccess),
            reasoning: None,
        },
    );
    state.preset_inputs = Some((bundle.clone(), vec![]));
    state.l2_access = Some(ChatSessionAccess::new(store.dir().to_owned()));
    let spawn = || {
        SidecarHost::spawn_with(
            &script,
            &script,
            HostOptions::default()
                .with_env("DSHR_DSH_PLUGINS", "[]")
                .with_registration_window(Duration::from_millis(250)),
        )
        .unwrap()
    };
    state.attach_sidecar(spawn());
    let request = call("preset_counter", json!({}));
    assert_eq!(state.execute_tool(&request).value["label"], "before");
    assert_eq!(
        state.execute_tool(&call(INSPECT_TOOL, json!({}))).value["preset"],
        "cordis"
    );
    let original_id = state.session_id.clone();
    let original_log = fs::read_to_string(&state.path).unwrap();
    assert!(state
        .select_blank_preset(Some("unavailable".to_owned()))
        .is_err());
    assert_eq!(fs::read_to_string(&state.path).unwrap(), original_log);
    state.select_blank_preset(None).unwrap();
    assert!(state.execute_tool(&request).is_error);
    state
        .select_blank_preset(Some("cordis".to_owned()))
        .unwrap();
    assert_eq!(state.execute_tool(&request).value["label"], "before");
    assert_eq!(state.session_id, original_id);
    write_bundle("after");
    state.switch_session(store.path_for("s-second"), "s-second");
    assert_eq!(state.execute_tool(&request).value["label"], "after");
    state.switch_session(store.path_for("s-first"), "s-first");

    let child_result = state.execute_tool(&call("spawn_child", json!({"prompt": "你好"})));
    assert!(
        !child_result.is_error,
        "child failed: {:?}",
        child_result.content
    );
    let child = store
        .scan()
        .unwrap()
        .into_iter()
        .find(|session| session.id != "s-first" && session.id != "s-second")
        .unwrap();
    state.switch_session(child.path, &child.id);
    assert_eq!(state.execute_tool(&request).value["label"], "before");
    assert!(state.execute_tool(&call(INSPECT_TOOL, json!({}))).is_error);
    fs::write(&bundle, "invalid: [").unwrap();
    assert_eq!(state.execute_tool(&request).value["label"], "before");
    assert!(state
        .load_errors()
        .iter()
        .any(|error| error.contains("预设配置读取失败")));

    write_bundle("after");
    state.kill_sidecar().unwrap();
    state.attach_sidecar(spawn());
    state.switch_session(store.path_for("s-first"), "s-first");
    assert_eq!(state.execute_tool(&request).value["label"], "after");
}

// M3-PRESET-005
#[test]
#[ignore = "requires the official sidecar's vendored Node dependencies"]
fn m3_preset_005_background_reload_publishes_while_idle() {
    use crate::{app_root, preset_reload};
    use dshr_compat_dsh_host::{HostOptions, SidecarHost};
    use std::sync::Mutex;

    let root = tempfile::tempdir().unwrap();
    let bundle = root.path().join("bundle.yml");
    let included = root.path().join("preset.yml");
    let patch = root.path().join("patch.yml");
    let app = app_root();
    let script = app.join("sidecar/dsh-official/sidecar-dsh.mjs");
    let plugin = app.join("crates/compat-dsh-host/tests/fixtures/preset-counter.mjs");
    let plugin_path = plugin.to_string_lossy().replace('\\', "/");
    let plugin_url = if plugin_path.starts_with('/') {
        format!("file://{plugin_path}")
    } else {
        format!("file:///{plugin_path}")
    };
    let config = |label: &str| {
        format!(
        "  config:\n    id: cordis\n    plugins:\n      - name: '{plugin_url}'\n        config:\n          label: '{label}'\n"
    )
    };
    let declaration = |label: &str| {
        format!(
            "- id: preset-cordis\n  name: '@deepseek-ai/dsh-agent-preset'\n{}",
            config(label),
        )
    };
    fs::write(&bundle, "- include: preset.yml\n").unwrap();
    fs::write(&included, declaration("initial")).unwrap();
    fs::write(&patch, "[]").unwrap();

    let mut chat = state(root.path(), ApprovalBroker::noninteractive());
    chat.preset_inputs = Some((bundle.clone(), vec![patch.clone()]));
    let spawn = || {
        SidecarHost::spawn_with(
            &script,
            &script,
            HostOptions::default()
                .with_env("DSHR_DSH_PLUGINS", "[]")
                .with_registration_window(Duration::from_millis(250)),
        )
        .unwrap()
    };
    chat.attach_sidecar(spawn());
    chat.workspace_sidecars
        .insert(root.path().join("parked"), spawn());
    chat.refresh_agent_presets();
    chat.host
        .as_mut()
        .unwrap()
        .bind_preset("s-old", "cordis", None)
        .unwrap();
    let chat = Arc::new(Mutex::new(chat));
    let updates = preset_reload::start(&chat).unwrap();
    let wait_for_background = || {
        updates
            .recv_timeout(Duration::from_secs(6))
            .expect("idle reload notification before session or model activity")
    };
    let registered = |chat: &ChatState, label: &str| {
        [
            chat.host.as_ref().unwrap(),
            &chat.workspace_sidecars[&root.path().join("parked")],
        ]
        .into_iter()
        .all(|host| {
            host.registrations().iter().any(|registration| {
                registration.preset_revision.is_some() && registration.description == label
            })
        })
    };

    assert!(registered(&chat.lock().unwrap(), "initial"));
    fs::write(&included, declaration("include-edit")).unwrap();
    wait_for_background();
    assert!(registered(&chat.lock().unwrap(), "include-edit"));
    fs::write(&bundle, declaration("bundle-edit")).unwrap();
    wait_for_background();
    assert!(registered(&chat.lock().unwrap(), "bundle-edit"));
    fs::write(
        &patch,
        format!("- id: preset-cordis\n{}", config("patch-edit")),
    )
    .unwrap();
    wait_for_background();
    assert!(registered(&chat.lock().unwrap(), "patch-edit"));

    fs::write(&patch, "invalid: [").unwrap();
    wait_for_background();
    {
        let mut chat = chat.lock().unwrap();
        assert!(registered(&chat, "patch-edit"));
        assert!(chat
            .load_errors()
            .iter()
            .any(|error| error.contains("预设配置读取失败")));
        let host = chat.host.as_mut().unwrap();
        host.bind_preset("s-child", "cordis", Some("s-old"))
            .unwrap();
        host.bind_preset("s-new", "cordis", None).unwrap();
        for (session, label) in [
            ("s-old", "initial"),
            ("s-child", "initial"),
            ("s-new", "patch-edit"),
        ] {
            let outcome = host
                .invoke_in_session(
                    "counter",
                    Some(session),
                    "preset_counter",
                    json!({}),
                    Duration::from_secs(3),
                )
                .unwrap();
            assert_eq!(outcome.terminal.payload["value"]["label"], label);
        }
    }

    fs::write(&patch, "[]").unwrap();
    wait_for_background();
    let chat = chat.lock().unwrap();
    assert!(registered(&chat, "bundle-edit"));
    assert!(chat
        .load_errors()
        .iter()
        .all(|error| !error.contains("预设配置读取失败")));
}
