#![cfg(test)]

use super::*;

#[test]
fn switching_sessions_rebinds_native_writes_to_the_session_workspace() {
    let root = std::env::temp_dir().join(format!(
        "dshr-workspace-switch-{}-{}",
        std::process::id(),
        now_millis()
    ));
    let first = root.join("first");
    let second = root.join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    let store = SessionIndex::new(root.join("sessions"));
    store
        .create_root("first", &first.to_string_lossy(), &now_ts())
        .unwrap();
    store
        .create_root("second", &second.to_string_lossy(), &now_ts())
        .unwrap();

    let mut state = ChatState::open_in(
        store.path_for("first"),
        "first",
        &Arc::new(AtomicBool::new(false)),
        ApprovalBroker::new(),
        DesktopSettings {
            model: Arc::new(DesktopModel::Mock),
            permission: Some(PermissionMode::DangerFullAccess),
            reasoning: None,
        },
        ChatRuntime::Desktop {
            allow_spawn: false,
            sidecar: false,
            diagnostics: true,
        },
    );
    assert_eq!(state.workspace, first);
    state
        .plugins
        .call(
            "probe_write",
            &json!({"path": "note.txt", "content": "first"}),
        )
        .unwrap();
    state.switch_session(store.path_for("second"), "second");
    assert_eq!(state.workspace, second);
    state
        .plugins
        .call(
            "probe_write",
            &json!({"path": "note.txt", "content": "second"}),
        )
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(first.join("note.txt")).unwrap(),
        "first"
    );
    assert_eq!(
        std::fs::read_to_string(second.join("note.txt")).unwrap(),
        "second"
    );
    drop(state);
    std::fs::remove_dir_all(&root).unwrap();
}
use std::path::Path;

#[test]
fn turn_rail_keeps_the_active_turn_visible_with_bounded_markers() {
    assert_eq!(turn_rail_window(0, 0), 0..0);
    assert_eq!(turn_rail_window(3, 2), 0..3);
    assert_eq!(turn_rail_window(24, 0), 0..7);
    assert_eq!(turn_rail_window(24, 10), 7..14);
    assert_eq!(turn_rail_window(24, 23), 17..24);
}

#[test]
fn command_menu_matches_only_slash_prefixes() {
    assert_eq!(slash_commands("/").collect::<Vec<_>>(), SlashCommand::ALL);
    assert_eq!(
        slash_commands("/se").collect::<Vec<_>>(),
        [SlashCommand::Search]
    );
    assert_eq!(slash_commands("/theme").next(), Some(SlashCommand::Theme));
    for ordinary_message in ["/src/main.rs", "/new.txt", "/new later", "see /search"] {
        assert_eq!(slash_commands(ordinary_message).next(), None);
    }
}

#[test]
fn text_attachment_survives_replay_and_is_sent_once_per_request() {
    struct RecordingModel(Arc<Mutex<Vec<Vec<String>>>>);

    impl ModelProvider for RecordingModel {
        fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
            self.0.lock().unwrap().push(
                request
                    .messages
                    .iter()
                    .filter_map(|message| match message {
                        ChatMessage::User(text) => Some(text.clone()),
                        _ => None,
                    })
                    .collect(),
            );
            Ok(ModelReply::text_only("已收到"))
        }
    }

    let root = std::env::temp_dir().join(format!(
        "dshr-attachment-test-{}",
        now_ts().replace(':', "-")
    ));
    let log = root.join("test.jsonl");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_with_settings(
        log.clone(),
        "test",
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
        DesktopSettings {
            model: Arc::new(DesktopModel::Mock),
            permission: Some(PermissionMode::ReadOnly),
            reasoning: None,
        },
    );
    let requests = Arc::new(Mutex::new(Vec::new()));
    *state.agent.provider_mut() = Box::new(RecordingModel(requests.clone()));
    assert!(state
        .stream_turn_with_attachments(
            "",
            &[DraftAttachment::new(
                "notes.txt".into(),
                AttachmentContent::Text("fixture body".into()),
            )],
            &mut |_| {},
        )
        .is_some());
    let first = "附件 \"notes.txt\"：\nfixture body".to_owned();

    let replayed = SessionLog::from_jsonl("test", &std::fs::read_to_string(&log).unwrap()).unwrap();
    assert!(matches!(
        &build_from_log(&replayed)[0],
        Item::User { text, attachments }
            if text.is_empty() && attachments.len() == 1
                && attachments[0].name == "notes.txt" && attachments[0].file.is_none()
    ));
    let index = SessionIndex::new(&root);
    assert_eq!(index.scan().unwrap()[0].title, "notes.txt");
    assert_eq!(
        history_for_model(&replayed)[0],
        ChatMessage::User(first.clone())
    );
    assert!(state.stream_turn("继续", &mut |_| {}).is_some());
    assert_eq!(
        *requests.lock().unwrap(),
        vec![vec![first.clone()], vec![first, "继续".to_owned()]]
    );
    drop(state);
    index.delete("test").unwrap();
    std::fs::remove_file(root.join("test.jsonl.lock")).unwrap();
    std::fs::remove_dir(&root).unwrap();
}

#[test]
fn media_attachments_survive_restart_and_export() {
    struct RecordingModel(Arc<Mutex<Vec<Vec<ChatMessage>>>>);

    impl ModelProvider for RecordingModel {
        fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
            self.0.lock().unwrap().push(
                request
                    .messages
                    .iter()
                    .filter(|message| message.user_text().is_some())
                    .cloned()
                    .collect(),
            );
            Ok(ModelReply::text_only("已收到"))
        }
    }

    let root = std::env::temp_dir().join(format!(
        "dshr-media-test-{}-{}",
        std::process::id(),
        now_ts().replace(':', "-")
    ));
    std::fs::create_dir(&root).unwrap();
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(300, 150, image::Rgb([32, 96, 224]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let fixtures = [
        ("样图.png", AttachmentFormat::Png, png.get_ref().as_slice()),
        (
            "notes.pdf",
            AttachmentFormat::Pdf,
            b"%PDF-1.4\nfixture\n%%EOF".as_slice(),
        ),
    ];
    let paths = fixtures
        .iter()
        .map(|(name, _, bytes)| {
            let path = root.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        })
        .collect::<Vec<_>>();
    assert!(read_attachments(paths.clone(), 1, 32 * 1024, 1024).is_err());
    assert!(read_attachments(paths.clone(), 4, 32 * 1024, 8).is_err());
    let attachments = read_attachments(paths, 4, 32 * 1024, 4096).unwrap();
    let thumbnail = ComposerAttachment::thumbnail(&attachments[0]).unwrap();
    assert_eq!(thumbnail.size(0), size(200u32.into(), 100u32.into()));
    assert_eq!(&thumbnail.as_bytes(0).unwrap()[..4], &[224, 96, 32, 255]);
    assert!(ComposerAttachment::thumbnail(&attachments[1]).is_none());

    let cancel = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let open = || {
        let mut state = ChatState::open_with_settings(
            root.join("test.jsonl"),
            "test",
            &cancel,
            ApprovalBroker::new(),
            false,
            false,
            DesktopSettings {
                model: Arc::new(DesktopModel::Mock),
                permission: Some(PermissionMode::ReadOnly),
                reasoning: None,
            },
        );
        state.spill = SpillStore::new(root.join("blobs"));
        *state.agent.provider_mut() = Box::new(RecordingModel(requests.clone()));
        state
    };
    let mut state = open();
    assert!(state
        .stream_turn_with_attachments("", &attachments, &mut |_| {})
        .is_some());
    drop(state);
    let mut state = open();
    let projection = build_from_log(&state.log);
    let Item::User { text, attachments } = &projection[0] else {
        panic!("missing user attachments after reopening");
    };
    assert!(text.is_empty());
    assert_eq!(attachments.len(), 2);
    for (attachment, (name, format, bytes)) in attachments.iter().zip(fixtures) {
        assert_eq!(attachment.name, name);
        let file = attachment.file.as_ref().unwrap();
        assert_eq!(file.size, bytes.len() as u64);
        assert_eq!(file.mime, format.mime());
    }
    let stored = StoredAttachment {
        root: state.spill.root().to_owned(),
        file: attachments[0].file.as_ref().unwrap().clone(),
    };
    let preview = PreviewSource::Stored(stored.clone()).load().unwrap();
    assert_eq!(preview.size(0), size(300u32.into(), 150u32.into()));
    assert_eq!(
        SessionIndex::new(&root).scan().unwrap()[0].title,
        "样图.png"
    );
    assert!(state.stream_turn("继续", &mut |_| {}).is_some());

    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].len(), 1);
    assert_eq!(recorded[1].len(), 2);
    assert_eq!(recorded[1][0], recorded[0][0]);
    assert_eq!(recorded[1][1], ChatMessage::user("继续"));
    let ChatMessage::UserWithAttachments { text, attachments } = &recorded[0][0] else {
        panic!("missing media user message");
    };
    assert!(text.is_empty());
    assert_eq!(attachments.len(), fixtures.len());
    for (attachment, (name, format, bytes)) in attachments.iter().zip(fixtures) {
        assert_eq!(attachment.name, name);
        assert_eq!(attachment.format, format);
        assert_eq!(attachment.bytes.as_deref(), Some(bytes));
    }

    let out = root.join("export");
    let manifest = export_session(
        &state.log,
        &SpillStoreSource::new(&state.spill),
        &out,
        &ExportOptions::new(now_ts()).fail_on_missing_blob(true),
    )
    .unwrap();
    assert!(verify_export(&out, "test").unwrap().is_ok());
    assert_eq!(manifest.attachments.len(), fixtures.len());
    for (_, format, bytes) in fixtures {
        let entry = manifest
            .attachments
            .iter()
            .find(|entry| entry.mime == format.mime())
            .unwrap();
        assert_eq!(std::fs::read(out.join(&entry.path)).unwrap(), bytes);
    }
    assert!(!std::fs::read(out.join(&manifest.jsonl.path))
        .unwrap()
        .windows(b"%PDF-1.4".len())
        .any(|window| window == b"%PDF-1.4"));
    let mut corrupted = fixtures[0].2.to_vec();
    corrupted[0] ^= 0xff;
    std::fs::write(state.spill.object_path(&stored.file), corrupted).unwrap();
    assert!(PreviewSource::Stored(stored).load().is_err());
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn clipboard_bitmap_is_converted_to_png_with_media_budget() {
    use image::{ImageFormat, Rgb, RgbImage};
    use std::io::Cursor;

    let mut bitmap = Cursor::new(Vec::new());
    RgbImage::from_pixel(2, 1, Rgb([10, 20, 30]))
        .write_to(&mut bitmap, ImageFormat::Bmp)
        .unwrap();
    let bitmap = bitmap.into_inner();
    assert!(read_clipboard_image(bitmap.clone(), 8).is_err());
    let attachment = read_clipboard_image(bitmap, 1024).unwrap();
    assert_eq!(attachment.name, "剪贴板图片.png");
    let AttachmentContent::Media { format, bytes } = attachment.content else {
        panic!("expected an image attachment");
    };
    assert_eq!(format, AttachmentFormat::Png);
    let png = image::load_from_memory_with_format(&bytes, ImageFormat::Png)
        .unwrap()
        .into_rgb8();
    assert_eq!(png.dimensions(), (2, 1));
    assert_eq!(png.get_pixel(0, 0), &Rgb([10, 20, 30]));
}

#[test]
fn permission_changes_are_visible_without_the_chat_state_lock() {
    let root = std::env::temp_dir().join(format!(
        "dshr-permission-test-{}",
        now_ts().replace(':', "-")
    ));
    let approvals = ApprovalBroker::new();
    let state = Mutex::new(ChatState::open_in(
        root.join("test.jsonl"),
        "test",
        &Arc::new(AtomicBool::new(false)),
        approvals.clone(),
        DesktopSettings {
            model: Arc::new(DesktopModel::Mock),
            permission: Some(PermissionMode::WorkspaceWrite),
            reasoning: None,
        },
        ChatRuntime::Desktop {
            allow_spawn: false,
            sidecar: false,
            diagnostics: true,
        },
    ));
    let permission = state.lock().unwrap().permission.clone();
    let state = state.lock().unwrap();
    let args = json!({ "path": "probe.txt", "content": "fixture" });
    assert!(matches!(
        state.plugins.registry().authorize("probe_write", &args),
        Err(RegistryError::NeedsApproval { .. })
    ));
    permission.set(PermissionMode::ReadOnly);
    assert!(matches!(
        state.plugins.registry().authorize("probe_write", &args),
        Err(RegistryError::Denied { .. })
    ));
    assert!(!permission.can_write());
    permission.set(PermissionMode::DangerFullAccess);
    assert!(state
        .plugins
        .registry()
        .authorize("probe_write", &args)
        .is_ok());
    assert!(permission.can_write());
    drop(state);
    SessionIndex::new(&root).delete("test").unwrap();
    std::fs::remove_file(root.join("test.jsonl.lock")).unwrap();
    std::fs::remove_dir(&root).unwrap();
}

#[test]
fn always_allow_keys_scope_the_tool_to_its_workspace_and_session() {
    let workspace = Path::new("workspace");
    let tool = ToolDefinition::new(
        "write",
        "test",
        ToolOrigin::Native {
            plugin_id: "fixture".to_owned(),
        },
    );
    let key = tool_approval_key("session-a", workspace, &tool);
    assert!(key.is_some());
    assert_ne!(key, tool_approval_key("session-b", workspace, &tool));
    assert_ne!(
        key,
        tool_approval_key("session-a", Path::new("other"), &tool)
    );
    let new_schema = tool.clone().with_schema(json!({ "type": "object" }));
    assert_ne!(key, tool_approval_key("session-a", workspace, &new_schema));
    let other_provider = ToolDefinition::new(
        "write",
        "test",
        ToolOrigin::Native {
            plugin_id: "other".to_owned(),
        },
    );
    assert_ne!(
        key,
        tool_approval_key("session-a", workspace, &other_provider)
    );
}

#[test]
fn reasoning_and_consecutive_tool_calls_replay_as_one_process() {
    let mut log = SessionLog::new("process");
    let ts = now_ts();
    for (kind, payload) in [
        (
            "message.user",
            json!({ "message_id": "u", "text": "question" }),
        ),
        (
            "message.assistant.reasoning",
            json!({ "text": "check both" }),
        ),
        ("tool.call", json!({ "call_id": "a", "tool": "read" })),
        ("tool.call", json!({ "call_id": "b", "tool": "read" })),
        ("tool.result", json!({ "call_id": "a", "content": [] })),
        ("tool.result", json!({ "call_id": "b", "content": [] })),
        (
            "message.assistant.reasoning",
            json!({ "text": "summarize" }),
        ),
        (
            "message.assistant.settled",
            json!({ "message_id": "a", "text": "answer" }),
        ),
    ] {
        log.append(EventSpec::durable(kind, &ts, payload)).unwrap();
    }
    let rows = rows_snapshot(&log, None);
    assert!(matches!(
        rows.as_slice(),
        [Row::User { .. }, Row::Process(steps), Row::Assistant(text)]
            if text == "answer" && matches!(steps.as_slice(), [
                Row::Reasoning { text: first, streaming: false },
                Row::Tool { pending: false, .. }, Row::Tool { pending: false, .. },
                Row::Reasoning { text: last, streaming: false },
            ] if first == "check both" && last == "summarize")
    ));
    let history = history_from_log(&log);
    assert!(matches!(
        history.as_slice(),
        [
            ChatMessage::User(_),
            ChatMessage::Assistant { reasoning: first, tool_calls, .. },
            ChatMessage::Tool { .. }, ChatMessage::Tool { .. },
            ChatMessage::Assistant { reasoning: last, text, .. },
        ] if first == "check both" && tool_calls.len() == 2 && last == "summarize" && text == "answer"
    ));
}

fn args(values: &[&str]) -> Vec<String> {
    // 第 0 个元素模拟 exe 路径，与 `std::env::args()` 一致。
    std::iter::once("dshr.exe")
        .chain(values.iter().copied())
        .map(str::to_owned)
        .collect()
}

#[test]
fn gui_launch_is_not_cli() {
    assert!(!is_cli_invocation(&args(&[])));
}

#[test]
fn cli_switches_attach_console() {
    for flag in [
        "--selftest",
        "--selftest-stream",
        "--selftest-dsh",
        "--selftest-l2",
        "--selftest-official",
        "--selftest-parallel-tools",
        "--selftest-dsh-write",
        "--selftest-approval",
        "--selftest-corrupt-session",
        "--selftest-crash-recovery",
        "--selftest-window-recovery",
        "--selftest-crash-child",
        "--selftest-recovery",
        "--selftest-sidecar-recovery",
        "--selftest-cancel",
        "--live",
        "--live-chat",
        "--export-session",
        "--retention-plan",
        "--install-plugin",
        "--uninstall-plugin",
        "--trust-key",
        "--selftest-plugin-lifecycle",
        "--seed-perf-session",
        "--console",
    ] {
        assert!(
            is_cli_invocation(&args(&[flag])),
            "{flag} 应识别为 CLI 用法"
        );
    }
}

// M4-A11Y-001：快捷键表与绑定数量一致、覆盖 G3 主流程键位、提示文案含平台主修饰键。
#[test]
fn keyboard_shortcut_table_is_unique_and_covers_primary_flow() {
    use std::collections::HashSet;

    let keys: HashSet<&str> = CHAT_SHORTCUTS.iter().map(|(key, _)| *key).collect();
    assert_eq!(keys.len(), CHAT_SHORTCUTS.len(), "快捷键不能重复");
    for required in [
        "cmd-n",
        "cmd-k",
        "cmd-b",
        "cmd-j",
        "cmd-shift-a",
        "cmd-shift-d",
        "escape",
        "cmd-w",
    ] {
        assert!(keys.contains(required), "缺少快捷键 {required}");
    }
    assert_eq!(
        chat_key_bindings().len(),
        CHAT_SHORTCUTS.len() + 2,
        "绑定数量必须与说明表一致"
    );
    let hint = chat_shortcut_hint();
    assert!(hint.contains("Ctrl") || hint.contains("Cmd"), "{hint}");
    assert_eq!(format_shortcut("cmd-shift-a", "Ctrl"), "Ctrl+Shift+A");
    assert_eq!(format_shortcut("escape", "Ctrl"), "Esc");
}

#[test]
fn dshr_home_wins_over_packaged_layout() {
    let home = Path::new("D:\\dshr-home");
    let picked = pick_data_root(
        Some(home),
        Some(Path::new("C:\\app")),
        Some(Path::new("C:\\Users\\me\\AppData\\Local")),
        true,
    );
    assert_eq!(picked, Some((DataRootSource::Home, home.to_path_buf())));
}

#[test]
fn packaged_layout_prefers_exe_dir() {
    let exe = Path::new("C:\\app");
    let picked = pick_data_root(
        None,
        Some(exe),
        Some(Path::new("C:\\Users\\me\\AppData\\Local")),
        true,
    );
    assert_eq!(picked, Some((DataRootSource::ExeDir, exe.to_path_buf())));
}

#[test]
fn dev_layout_uses_local_app_data() {
    // 从 target/release 跑（exe 旁没有 sidecar/）→ 数据落 %LOCALAPPDATA%\dshr。
    let local = Path::new("C:\\Users\\me\\AppData\\Local");
    let picked = pick_data_root(
        None,
        Some(Path::new("C:\\repo\\target\\release")),
        Some(local),
        false,
    );
    assert_eq!(
        picked,
        Some((DataRootSource::LocalAppData, local.join("dshr")))
    );
}

#[test]
fn without_any_candidate_there_is_no_pick() {
    assert_eq!(pick_data_root(None, None, None, false), None);
    // 打包布局标记不可用（不可写）时也不能选 exe 目录。
    assert_eq!(
        pick_data_root(None, Some(Path::new("C:\\app")), None, false),
        None
    );
}

#[test]
fn tool_timeout_is_clamped_to_sane_range() {
    assert_eq!(clamp_tool_timeout_ms(0), Duration::from_secs(1));
    assert_eq!(clamp_tool_timeout_ms(999), Duration::from_secs(1));
    assert_eq!(clamp_tool_timeout_ms(120_000), Duration::from_secs(120));
    assert_eq!(clamp_tool_timeout_ms(601_000), Duration::from_secs(600));
    assert_eq!(clamp_tool_timeout_ms(u64::MAX), Duration::from_secs(600));
}

#[test]
fn sidecar_tool_timeout_follows_declaration_without_override() {
    if std::env::var_os("DSHR_TOOL_TIMEOUT_MS").is_some() {
        // 显式覆盖存在时以覆盖为准（clamp 后）。
        let definition = ToolDefinition::new(
            "x",
            "x",
            ToolOrigin::Native {
                plugin_id: "p".into(),
            },
        );
        assert_eq!(
            sidecar_tool_timeout(&definition),
            clamp_tool_timeout_ms(
                std::env::var("DSHR_TOOL_TIMEOUT_MS")
                    .expect("present")
                    .parse()
                    .expect("number")
            )
        );
        return;
    }
    let mut definition = ToolDefinition::new(
        "bash",
        "run shell",
        ToolOrigin::Native {
            plugin_id: "p".into(),
        },
    );
    definition.timeout_ms = 30_000;
    assert_eq!(sidecar_tool_timeout(&definition), Duration::from_secs(30));
    definition.timeout_ms = 120_000;
    assert_eq!(sidecar_tool_timeout(&definition), Duration::from_secs(120));
}
