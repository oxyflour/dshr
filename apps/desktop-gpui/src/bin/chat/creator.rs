use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dshr_agent_core::{ToolCallRequest, ToolOutcome, ToolSpec};
use dshr_plugin_api::manifest::{RuntimeKind, CPP_API};
use dshr_plugin_api::{Manifest, PluginError};
use dshr_plugin_host::PluginHost;
use dshr_session_store::SessionLog;
use dshr_tool_runtime::{AllowAll, ApprovalBroker, ApprovalOutcome, ToolOrigin};
use dshr_ui_contributions::parse_tree;
use serde_json::{json, Value};
use tempfile::TempDir;

#[path = "creator/build.rs"]
mod build;

pub(super) const PRESET: &str = "dshr-creator";
pub(super) const CORDIS_PRESET: &str = "cordis";
pub(super) const WRITE_FILE_TOOL: &str = "creator_write_file";
pub(super) const BUILD_PLUGIN_TOOL: &str = "creator_build_plugin";
pub(super) const TRY_PLUGIN_TOOL: &str = "creator_try_plugin";
const INSPECT_TOOL: &str = "creator_inspect";
const PREVIEW_TOOL: &str = "creator_preview_panel";

const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PACKAGE_ENTRIES: usize = 2048;

pub(super) fn trial_panel_calls(log: &SessionLog) -> HashMap<&str, u64> {
    let events = log.events();
    let mut calls = HashMap::new();
    for result in events
        .iter()
        .rev()
        .filter(|event| event.event_type == "tool.result")
    {
        let value = &result.payload["value"];
        let Some((plugin, seq)) = value["plugin"]
            .as_str()
            .zip(result.payload["call_seq"].as_u64())
        else {
            continue;
        };
        if calls.contains_key(plugin) || result.payload["is_error"] != false {
            continue;
        }
        if let Ok(index) = events.binary_search_by_key(&seq, |event| event.seq) {
            let call = &events[index];
            if call.event_type == "tool.call"
                && call.payload["tool"] == TRY_PLUGIN_TOOL
                && call.payload["origin"] == "native"
            {
                calls.insert(plugin, seq);
            }
        }
    }
    calls
}
const MAX_DIRECTORY_DEPTH: usize = 32;

pub(super) fn is_creator_session(log: &SessionLog) -> bool {
    log.created().is_some_and(|created| {
        !created.is_child() && matches!(created.preset.as_deref(), Some(PRESET | CORDIS_PRESET))
    })
}

pub(super) fn specs() -> [ToolSpec; 4] {
    [
        ToolSpec {
            name: INSPECT_TOOL.to_owned(),
            description: "Start here for a plugin task. Read workspace_panel for a complete, compilable panel with an Input and Button, including their action payloads. Read it before writing or extending a plugin UI.".to_owned(),
            parameters: object_schema(None),
        },
        ToolSpec {
            name: PREVIEW_TOOL.to_owned(),
            description: "Optional visual preview of an ADC-v1 tree, including controls. The preview never dispatches actions or registers a panel. Copy the element and action format from creator_inspect.workspace_panel before calling; then build and try the plugin.".to_owned(),
            parameters: object_schema(Some(("element", "object"))),
        },
        ToolSpec {
            name: BUILD_PLUGIN_TOOL.to_owned(),
            description: "Build a Rust external-process plugin. Pass the workspace-relative directory containing plugin.toml, Cargo.toml and src/. The host supplies the SDK, requests approval and writes the executable to runtime.entry. Fix compiler errors before retrying.".to_owned(),
            parameters: object_schema(Some(("path", "string"))),
        },
        ToolSpec {
            name: TRY_PLUGIN_TOOL.to_owned(),
            description: "Try a built CPP-v1 plugin, or replace the trial of the same package after a rebuild. For UI tasks pass expect_panel:true and require a nonempty panels result. This checks the first render; report actual clicks only if exercised.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative built package directory"},
                    "expect_panel": {"type": "boolean", "description": "Require a workspace panel with a valid first render"}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
    ]
}

fn object_schema(property: Option<(&str, &str)>) -> Value {
    let (properties, required) = match property {
        Some((name, kind)) => (json!({ (name): { "type": kind } }), json!([name])),
        None => (json!({}), json!([])),
    };
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

pub(super) struct Session<'a> {
    pub preset: &'a str,
    pub workspace: &'a Path,
    pub sessions_dir: &'a Path,
    pub scope: &'a mut TrialScope,
    pub read_only: bool,
}

impl Session<'_> {
    pub(super) fn execute(
        &mut self,
        call: &ToolCallRequest,
        tool_names: &[String],
        approvals: &ApprovalBroker,
        cancel: &AtomicBool,
    ) -> ToolOutcome {
        let Some(args) = call.arguments.as_object() else {
            return ToolOutcome::failure("创作工具需要 JSON 对象参数");
        };
        let result = match call.tool.as_str() {
            INSPECT_TOOL if args.is_empty() => Ok(inspect(self.preset, tool_names)),
            PREVIEW_TOOL if args.len() == 1 && args.contains_key("element") => {
                preview_panel(&args["element"])
            }
            BUILD_PLUGIN_TOOL if args.len() == 1 => {
                match args.get("path").and_then(Value::as_str) {
                    Some(path) => build::run(self, path, call, approvals, cancel),
                    None => Err("path 必须是工作区相对路径".to_owned()),
                }
            }
            TRY_PLUGIN_TOOL
                if args
                    .keys()
                    .all(|key| matches!(key.as_str(), "path" | "expect_panel")) =>
            {
                match (
                    args.get("path").and_then(Value::as_str),
                    args.get("expect_panel"),
                ) {
                    (Some(path), None) => self.try_plugin(path, false, call, approvals, cancel),
                    (Some(path), Some(Value::Bool(expect))) => {
                        self.try_plugin(path, *expect, call, approvals, cancel)
                    }
                    _ => Err("path 必须是工作区相对路径，expect_panel 必须是布尔值".to_owned()),
                }
            }
            _ => Err("创作工具参数不符合工具定义".to_owned()),
        };
        result.unwrap_or_else(ToolOutcome::failure)
    }

    fn try_plugin(
        &mut self,
        path: &str,
        expect_panel: bool,
        call: &ToolCallRequest,
        approvals: &ApprovalBroker,
        cancel: &AtomicBool,
    ) -> Result<ToolOutcome, String> {
        if self.read_only {
            return Err("只读权限下无法启动插件试用".to_owned());
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("插件试用已取消".to_owned());
        }

        let staging_root = self.sessions_dir.join(".creator-trials");
        let (trial, manifest, runtime) = prepare_trial(self.workspace, path, &staging_root)?;
        let plugin_id = &manifest.plugin.id;
        let previous = self
            .scope
            .trials
            .iter()
            .position(|item| item.plugin_id == *plugin_id);
        if previous.is_some_and(|index| self.scope.trials[index].source != trial.source) {
            return Err(format!(
                "插件 {plugin_id} 已从另一个包目录试用。请使用原包目录或更改插件 ID。"
            ));
        }
        if previous.is_none()
            && self
                .scope
                .plugins
                .loaded_plugins()
                .contains(&plugin_id.as_str())
        {
            return Err(format!("插件 ID {plugin_id} 与创造模式内置插件冲突"));
        }

        let preview = json!({
            "plugin": plugin_id,
            "version": manifest.plugin.version,
            "runtime": runtime,
            "path": path,
            "permissions": permission_names(&manifest),
            "replaced": previous.is_some(),
        });
        if cancel.load(Ordering::Relaxed)
            || approvals.request_user_cancellable(
                &call.call_id,
                TRY_PLUGIN_TOOL,
                &preview,
                Duration::from_secs(300),
                cancel,
            ) != ApprovalOutcome::AllowOnce
            || cancel.load(Ordering::Relaxed)
        {
            return Err("插件试用未获批准".to_owned());
        }

        let mut candidate = PluginHost::new(AllowAll);
        super::load_cpp_package(&mut candidate, manifest, trial.directory.path())?;
        let plugin_id = &trial.plugin_id;
        let panels = validate_trial_panels(&mut candidate, plugin_id, expect_panel)?;
        if cancel.load(Ordering::Relaxed) {
            return Err("插件试用已取消".to_owned());
        }
        let tools: Vec<_> = candidate
            .registry()
            .list()
            .into_iter()
            .filter(|tool| {
                !tool.name.starts_with("probe_")
                    && matches!(&tool.origin, ToolOrigin::Native { plugin_id: owner } if owner == plugin_id)
            })
            .map(|tool| &tool.name)
            .collect();
        let mut result = json!({ "plugin": plugin_id, "tools": tools, "panels": panels, "replaced": previous.is_some() });
        if !panels.is_empty() {
            result["panel_location"] = json!("Inside this trial's result card in the current conversation, after this turn finishes.");
        }
        self.scope
            .plugins
            .adopt_candidate(candidate, plugin_id)
            .map_err(|error| format!("插件 {plugin_id} 装载失败: {error}"))?;
        if let Some(index) = previous {
            self.scope.trials[index] = trial;
        } else {
            self.scope.trials.push(trial);
        }
        self.scope.revision += 1;
        Ok(json_outcome(result))
    }
}

fn validate_trial_panels(
    candidate: &mut PluginHost,
    plugin_id: &str,
    expect_panel: bool,
) -> Result<Vec<String>, String> {
    let panels: Vec<_> = candidate
        .panels()
        .into_iter()
        .filter(|registration| {
            registration.plugin_id == plugin_id && registration.panel.slot == "workspace.panel"
        })
        .map(|registration| {
            (
                registration.panel.title.clone(),
                registration.panel.view_name().to_owned(),
            )
        })
        .collect();
    if expect_panel && panels.is_empty() {
        return Err("插件未注册 workspace.panel。将 PanelDecl 放进 ReadyDecls::new 的第二个参数，查看 creator_inspect.workspace_panel 后重建并试用。".to_owned());
    }
    let mut titles = Vec::with_capacity(panels.len());
    for (title, view) in panels {
        let tree = candidate
            .render_panel(plugin_id, &view)
            .map_err(|error| format!("面板「{title}」的 view {view} 无法渲染: {error}"))?;
        let parsed = parse_tree(&tree);
        if let Some(diagnostic) = parsed.diagnostics.first() {
            return Err(format!(
                "面板「{title}」的 view {view} 在 {} 校验失败: {}。查看 creator_inspect.workspace_panel。",
                diagnostic.path, diagnostic.message
            ));
        }
        titles.push(title);
    }
    Ok(titles)
}

pub(super) fn build_available() -> Result<(), String> {
    build::toolchain().map(|_| ())
}

fn inspect(preset: &str, tools: &[String]) -> ToolOutcome {
    let build = match build::toolchain() {
        Ok(_) => json!({ "cargo_found": true, "sdk": "included by the build tool" }),
        Err(error) => json!({
            "cargo_found": false,
            "sdk": "included by the build tool",
            "setup_error": error,
            "component_available": crate::runtime_components::asset(
                crate::runtime_components::Component::CreatorBuild
            ).is_ok(),
        }),
    };
    let external_entry = if cfg!(windows) {
        "bin/hello-world.exe"
    } else {
        "bin/hello-world"
    };
    let manifest_example = |kind, entry| {
        format!(
            "[plugin]\nid = \"example.hello-world\"\nversion = \"0.1.0\"\napi = \"{CPP_API}\"\n\n[runtime]\nkind = \"{kind}\"\nentry = \"{entry}\"\n"
        )
    };
    json_outcome(json!({
        "preset": preset,
        "tools": tools,
        "creator_tools": [INSPECT_TOOL, PREVIEW_TOOL, WRITE_FILE_TOOL, BUILD_PLUGIN_TOOL, TRY_PLUGIN_TOOL],
        "build": build,
        "plugin_manifest": "plugin.toml",
        "api": CPP_API,
        "runtime_kinds": ["external-process", "wasm-component"],
        "manifest": {
            "examples": {
                "external-process": manifest_example("external-process", external_entry),
                "wasm-component": manifest_example("wasm-component", "plugin.wasm"),
            },
            "required": "[plugin] requires id (lowercase vendor.name), version (SemVer), and api. [runtime] declares kind and entry for a trial package. entry is relative to plugin.toml and uses forward slashes.",
            "validation": "The parser rejects unknown fields, including [plugin].name, [plugin].display_name and [runtime].command. Omit [permissions] for the default no permissions.",
            "entry": format!("Use a built executable such as {external_entry} for external-process, or the compiled .wasm for wasm-component. creator_build_plugin places the Rust executable at entry."),
            "ui": "For Rust external-process, register panels in process.ready using ReadyDecls. See workspace_panel for complete files.",
        },
        "rust_external_process": "Create Cargo.toml (package name matches the entry basename without .exe, edition 2021, [workspace], and dshr-plugin-sdk = { path = \"sdk\" }) and src/main.rs. Use ToolDecl::new(name, description), ReadyDecls::new(vec![tool], vec![]), and return ChildSession::serve(decls, |call: &ToolCall, _: &CancelFlag| -> Result<ToolResult, ToolError> { Ok(ToolResult::text(message)) }) from main -> Result<(), SessionError>. Use call.tool for the name, call.arguments for arguments, and ToolError::invalid_arguments(message) for invalid input.",
        "workspace_panel": {
            "plugin.toml": manifest_example("external-process", external_entry).replace("hello-world", "hello-panel"),
            "Cargo.toml": "[package]\nname = \"hello-panel\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndshr-plugin-sdk = { path = \"sdk\" }\nserde_json = \"=1.0.154\"\n\n[workspace]\n",
            "src/main.rs": include_str!("creator/panel-example.rs"),
            "workflow": "Write these files in one package directory. Use the direct serde_json dependency for json! and Value. Build injects sdk/ into a temporary directory. Build and then try with expect_panel:true. Confirm panels is nonempty before saying the panel is available.",
            "actions": "Use props.a11yLabel and props.onChange/onPress {\"call\":\"method\",\"args\":{}}. On Input submit the SDK receives (view, \"call\", {\"method\":\"method\",\"args\":{\"value\":\"typed text\"}}). A command receives (view, \"command\", {\"id\":\"command id\"}). The preview cannot trigger these callbacks."
        },
        "panel_slot": "workspace.panel",
        "panel_contract": "ADC-v1",
        "authoring": "Write the package, build it with creator_build_plugin, then try it and call the registered tool. Build and trial request approval. If build.component_available is true and Cargo is missing, direct the user to Settings → 运行组件 to install 创造模式构建组件. Otherwise report the setup error."
    }))
}

fn preview_panel(tree: &Value) -> Result<ToolOutcome, String> {
    let parsed = parse_tree(tree);
    if let Some(diagnostic) = parsed.diagnostics.first() {
        return Err(format!(
            "ADC 预览在 {} 校验失败: {}。查看 creator_inspect.workspace_panel 的元素与动作示例。",
            diagnostic.path, diagnostic.message
        ));
    }
    let mut outcome = json_outcome(
        json!({ "preview": "adc-v1", "nodes": parsed.node_count(), "interactive": false }),
    );
    outcome.meta = Some(json!({ "ui": tree }));
    Ok(outcome)
}

fn json_outcome(value: Value) -> ToolOutcome {
    ToolOutcome::success(json!([{ "type": "json", "value": &value }]), value)
}

pub(super) struct TrialScope {
    // Drop the running plugins before removing the package snapshots.
    pub plugins: PluginHost,
    trials: Vec<Trial>,
    pub(super) revision: u64,
}

impl TrialScope {
    pub(super) fn new(permission: super::LivePermission, workspace: &Path) -> Self {
        let mut plugins = PluginHost::new(permission);
        let result =
            super::WorkspaceWritePlugin::new(workspace, WRITE_FILE_TOOL, "builtin.creator-write")
                .and_then(|writer| {
                    plugins
                        .load(Box::new(writer))
                        .map_err(|error| error.to_string())
                });
        if let Err(error) = result {
            eprintln!("[chat] 创造会话写入工具不可用: {error}");
        }
        Self {
            plugins,
            trials: Vec::new(),
            revision: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.trials.len()
    }
}

impl Drop for TrialScope {
    fn drop(&mut self) {
        while let Some(trial) = self.trials.pop() {
            let _ = self.plugins.unload(&trial.plugin_id);
        }
    }
}

struct Trial {
    plugin_id: String,
    source: PathBuf,
    directory: TempDir,
}

fn prepare_trial(
    workspace: &Path,
    relative: &str,
    staging_root: &Path,
) -> Result<(Trial, Manifest, &'static str), String> {
    let source = source_directory(workspace, relative)?;
    fs::create_dir_all(staging_root).map_err(|error| error.to_string())?;
    let directory = tempfile::Builder::new()
        .prefix("trial-")
        .tempdir_in(staging_root)
        .map_err(|error| error.to_string())?;
    if directory
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?
        .starts_with(&source)
    {
        return Err("插件包目录不能包含宿主试用目录".to_owned());
    }

    let mut budget = SnapshotBudget {
        bytes: MAX_PACKAGE_BYTES,
        entries: MAX_PACKAGE_ENTRIES,
    };
    snapshot_directory(&source, directory.path(), 0, &mut budget)?;
    let manifest = read_manifest(directory.path(), relative)?;
    let runtime = match manifest.runtime.kind {
        RuntimeKind::ExternalProcess => "external-process",
        RuntimeKind::WasmComponent => "wasm-component",
        _ => return Err("试用支持 external-process 和 wasm-component".to_owned()),
    };
    let entry = manifest.runtime.entry.as_deref().unwrap_or("plugin.wasm");
    let entry_path = directory.path().join(entry);
    let resolved = entry_path.canonicalize().map_err(|error| {
        let entry = Path::new(relative).join(entry);
        if error.kind() == io::ErrorKind::NotFound {
            format!(
                "缺少构建产物 `{}`。Rust 插件可先调用 creator_build_plugin，构建成功后再试用。",
                entry.display()
            )
        } else {
            format!("入口文件 `{}` 无效。{error}", entry.display())
        }
    })?;
    let root = directory
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err("入口必须是插件包内的普通文件".to_owned());
    }
    let trial = Trial {
        plugin_id: manifest.plugin.id.clone(),
        source: source.canonicalize().map_err(|error| error.to_string())?,
        directory,
    };
    Ok((trial, manifest, runtime))
}

fn read_manifest(package: &Path, relative: &str) -> Result<Manifest, String> {
    let manifest_path = Path::new(relative).join("plugin.toml");
    let manifest_error = |error: PluginError| {
        let detail = match error {
            PluginError::Manifest { reason, .. } => reason,
            other => other.to_string(),
        };
        format!(
            "插件清单 `{}` 无效。{detail}\n调用 creator_inspect 查看清单样例，修正原文件后再试用。",
            manifest_path.display()
        )
    };
    let manifest = Manifest::from_path(package.join("plugin.toml")).map_err(manifest_error)?;
    manifest
        .validate(&super::host_sdk_version())
        .map_err(manifest_error)?;
    Ok(manifest)
}

fn source_directory(workspace: &Path, relative: &str) -> Result<PathBuf, String> {
    let root = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let mut source = root.clone();
    for component in Path::new(relative).components() {
        if component == Component::CurDir {
            continue;
        }
        let Component::Normal(name) = component else {
            return Err("path 必须指向工作区内的子目录".to_owned());
        };
        source.push(name);
        if !checked_metadata(&source)?.is_dir() {
            return Err("path 必须指向插件包目录".to_owned());
        }
    }
    if source == root
        || !source
            .canonicalize()
            .map_err(|error| error.to_string())?
            .starts_with(&root)
    {
        return Err("path 必须指向工作区内的子目录".to_owned());
    }
    Ok(source)
}

struct SnapshotBudget {
    bytes: u64,
    entries: usize,
}

fn snapshot_directory(
    source: &Path,
    target: &Path,
    depth: usize,
    budget: &mut SnapshotBudget,
) -> Result<(), String> {
    if depth > MAX_DIRECTORY_DEPTH {
        return Err("插件包目录超过 32 层".to_owned());
    }
    for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
        if budget.entries == 0 {
            return Err("插件包超过 2048 个条目".to_owned());
        }
        budget.entries -= 1;
        let entry = entry.map_err(|error| error.to_string())?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let metadata = checked_metadata(&from)?;
        if metadata.is_dir() {
            fs::create_dir(&to).map_err(|error| error.to_string())?;
            snapshot_directory(&from, &to, depth + 1, budget)?;
        } else if metadata.is_file() {
            copy_snapshot_file(&from, &to, &metadata, budget)?;
        } else {
            return Err(format!("插件包包含特殊文件: {}", from.display()));
        }
    }
    Ok(())
}

fn copy_snapshot_file(
    from: &Path,
    to: &Path,
    metadata: &fs::Metadata,
    budget: &mut SnapshotBudget,
) -> Result<(), String> {
    if metadata.len() > budget.bytes {
        return Err("插件包超过 64 MiB".to_owned());
    }
    let input = fs::File::open(from).map_err(|error| error.to_string())?;
    let mut output = fs::File::create(to).map_err(|error| error.to_string())?;
    let copied = io::copy(&mut input.take(budget.bytes + 1), &mut output)
        .map_err(|error| error.to_string())?;
    budget.bytes = budget
        .bytes
        .checked_sub(copied)
        .ok_or("插件包超过 64 MiB")?;
    #[cfg(unix)]
    fs::set_permissions(to, metadata.permissions()).map_err(|error| error.to_string())?;
    Ok(())
}

fn checked_metadata(path: &Path) -> Result<fs::Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x0400 != 0 {
            return Err(format!("插件包包含重解析点: {}", path.display()));
        }
    }
    if metadata.file_type().is_symlink() {
        return Err(format!("插件包包含链接: {}", path.display()));
    }
    Ok(metadata)
}

fn permission_names(manifest: &Manifest) -> Vec<&'static str> {
    let Some(permissions) = &manifest.permissions else {
        return Vec::new();
    };
    [
        (permissions.workspace_read, "workspace_read"),
        (permissions.workspace_write, "workspace_write"),
        (permissions.network, "network"),
        (permissions.process_spawn, "process_spawn"),
        (permissions.credential_use, "credential_use"),
    ]
    .into_iter()
    .filter_map(|(enabled, name)| enabled.then_some(name))
    .collect()
}

pub(super) fn approval_preview(args: &Value) -> String {
    let field = |key| args[key].as_str().unwrap_or("?");
    let permissions = args["permissions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let permissions = if permissions.is_empty() {
        "无".to_owned()
    } else {
        permissions.join(", ")
    };
    let runtime = field("runtime");
    let execution = if runtime == "external-process" {
        " · 以当前账号启动外部程序"
    } else {
        ""
    };
    format!(
        "{} {} · {runtime}{execution}\n声明权限 {}\n来源 {}",
        field("plugin"),
        field("version"),
        permissions,
        field("path"),
    )
}

#[cfg(test)]
#[path = "creator/tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "creator/live_test.rs"]
pub(super) mod live_test;
