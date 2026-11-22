use dshr_plugin_api::manifest::RuntimeKind;
use dshr_plugin_api::{HostVersion, Manifest, Plugin, PluginContext, PluginError};
use dshr_plugin_host::{ExternalPlugin, ExternalPluginSpec, PluginHost};
use dshr_profile::Profile;
use dshr_sandbox::Workspace;
use dshr_tool_runtime::{ToolDefinition, ToolOrigin};
use serde_json::json;
use std::path::Path;

/// 一个最小的 Rust 原生插件：注册 `probe_native` 工具 + 一个 Panel。
pub(crate) struct ProbeNativePlugin;

impl Plugin for ProbeNativePlugin {
    fn id(&self) -> &'static str {
        "builtin.probe"
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "probe_native",
                "把输入文本转成大写（Rust 原生插件，不经过 Node）",
                ToolOrigin::Native {
                    plugin_id: "builtin.probe".to_owned(),
                },
            )
            .with_schema(json!({
                "type": "object",
                "required": ["text"],
                "properties": { "text": { "type": "string" } }
            }))
            .with_presentation("generic"),
        )?;
        ctx.register_panel(
            dshr_plugin_api::PanelContribution::new(
                "probe.panel",
                "Probe Panel",
                "workspace.panel",
            )
            // 插件面板的 ADC 视图名（`render_view` 的入参；与注册 id 分开）。
            .with_view("probe.panel"),
        );
        Ok(())
    }

    /// ADC-v1 面板 view：证明"原生插件 → 宿主 → adc_panel_body"的完整产品链路。
    fn render_view(&mut self, view: &str) -> Result<serde_json::Value, PluginError> {
        if view != "probe.panel" {
            return Err(PluginError::Invalid(format!("unknown view `{view}`")));
        }
        Ok(json!({
            "type": "Card",
            "props": {"title": "Probe Panel"},
            "children": [
                {"type": "Text", "props": {"value": "原生插件 ADC 面板（probe_native）"}},
                {"type": "Row", "props": {"gap": 8, "align": "center"}, "children": [
                    {"type": "Badge", "props": {"label": "native", "tone": "success"}},
                    {"type": "Text", "props": {"value": "只读渲染 · 事件留 M4", "style": "caption"}}
                ]}
            ]
        }))
    }

    fn call(
        &mut self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        if tool != "probe_native" {
            return Err(PluginError::UnknownTool {
                plugin: "builtin.probe".to_owned(),
                tool: tool.to_owned(),
            });
        }
        let text = arguments
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        Ok(json!({ "native_echo": text.to_uppercase() }))
    }
}

/// 原生写入工具：走沙箱 seam（路径根 + 写前快照 + 1 MiB 限制）。
///
/// 工具展示类型是 `diff`，所以默认策略会要求审批；审批通过后才写文件。
pub(crate) struct WorkspaceWritePlugin {
    workspace: Workspace,
    tool: &'static str,
    id: &'static str,
}

impl WorkspaceWritePlugin {
    pub(crate) fn new(root: &Path, tool: &'static str, id: &'static str) -> Result<Self, String> {
        if !root.is_dir() {
            return Err("工作区目录不可用".to_owned());
        }
        Workspace::new(root)
            .map(|workspace| Self {
                workspace,
                tool,
                id,
            })
            .map_err(|error| error.to_string())
    }
}

impl Plugin for WorkspaceWritePlugin {
    fn id(&self) -> &'static str {
        self.id
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                self.tool,
                "写入工作区内的 UTF-8 文件。支持新建多层父目录，例如 hello-world/src/main.rs。路径相对工作区，覆盖前留快照，单次限 1 MiB。",
                ToolOrigin::Native {
                    plugin_id: self.id.to_owned(),
                },
            )
            .with_schema(json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                }
            }))
            .with_presentation("diff"),
        )?;
        Ok(())
    }

    fn call(
        &mut self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        if tool != self.tool {
            return Err(PluginError::UnknownTool {
                plugin: self.id.to_owned(),
                tool: tool.to_owned(),
            });
        }
        let path = arguments
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| PluginError::Invalid(String::from("missing path")))?;
        let content = arguments
            .get("content")
            .and_then(|value| value.as_str())
            .ok_or_else(|| PluginError::Invalid(String::from("missing content")))?;
        let receipt = self
            .workspace
            .write_text_creating_parents(path, content)
            .map_err(|err| PluginError::Invalid(err.to_string()))?;
        Ok(json!({
            "path": receipt.path.display().to_string(),
            "bytes": receipt.bytes,
            "snapshot": receipt.snapshot.map(|snapshot| snapshot.id),
        }))
    }
}

pub(crate) fn register_probe_write(plugins: &mut PluginHost, workspace: &Path) {
    let result = WorkspaceWritePlugin::new(workspace, "probe_write", "builtin.probe-write")
        .and_then(|plugin| {
            plugins
                .load(Box::new(plugin))
                .map_err(|error| error.to_string())
        });
    if let Err(error) = result {
        eprintln!("[chat] 工作区写入工具不可用: {error}");
    }
}

/// 宿主兼容基线：用 `dshr-plugin-api` 的 semver（0.1.0），不是应用 workspace 的 0.0.0。
pub(crate) fn host_sdk_version() -> HostVersion {
    HostVersion::parse(dshr_plugin_api::HOST_VERSION).unwrap_or_else(|_| HostVersion::new(0, 1, 0))
}

/// 从 profile 的 enabled 条目装载已安装插件（spec/cpp-v1.md §3.3）：
/// - `external-process` → [`ExternalPluginSpec`] + [`ExternalPlugin::spawn`]；
/// - `wasm-component` → [`PluginHost::load_wasm_plugin`]（P1-1：不再静默跳过）；
/// - 其它 runtime kind（如 `rust-static`）装错位置 → 记为 plugin error（不静默）。
///
/// 单个插件失败只记诊断，不影响其它插件与内置工具。
pub(crate) fn load_enabled_external_plugins(
    plugins: &mut PluginHost,
    profile: &Profile,
    sidecar_dir: &std::path::Path,
) -> Vec<String> {
    let mut errors = Vec::new();
    let node_modules = sidecar_dir.join("node_modules");
    for entry in profile.entries().iter().filter(|entry| entry.enabled) {
        let package_dir = node_modules.join(&entry.name);
        let manifest_path = package_dir.join("plugin.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = match Manifest::from_path(&manifest_path) {
            Ok(manifest) => manifest,
            Err(err) => {
                errors.push(format!("插件 {} 的 manifest 解析失败: {err}", entry.name));
                continue;
            }
        };
        if let Err(error) = load_cpp_package(plugins, manifest, &package_dir) {
            errors.push(error);
        }
    }
    errors
}

pub(crate) fn load_cpp_package(
    plugins: &mut PluginHost,
    manifest: Manifest,
    package_dir: &std::path::Path,
) -> Result<(), String> {
    let plugin_id = manifest.plugin.id.clone();
    let host = host_sdk_version();
    match manifest.runtime.kind {
        RuntimeKind::ExternalProcess => {
            let spec = ExternalPluginSpec::new(manifest, package_dir, host, 1);
            let plugin = ExternalPlugin::spawn(spec)
                .map_err(|error| format!("插件 {plugin_id} 启动失败: {error}"))?;
            plugins
                .load(Box::new(plugin))
                .map_err(|error| format!("插件 {plugin_id} 装载失败: {error}"))?;
            eprintln!("[plugin] external-process 已装载: {plugin_id}");
            Ok(())
        }
        RuntimeKind::WasmComponent => {
            let entry = manifest.runtime.entry.as_deref().unwrap_or("plugin.wasm");
            let is_component = std::fs::read(package_dir.join(entry))
                .map(|bytes| bytes.get(4..8) == Some(&[0x0d, 0x00, 0x01, 0x00]))
                .unwrap_or(false);
            let loaded = if is_component {
                plugins.load_cpp_component_plugin_from_manifest(&manifest, package_dir, &host)
            } else {
                plugins.load_wasm_plugin(&manifest, package_dir, &host)
            };
            loaded.map_err(|error| format!("插件 {plugin_id} WASM 装载失败: {error}"))?;
            eprintln!("[plugin] wasm-component 已装载: {plugin_id}");
            Ok(())
        }
        other => Err(format!(
            "插件 {plugin_id} 的 runtime.kind={other:?} 不支持从安装包装载（rust-static 需内置）"
        )),
    }
}
