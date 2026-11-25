use super::{BUILD_PLUGIN_TOOL, CORDIS_PRESET, INSPECT_TOOL, TRY_PLUGIN_TOOL, WRITE_FILE_TOOL};
use crate::{
    runtime_components, AdcAction, ChatState, DesktopModel, DesktopSettings, PermissionMode,
    Profile,
};
use dshr_agent_core::ToolCallRequest;
use dshr_llm_deepseek::DeepSeekProvider;
use dshr_session_store::{SessionCreated, SessionIndex};
use dshr_tool_runtime::{ApprovalBroker, ApprovalOutcome};
use dshr_ui_contributions::{parse_tree, ElementKind};
use serde_json::json;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn relative_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

static PANEL_WINDOW_TEST: AtomicBool = AtomicBool::new(false);

// CPP-CREATOR-015. Open the normal desktop against the prepared notes fixture.
#[test]
#[ignore = "interactive Windows desktop check with a prepared workspace and DeepSeek"]
fn live_creator_desktop_displays_trial_panel() {
    let workspace = std::env::var_os("DSHR_WORKSPACE").expect("isolated workspace");
    let workspace = Path::new(&workspace);
    let home = std::env::var_os("DSHR_HOME").expect("isolated app data directory");
    assert!(workspace.starts_with(Path::new(&home).parent().unwrap()));
    assert!(workspace.join("notes-panel/bin/notes-panel.exe").is_file());
    PANEL_WINDOW_TEST.store(true, Ordering::Release);
    crate::main();
}

pub(crate) fn drive_notes_panel_approval(approvals: &Arc<ApprovalBroker>) {
    if !PANEL_WINDOW_TEST.load(Ordering::Acquire) {
        return;
    }
    let approvals = Arc::clone(approvals);
    std::thread::spawn(move || loop {
        for pending in approvals.pending() {
            let fixture = pending.tool == TRY_PLUGIN_TOOL
                && pending.arguments["plugin"] == "demo.notes-panel"
                && pending.arguments["path"] == "notes-panel"
                && pending.arguments["permissions"]
                    .as_array()
                    .is_some_and(Vec::is_empty);
            approvals.resolve(
                &pending.call_id,
                if fixture {
                    ApprovalOutcome::AllowOnce
                } else {
                    ApprovalOutcome::Deny
                },
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    });
}

// CPP-CREATOR-007
#[test]
#[ignore = "requires a real DeepSeek connection and native Cargo with cached SDK dependencies"]
fn live_creator_authors_and_invokes_hello_world() {
    run_live_creator(CreatorScenario::Tool);
}

// CPP-CREATOR-014
#[test]
#[ignore = "requires a real DeepSeek connection and the Rust build component"]
fn live_creator_authors_and_exercises_an_interactive_panel() {
    run_live_creator(CreatorScenario::Panel);
}

#[test]
#[ignore = "requires a real DeepSeek connection and the Rust build component"]
fn live_creator_authors_a_slider_from_exact_contracts() {
    run_live_creator(CreatorScenario::Slider);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CreatorScenario {
    Tool,
    Panel,
    Slider,
}

fn run_live_creator(scenario: CreatorScenario) {
    let root =
        PathBuf::from(std::env::var_os("DSHR_LIVE_CREATOR_DIR").expect("isolated workspace"));
    let existing =
        scenario == CreatorScenario::Tool && root.join("hello-world/plugin.toml").is_file();
    assert!(
        existing || std::fs::read_dir(&root).unwrap().next().is_none(),
        "use an empty workspace or a copy of the hello-world package"
    );
    let managed = std::env::var_os("DSHR_LIVE_CREATOR_MANAGED_COMPONENTS").is_some();
    if managed {
        let node = dshr_sidecar_guard::resolve_node().expect("bundled Node");
        assert!(node.is_vendored());
        assert!(
            super::build::toolchain().is_err(),
            "expected no Cargo on the test machine"
        );
        for component in runtime_components::Component::ALL {
            assert!(runtime_components::installed(component).is_none());
        }
        println!("BUNDLED_NODE {}", node.program.display());
    } else {
        assert!(super::build::toolchain().is_ok(), "native Cargo required");
    }
    let model = DeepSeekProvider::from_env().expect("DeepSeek credential");
    let store = SessionIndex::new(root.join("sessions"));
    let id = store.new_id();
    println!("LIVE_CREATOR_TRACE {}", store.path_for(&id).display());
    store
        .create_with_created(
            &id,
            &SessionCreated::root(root.to_string_lossy()).with_preset(CORDIS_PRESET),
            &crate::now_ts(),
        )
        .unwrap();
    crate::prepare_desktop_sidecar(&Profile::defaults(&root));
    let cancel = Arc::new(AtomicBool::new(false));
    let approvals = ApprovalBroker::new();
    let mut state = ChatState::open_with_settings(
        store.path_for(&id),
        &id,
        &cancel,
        approvals.clone(),
        false,
        true,
        DesktopSettings {
            model: Arc::new(DesktopModel::DeepSeek(model)),
            permission: Some(PermissionMode::WorkspaceWrite),
            reasoning: None,
        },
    );
    if managed {
        let call = |state: &mut ChatState, id: &str, tool: &str, arguments| {
            state.execute_tool(&ToolCallRequest {
                call_id: id.to_owned(),
                tool: tool.to_owned(),
                arguments,
            })
        };
        let read_path = root.join("base-read.txt");
        std::fs::write(&read_path, "official read before installing components").unwrap();
        let read = call(
            &mut state,
            "base-read",
            "read",
            serde_json::json!({"file_path": "base-read.txt"}),
        );
        assert!(!read.is_error && read.content.to_string().contains("official read before"));
        println!("OFFICIAL_READ_BEFORE_INSTALL_PASS");
        let python_tool = runtime_components::WORKSPACE_DEPS_TOOL;
        let no_python = call(
            &mut state,
            "python-missing",
            python_tool,
            serde_json::json!({}),
        );
        assert!(no_python.is_error);
        for component in runtime_components::Component::ALL {
            let install = std::process::Command::new(crate::app_root().join("dshr.exe"))
                .args(["--install-runtime-component", component.id()])
                .output()
                .expect("run application component installer");
            assert!(install.status.success(), "{install:?}");
            let destination =
                runtime_components::installed(component).expect("install release asset");
            let asset = runtime_components::asset(component).unwrap();
            println!(
                "INSTALLED {} {} {}",
                component.id(),
                asset.version,
                destination.display()
            );
        }
        let python = call(
            &mut state,
            "python-installed",
            python_tool,
            serde_json::json!({}),
        );
        assert!(!python.is_error, "{python:?}");
        let executable = Path::new(
            python.value["python"]
                .as_str()
                .expect("official Python path"),
        );
        let installed =
            runtime_components::installed(runtime_components::Component::Python).unwrap();
        assert!(executable.starts_with(installed));
        let output = std::process::Command::new(executable)
            .args([
                "-I",
                "-c",
                "import json,sys; print(json.dumps(list(sys.version_info[:3])))",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        println!(
            "OFFICIAL_PYTHON_PASS {}",
            String::from_utf8_lossy(&output.stdout).trim()
        );
        std::fs::remove_file(read_path).unwrap();
    }
    let done = Arc::new(AtomicBool::new(false));
    let finish = done.clone();
    let answer = std::thread::spawn(move || {
        while !finish.load(Ordering::Acquire) {
            for pending in approvals.pending() {
                let path = pending.arguments["path"].as_str().unwrap_or_default();
                let expected = match pending.tool.as_str() {
                    "edit" => {
                        existing && pending.arguments["file_path"] == "hello-world/src/main.rs"
                    }
                    _ if !relative_path(path) => false,
                    tool => match tool {
                        WRITE_FILE_TOOL => path.ends_with(".toml") || path.ends_with(".rs"),
                        BUILD_PLUGIN_TOOL => pending.arguments["entry"]
                            .as_str()
                            .is_some_and(relative_path),
                        TRY_PLUGIN_TOOL => pending.arguments["permissions"]
                            .as_array()
                            .is_some_and(Vec::is_empty),
                        _ => false,
                    },
                };
                let outcome = if expected {
                    ApprovalOutcome::AllowOnce
                } else {
                    ApprovalOutcome::Deny
                };
                approvals.resolve(&pending.call_id, outcome);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });

    let prompt = match scenario {
        CreatorScenario::Slider => "做一个创造模式插件，在聊天右侧栏的独立页签里用滑块调音量，范围 0 到 10，默认 2。用创造模式的查询结果了解接口，构建并试用。",
        CreatorScenario::Panel => "做一个带界面的 Hello World 插件，有名字输入框和打招呼按钮。输入名字后点击按钮，面板显示问候语。",
        CreatorScenario::Tool if existing => "请先检查创造模式的构建环境，再继续构建和试用工作区已有的 hello-world 插件。遇到编译错误请用 edit 或 creator_write_file 修正源码，最终实际调用注册的问候工具。",
        CreatorScenario::Tool => "做一个 hello world 插件，并试运行。",
    };
    let result = state.stream_turn(prompt, &mut |_| {});
    done.store(true, Ordering::Release);
    answer.join().unwrap();
    assert!(result.is_some(), "creator turn failed");

    let events = state.log.events();
    let successful = |tool: &str| {
        events
            .iter()
            .filter(|event| event.event_type == "tool.call" && event.payload["tool"] == tool)
            .find_map(|call| {
                events.iter().find(|event| {
                    event.event_type == "tool.result"
                        && event.payload["call_id"] == call.payload["call_id"]
                        && event.payload["is_error"] == false
                })
            })
    };
    let inspect = successful(INSPECT_TOOL).expect("agent must inspect authoring guidance");
    if !existing {
        assert!(
            successful(WRITE_FILE_TOOL).is_some() || successful("write").is_some(),
            "agent must write the package in the empty workspace"
        );
    }
    let build = successful(BUILD_PLUGIN_TOOL).expect("agent must build via the product tool");
    let trial = successful(TRY_PLUGIN_TOOL).expect("agent must try the built plugin");
    assert_eq!(inspect.payload["value"]["build"]["cargo_found"], true);
    assert!(inspect.seq < build.seq && build.seq < trial.seq);
    let entry = build.payload["value"]["entry"].as_str().unwrap();
    assert!(root.join(entry).is_file());
    let plugin_dir = build.payload["value"]["path"].as_str().unwrap();
    assert!(!root.join(plugin_dir).join("sdk").exists());
    assert!(!events.iter().any(|event| {
        event.event_type == "tool.call"
            && event.payload["tool"]
                .as_str()
                .is_some_and(|tool| tool.starts_with("probe_"))
    }));
    if scenario == CreatorScenario::Panel {
        let panels = trial.payload["value"]["panels"].as_array().unwrap();
        assert!(!panels.is_empty(), "agent must successfully try the panel");
        let try_call = events
            .iter()
            .find(|event| {
                event.event_type == "tool.call"
                    && event.payload["call_id"] == trial.payload["call_id"]
            })
            .unwrap();
        assert_eq!(try_call.payload["arguments"]["expect_panel"], true);
        assert!(!events.iter().any(|event| event.event_type == "tool.result" && event.payload["is_error"] == true),
            "panel authoring should build and try without retries; see the saved trace");
        let plugin_id = trial.payload["value"]["plugin"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_panel_actions(&mut state, &plugin_id);
        println!("LIVE_CREATOR_PANEL_PASS {}", store.path_for(&id).display());
        return;
    }
    if scenario == CreatorScenario::Slider {
        let first_write = events
            .iter()
            .find(|event| {
                event.event_type == "tool.call"
                    && matches!(
                        event.payload["tool"].as_str(),
                        Some(WRITE_FILE_TOOL | "write")
                    )
            })
            .expect("agent must write the package");
        let queries: Vec<_> = events
            .iter()
            .filter(|event| {
                event.seq < first_write.seq
                    && event.event_type == "tool.result"
                    && event.payload["is_error"] == false
                    && event.payload["value"]["contract"].is_object()
            })
            .map(|event| &event.payload["value"])
            .collect();
        assert!(queries
            .iter()
            .any(|value| value["section"] == "slot" && value["name"] == "workspace.sidebar.tab"));
        assert!(queries
            .iter()
            .any(|value| value["section"] == "element" && value["name"] == "Slider"));
        assert!(queries.iter().any(|value| value["section"] == "sdk"));
        assert!(!events.iter().any(|event| event.event_type == "tool.result" && event.payload["is_error"] == true),
            "the queried contracts should support a first-pass build and trial");
        let plugin_id = trial.payload["value"]["plugin"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_slider_action(&mut state, &plugin_id);
        println!("LIVE_CREATOR_SLIDER_PASS {}", store.path_for(&id).display());
        return;
    }
    let registered = trial.payload["value"]["tools"].as_array().unwrap();
    assert!(
        registered
            .iter()
            .filter_map(|name| name.as_str())
            .any(|name| {
                successful(name).is_some_and(|event| {
                    event.seq > trial.seq
                        && event.payload["content"]
                            .to_string()
                            .to_lowercase()
                            .contains("hello")
                })
            }),
        "agent must invoke a registered greeting tool"
    );
    println!("LIVE_CREATOR_PASS {}", store.path_for(&id).display());
}

fn assert_slider_action(state: &mut ChatState, plugin_id: &str) {
    let find_slider = |state: &ChatState| {
        let panel = state
            .adc_panels
            .iter()
            .find(|panel| {
                panel.trial
                    && panel.plugin_id == plugin_id
                    && panel
                        .slot
                        .as_ref()
                        .is_some_and(|slot| slot.slot_id == "workspace.sidebar.tab")
            })
            .expect("sidebar tab contribution");
        let target = dshr_plugin_host::SlotTarget::from(panel.slot.as_ref().unwrap());
        let tree = parse_tree(panel.tree.as_ref().expect("slot rendered"));
        assert!(tree.diagnostics.is_empty());
        let mut slider = None;
        tree.root.walk(&mut |node| {
            if node.kind == ElementKind::Slider && slider.is_none() {
                slider = Some((node.f64_prop("value"), node.action("onChange")));
            }
        });
        let (value, action) = slider.expect("Slider in the contribution");
        (target, value, action)
    };
    let (target, initial, action) = find_slider(state);
    assert_eq!(initial, Some(2.0));
    let Some(AdcAction::Call { method, mut args }) = action else {
        panic!("Slider requires a call action");
    };
    args["value"] = json!(7.5);
    state
        .apply_adc_slot_action(target, "call", &json!({"method": method, "args": args}))
        .unwrap();
    assert_eq!(find_slider(state).1, Some(7.5));
}

fn assert_panel_actions(state: &mut ChatState, plugin_id: &str) {
    let panel = state
        .adc_panels
        .iter()
        .find(|panel| panel.plugin_id == plugin_id)
        .expect("pane visible");
    let view = panel.view.clone();
    let tree = parse_tree(panel.tree.as_ref().expect("first render"));
    assert!(tree.diagnostics.is_empty());
    let mut input = None;
    let mut button = None;
    tree.root.walk(&mut |node| match node.kind {
        ElementKind::Input => {
            input = node.action("onChange");
        }
        ElementKind::Button if button.is_none() => {
            button = node.action("onPress");
        }
        _ => {}
    });
    let Some(AdcAction::Call { method, mut args }) = input else {
        panic!("the name field must pass its value in call.args");
    };
    args.as_object_mut()
        .unwrap()
        .insert("value".to_owned(), json!("Panel Check"));
    state
        .apply_adc_action(
            plugin_id,
            &view,
            "call",
            &json!({"method":method,"args":args}),
        )
        .unwrap();
    let Some(AdcAction::Call { method, args }) = button else {
        panic!("greeting button must send its panel action");
    };
    state
        .apply_adc_action(
            plugin_id,
            &view,
            "call",
            &json!({"method":method,"args":args}),
        )
        .unwrap();
    let panel = state
        .adc_panels
        .iter()
        .find(|panel| panel.plugin_id == plugin_id)
        .unwrap();
    let refreshed = parse_tree(panel.tree.as_ref().expect("render after action"));
    assert!(refreshed.diagnostics.is_empty());
    let mut greet = false;
    refreshed.root.walk(&mut |node| {
        if node.kind == ElementKind::Text {
            greet |= node
                .str_prop("value")
                .is_some_and(|text| text.contains("Panel Check"));
        }
    });
    assert!(
        greet,
        "render after editing and pressing must include the name in the greeting"
    );
}
