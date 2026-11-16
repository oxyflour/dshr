//! `WasmPlugin`：把 [`crate::WasmPluginRuntime`] 适配成 `dshr_plugin_api::Plugin`。
//!
//! 这是 P1-1（review-m3-implementation）的修复点：WASM 不再"只有引擎"，而是能像
//! native / external-process 一样被 `PluginHost` 装载、调用、卸载：
//!
//! - `id` / `version` 来自 manifest（`Plugin::id` 是 `&'static str`，字符串一次性
//!   leak，每个插件 O(1)，与 `external.rs` 同策略）；
//! - `apply` 按 manifest 的 `[[tools]]` 注册 `ToolDefinition`（`input_schema` /
//!   `output_schema` 从包内相对路径读入内联 JSON），按 `[[ui]]` 注册 panel；
//! - `call` 走 `WasmPluginRuntime::invoke(tool, args)`（core-module ABI，见 crate 文档）；
//! - `render_view` 走 ABI 的 `ui.render` 调用（guest 不支持时返回可读错误）；
//! - `unload` 丢弃 runtime（epoch 线程随 Drop 停止）。
//!
//! 入口：`WasmPlugin::from_manifest(&manifest, package_dir, &host_version)`。
//! `plugin-host` 的 `PluginHost::load_wasm_plugin` 就是它的薄包装。

use std::fs;
use std::path::{Component, Path, PathBuf};

use dshr_plugin_api::manifest::{Manifest, RuntimeKind};
use dshr_plugin_api::{HostVersion, PanelContribution, Plugin, PluginContext, PluginError};
use dshr_tool_runtime::{ToolDefinition, ToolOrigin};
use serde_json::{json, Value};

use crate::{WasmEngineConfig, WasmError, WasmPluginRuntime};

/// 一个可从 manifest + 包目录装载的 WASM 插件。
pub struct WasmPlugin {
    manifest: Manifest,
    package_dir: PathBuf,
    runtime: Option<WasmPluginRuntime>,
}

impl std::fmt::Debug for WasmPlugin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmPlugin")
            .field("plugin_id", &self.manifest.plugin.id)
            .field("version", &self.manifest.plugin.version)
            .field("loaded", &self.runtime.is_some())
            .field("tools", &self.manifest.tools.len())
            .field("panels", &self.manifest.ui.len())
            .finish()
    }
}

impl WasmPlugin {
    /// 从 typed manifest + 安装后的包目录创建插件（不执行；`apply` 时才注册声明）。
    pub fn from_manifest(
        manifest: &Manifest,
        package_dir: impl Into<PathBuf>,
        host: &HostVersion,
    ) -> Result<Self, WasmError> {
        // 与 PluginHost::load_with_manifest 同一套装载期校验（api/min_host/配额/…）。
        manifest.validate(host)?;
        if manifest.runtime.kind != RuntimeKind::WasmComponent {
            return Err(WasmError::Manifest(PluginError::InvalidManifest(format!(
                "runtime.kind is `{:?}`, not `wasm-component`",
                manifest.runtime.kind
            ))));
        }
        let package_dir = package_dir.into();
        let entry = manifest.runtime.entry.as_deref().unwrap_or_default();
        let entry_path = resolve_entry(&package_dir, entry)?;
        let source = fs::read(&entry_path).map_err(|error| {
            WasmError::InvalidModule(format!(
                "cannot read wasm entry `{}`: {error}",
                entry_path.display()
            ))
        })?;
        let config = WasmEngineConfig::from_manifest(manifest)?;
        let runtime = WasmPluginRuntime::from_bytes(source, config)?
            .with_plugin_id(manifest.plugin.id.clone());
        Ok(Self {
            manifest: manifest.clone(),
            package_dir,
            runtime: Some(runtime),
        })
    }

    /// 当前是否持有可调用的 runtime（`unload` 后为 false）。
    pub fn is_loaded(&self) -> bool {
        self.runtime.is_some()
    }

    /// 累计 trap 次数（诊断；宿主存活证据）。
    pub fn trap_count(&self) -> u32 {
        self.runtime
            .as_ref()
            .map(WasmPluginRuntime::trap_count)
            .unwrap_or(0)
    }

    fn runtime_mut(&mut self) -> Result<&mut WasmPluginRuntime, PluginError> {
        self.runtime.as_mut().ok_or_else(|| {
            PluginError::Invalid(format!(
                "wasm plugin `{}` is unloaded",
                self.manifest.plugin.id
            ))
        })
    }

    fn read_schema(&self, relative: &str) -> Result<Value, PluginError> {
        let path = resolve_package_file(&self.package_dir, relative)?;
        let raw = fs::read_to_string(&path).map_err(|error| PluginError::Manifest {
            path: path.display().to_string(),
            reason: format!("cannot read schema: {error}"),
        })?;
        serde_json::from_str(&raw).map_err(|error| PluginError::Manifest {
            path: path.display().to_string(),
            reason: format!("invalid schema JSON: {error}"),
        })
    }
}

impl Plugin for WasmPlugin {
    fn id(&self) -> &str {
        &self.manifest.plugin.id
    }

    fn version(&self) -> &str {
        &self.manifest.plugin.version
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        // 运行时必须仍然存在：apply 失败时宿主回滚全部声明。
        if self.runtime.is_none() {
            return Err(PluginError::Invalid(format!(
                "wasm plugin `{}` is unloaded",
                self.manifest.plugin.id
            )));
        }
        for tool in &self.manifest.tools {
            let input_schema = self.read_schema(&tool.input_schema)?;
            let output_schema = self.read_schema(&tool.output_schema)?;
            let mut definition = ToolDefinition::new(
                tool.name.clone(),
                tool.description.clone(),
                ToolOrigin::Native {
                    plugin_id: self.manifest.plugin.id.clone(),
                },
            )
            .with_schema(input_schema)
            .with_output_schema(output_schema)
            .with_timeout_ms(tool.timeout_ms.unwrap_or(120_000));
            if let Some(presentation) = &tool.presentation {
                definition = definition.with_presentation(presentation.clone());
            }
            ctx.register_tool(definition)?;
        }
        for entry in &self.manifest.ui {
            ctx.register_panel(
                PanelContribution::new(entry.id.clone(), entry.label.clone(), entry.slot.clone())
                    .with_view(entry.view.clone()),
            );
        }
        Ok(())
    }

    fn call(&mut self, tool: &str, arguments: &Value) -> Result<Value, PluginError> {
        if !self.manifest.tools.iter().any(|decl| decl.name == tool) {
            return Err(PluginError::UnknownTool {
                plugin: self.manifest.plugin.id.clone(),
                tool: tool.to_owned(),
            });
        }
        let runtime = self.runtime_mut()?;
        runtime
            .invoke(tool, arguments)
            .map_err(|error| map_wasm_error(error, tool))
    }

    /// ADC-v1 view：走 core ABI 的 `ui.render`（`{"tool":"ui.render","arguments":{...}}`）。
    fn render_view(&mut self, view: &str) -> Result<Value, PluginError> {
        let runtime = self.runtime_mut()?;
        runtime
            .invoke("ui.render", &json!({ "view": view, "state_seq": 0 }))
            .map_err(|error| map_wasm_error(error, "ui.render"))
    }

    fn unload(&mut self) {
        // Drop 会通知 epoch 线程退出；下一次 unload 幂等（runtime 已是 None）。
        self.runtime.take();
    }
}

fn map_wasm_error(error: WasmError, tool: &str) -> PluginError {
    match error {
        // manifest 错误保持原始类型（宿主装载期已经校验过，这里是兜底）。
        WasmError::Manifest(plugin_error) => plugin_error,
        other => {
            // 多数 WasmError 的 Display 已带 `E_*` 前缀（如 `E_QUOTA: wasm fuel exhausted`）；
            // 没有前缀的（ABI/Guest/Trap）补上 code，避免出现 `E_QUOTA: E_QUOTA:`。
            let message = other.to_string();
            let reason = if message.contains("E_") {
                message
            } else {
                format!("{}: {message}", other.code())
            };
            PluginError::ToolFailed {
                tool: tool.to_owned(),
                reason,
            }
        }
    }
}

/// `runtime.entry`：包内相对路径，拒绝绝对路径/`..`/符号链接（spec/cpp-v1.md §12.6）。
fn resolve_entry(package_dir: &Path, entry: &str) -> Result<PathBuf, WasmError> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Err(WasmError::Manifest(PluginError::InvalidManifest(
            "runtime.entry must not be empty for `wasm-component`".to_owned(),
        )));
    }
    resolve_package_file(package_dir, entry).map_err(WasmError::Manifest)
}

fn resolve_package_file(package_dir: &Path, relative: &str) -> Result<PathBuf, PluginError> {
    let relative = relative.trim();
    if relative.is_empty() {
        return Err(PluginError::InvalidManifest(
            "package-relative path must not be empty".to_owned(),
        ));
    }
    if relative.contains('\\') {
        return Err(PluginError::InvalidManifest(format!(
            "`{relative}` must use `/` separators (no backslashes)"
        )));
    }
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(PluginError::InvalidManifest(format!(
            "`{relative}` must be relative to the package directory"
        )));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PluginError::InvalidManifest(format!(
                    "`{relative}` must stay inside the package directory"
                )))
            }
        }
    }
    let joined = package_dir.join(path);
    let metadata = fs::symlink_metadata(&joined).map_err(|error| PluginError::Manifest {
        path: joined.display().to_string(),
        reason: format!("not accessible: {error}"),
    })?;
    if metadata.file_type().is_symlink() {
        return Err(PluginError::InvalidManifest(format!(
            "`{relative}` must not be a symlink"
        )));
    }
    if !metadata.is_file() {
        return Err(PluginError::InvalidManifest(format!(
            "`{relative}` is not a regular file"
        )));
    }
    Ok(joined)
}
