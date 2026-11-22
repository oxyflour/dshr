#![cfg(test)]

use super::*;
use dshr_tool_runtime::{AllowAll, ToolDefinition};
use serde_json::json;

struct ProbeNative {
    applied: bool,
    unloaded: bool,
    fail_apply: bool,
}

impl ProbeNative {
    fn new() -> Self {
        Self {
            applied: false,
            unloaded: false,
            fail_apply: false,
        }
    }

    fn failing() -> Self {
        Self {
            fail_apply: true,
            ..Self::new()
        }
    }
}

impl Plugin for ProbeNative {
    fn id(&self) -> &'static str {
        "builtin.probe"
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "probe_native",
                "echo a string from a Rust native plugin",
                ToolOrigin::Native {
                    plugin_id: "builtin.probe".to_owned(),
                },
            )
            .with_schema(json!({
                "type": "object",
                "required": ["text"],
                "properties": { "text": { "type": "string" } }
            })),
        )?;
        ctx.register_panel(
            PanelContribution::new("probe.panel", "Probe Panel", "workspace.panel")
                .with_view("probe.panel"),
        );

        if self.fail_apply {
            // 故意在注册之后失败：宿主必须回滚。
            return Err(PluginError::Invalid("boom".to_owned()));
        }
        self.applied = true;
        Ok(())
    }

    fn call(&mut self, tool: &str, arguments: &Value) -> Result<Value, PluginError> {
        if tool != "probe_native" {
            return Err(PluginError::UnknownTool {
                plugin: "builtin.probe".to_owned(),
                tool: tool.to_owned(),
            });
        }
        let text = arguments
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(json!({ "native_echo": text.to_uppercase() }))
    }

    fn unload(&mut self) {
        self.unloaded = true;
    }

    /// ADC panel：原生插件也能渲染 view（与 external-process 走同一 trait 方法）。
    fn render_view(&mut self, view: &str) -> Result<Value, PluginError> {
        if view != "probe.panel" {
            return Err(PluginError::Invalid(format!("unknown view `{view}`")));
        }
        Ok(json!({
            "type": "Card",
            "props": {"title": "Probe Panel"},
            "children": [
                {"type": "Text", "props": {"value": "原生插件 ADC 面板"}},
                {"type": "Badge", "props": {"label": "native", "tone": "success"}}
            ]
        }))
    }
}

/// 不实现 view 的插件（验证 trait 默认实现的降级错误）。
struct NoViewNative;

impl Plugin for NoViewNative {
    fn id(&self) -> &'static str {
        "builtin.no-view"
    }

    fn apply(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }

    fn call(&mut self, _tool: &str, _arguments: &Value) -> Result<Value, PluginError> {
        Err(PluginError::Invalid("no tools".to_owned()))
    }
}

#[test]
fn load_registers_tool_and_panel() {
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(ProbeNative::new())).unwrap();

    let tool = host
        .registry()
        .get("probe_native")
        .expect("tool registered");
    assert_eq!(tool.origin.kind(), "native");
    assert_eq!(host.panels().len(), 1);
    assert_eq!(host.panels()[0].panel.slot, "workspace.panel");
    assert!(host.is_loaded("builtin.probe"));
}

#[test]
fn m4_component_012_component_plugin_loads_through_host() {
    const RUN_WAT: &str = r#"
    (component
      (core module $m
        (func (export "run") (result i32)
          i32.const 42))
      (core instance $i (instantiate $m))
      (func (export "run") (result u32)
        (canon lift (core func $i "run")))
    )
    "#;
    let mut host = PluginHost::new(AllowAll);
    host.load_component_plugin("probe.component", "0.1.0", RUN_WAT)
        .expect("component plugin loads");
    assert!(host.is_loaded("probe.component"));
    assert!(host.registry().get("component.run").is_some());
    let value = host
        .call_approved("component.run", &json!({}))
        .expect("component call through host");
    assert_eq!(value["value"], 42);
}

#[test]
fn m4_component_013_component_probe_from_manifest() {
    const RUN_WAT: &str = r#"
    (component
      (core module $m
        (func (export "run") (result i32)
          i32.const 42))
      (core instance $i (instantiate $m))
      (func (export "run") (result u32)
        (canon lift (core func $i "run")))
    )
    "#;
    let dir = temp_wasm_package("component-probe");
    std::fs::write(dir.join("plugin.wasm"), RUN_WAT).expect("write component");
    let mut manifest = wasm_manifest();
    manifest.tools.clear();
    manifest.ui.clear();
    manifest.runtime.entry = Some("plugin.wasm".to_owned());
    let mut host = PluginHost::new(AllowAll);
    host.load_component_plugin_from_manifest(&manifest, &dir, &host_version())
        .expect("load component probe");
    let value = host
        .call_approved("component.run", &json!({}))
        .expect("component call");
    assert_eq!(value["value"], 42);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn m4_component_017_cpp_component_from_manifest() {
    const GUEST: &[u8] =
        include_bytes!("../../plugin-wasm/tests/fixtures/cpp-component-guest.wasm");
    let dir = temp_wasm_package("cpp-component");
    std::fs::write(dir.join("plugin.wasm"), GUEST).expect("write guest");
    let mut manifest = wasm_manifest();
    manifest.tools.clear();
    manifest.ui.clear();
    manifest.runtime.entry = Some("plugin.wasm".to_owned());
    let mut host = PluginHost::new(AllowAll);
    host.load_cpp_component_plugin_from_manifest(&manifest, &dir, &host_version())
        .expect("load cpp component");
    assert!(host.is_loaded("cpp.wasm.host"));
    assert!(host.registry().get("echo").is_some());
    let value = host
        .call_approved("echo", &json!({ "text": "hello" }))
        .expect("call echo");
    assert_eq!(value["content"][0]["text"], "{\"text\":\"hello\"}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unload_removes_tool_and_panel() {
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(ProbeNative::new())).unwrap();
    host.unload("builtin.probe").unwrap();

    assert!(host.registry().get("probe_native").is_none());
    assert!(host.panels().is_empty());
    assert!(!host.is_loaded("builtin.probe"));
    assert!(matches!(
        host.unload("builtin.probe").unwrap_err(),
        HostError::NotFound(_)
    ));
}

#[test]
fn duplicate_load_is_rejected() {
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(ProbeNative::new())).unwrap();
    let err = host.load(Box::new(ProbeNative::new())).unwrap_err();
    assert!(matches!(err, HostError::Duplicate(_)));
}

#[test]
fn failed_apply_leaves_no_registrations() {
    let mut host = PluginHost::new(AllowAll);
    let err = host.load(Box::new(ProbeNative::failing())).unwrap_err();
    assert!(matches!(err, HostError::Plugin(_)));
    assert!(host.registry().is_empty(), "apply 失败必须原子回滚");
    assert!(host.panels().is_empty());
    assert!(!host.is_loaded("builtin.probe"));
}

#[test]
fn call_goes_through_policy_and_dispatches_to_plugin() {
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(ProbeNative::new())).unwrap();

    let out = host.call("probe_native", &json!({ "text": "hi" })).unwrap();
    assert_eq!(out["native_echo"], "HI");

    // schema 拦截：缺 required
    assert!(matches!(
        host.call("probe_native", &json!({})).unwrap_err(),
        HostError::Registry(RegistryError::InvalidArguments { .. })
    ));
}

#[test]
fn sidecar_generation_invalidation_does_not_touch_native_tools() {
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(ProbeNative::new())).unwrap();
    host.registry
        .get("probe_native")
        .expect("native tool present");

    // 通过注册表直接模拟 sidecar 工具，再让它整代失效。
    let registry = &mut host.registry;
    registry
        .register(ToolDefinition::new(
            "js_tool",
            "sidecar tool",
            ToolOrigin::Sidecar {
                plugin_id: "probe.plugin".to_owned(),
                registration_id: "reg-1".to_owned(),
                generation: "node-0001".to_owned(),
            },
        ))
        .unwrap();
    assert_eq!(registry.invalidate_generation("node-0001"), 1);
    assert!(registry.get("probe_native").is_some());
    assert!(registry.get("js_tool").is_none());
}

fn manifest_json(id: &str, api: &str, min_host_version: Option<&str>) -> serde_json::Value {
    let mut plugin = json!({
        "id": id,
        "version": "0.1.0",
        "api": api,
    });
    if let Some(required) = min_host_version {
        plugin["min_host_version"] = json!(required);
    }
    json!({ "plugin": plugin, "runtime": { "kind": "rust-static" } })
}

fn host_version() -> HostVersion {
    HostVersion::new(0, 1, 0)
}

// CPP-FIBER-023..027：PluginHost 的 manifest 装载接入（CPP-A）。
#[test]
fn cpp_fiber_023_load_with_manifest_validates_then_loads() {
    let manifest = Manifest::from_json_str(
        &manifest_json("builtin.probe", "cordis-portable/v1", None).to_string(),
    )
    .unwrap();
    let mut host = PluginHost::new(AllowAll);
    host.load_with_manifest(&manifest, Box::new(ProbeNative::new()), &host_version())
        .unwrap();
    assert!(host.is_loaded("builtin.probe"));
    assert!(host.registry().get("probe_native").is_some());
    assert_eq!(host.panels().len(), 1);
}

#[test]
fn cpp_fiber_024_load_with_manifest_rejects_too_old_host() {
    let manifest = Manifest::from_json_str(
        &manifest_json("builtin.probe", "cordis-portable/v1", Some("9.9.9")).to_string(),
    )
    .unwrap();
    let mut host = PluginHost::new(AllowAll);
    let err = host
        .load_with_manifest(&manifest, Box::new(ProbeNative::new()), &host_version())
        .unwrap_err();
    let HostError::Plugin(error) = &err else {
        panic!("预期 PluginError，实际 {err:?}");
    };
    assert_eq!(error.code(), "E_VERSION");
    assert!(matches!(error, PluginError::HostTooOld { .. }));
    assert!(!host.is_loaded("builtin.probe"));
    assert!(host.registry().is_empty());
}

#[test]
fn cpp_fiber_025_load_with_manifest_rejects_api_mismatch() {
    // 直接 serde 解析以绕过 `from_json_str` 的 api 检查，验证装载期仍会拒绝。
    let raw = manifest_json("builtin.probe", "cordis-portable/v2", None).to_string();
    let manifest: Manifest = serde_json::from_str(&raw).unwrap();
    let mut host = PluginHost::new(AllowAll);
    let err = host
        .load_with_manifest(&manifest, Box::new(ProbeNative::new()), &host_version())
        .unwrap_err();
    let HostError::Plugin(error) = &err else {
        panic!("预期 PluginError，实际 {err:?}");
    };
    assert_eq!(error.code(), "E_VERSION");
    assert!(matches!(error, PluginError::UnsupportedApi { .. }));
    assert!(host.registry().is_empty());
}

#[test]
fn cpp_fiber_026_load_with_manifest_rejects_plugin_id_mismatch() {
    let manifest = Manifest::from_json_str(
        &manifest_json("other.plugin", "cordis-portable/v1", None).to_string(),
    )
    .unwrap();
    let mut host = PluginHost::new(AllowAll);
    let err = host
        .load_with_manifest(&manifest, Box::new(ProbeNative::new()), &host_version())
        .unwrap_err();
    let HostError::Plugin(error) = &err else {
        panic!("预期 PluginError，实际 {err:?}");
    };
    assert_eq!(error.code(), "E_MANIFEST");
    assert!(matches!(error, PluginError::InvalidManifest(_)));
    assert!(!host.is_loaded("builtin.probe"));
    assert!(host.registry().is_empty());
}

#[test]
fn cpp_fiber_027_load_with_manifest_leaves_registry_untouched_on_plugin_failure() {
    let manifest = Manifest::from_json_str(
        &manifest_json("builtin.probe", "cordis-portable/v1", None).to_string(),
    )
    .unwrap();
    let mut host = PluginHost::new(AllowAll);
    let err = host
        .load_with_manifest(&manifest, Box::new(ProbeNative::failing()), &host_version())
        .unwrap_err();
    assert!(matches!(err, HostError::Plugin(_)));
    assert!(host.registry().is_empty());
    assert!(host.panels().is_empty());
    assert!(!host.is_loaded("builtin.probe"));
}

// ---- ADC-v1 panel 渲染入口（ADC-ELEM-PANEL-HOST-*） --------------------

// ADC-ELEM-PANEL-HOST-001
#[test]
fn render_panel_returns_adc_tree_from_native_plugin() {
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(ProbeNative::new())).unwrap();

    // panel 元数据带 view：render_panel 的入参（id 与 view 可以不同）。
    let registration = host
        .panels()
        .into_iter()
        .find(|registration| registration.panel.slot == "workspace.panel")
        .expect("panel registered");
    assert_eq!(registration.panel.view_name(), "probe.panel");

    let tree = host
        .render_panel("builtin.probe", "probe.panel")
        .expect("render_panel");
    assert_eq!(tree["type"], json!("Card"));
    assert_eq!(tree["props"]["title"], json!("Probe Panel"));

    // 未知 view → 插件的可读错误（不是 panic / NotFound）。
    let error = host
        .render_panel("builtin.probe", "missing.view")
        .unwrap_err();
    assert!(matches!(&error, HostError::Plugin(PluginError::Invalid(_))));
    assert!(error.to_string().contains("missing.view"), "{error}");
}

// ADC-ELEM-PANEL-HOST-002
#[test]
fn render_panel_unknown_plugin_is_not_found() {
    let mut host = PluginHost::new(AllowAll);
    let error = host
        .render_panel("missing.plugin", "workspace.panel")
        .unwrap_err();
    assert!(matches!(&error, HostError::NotFound(_)));
    assert!(error.to_string().contains("missing.plugin"), "{error}");
}

// M4-ADC-002（M4 W3）：ui.action 路由到指定插件，未知插件报 NotFound。
#[derive(Clone)]
struct ActionProbe {
    seen: std::sync::Arc<std::sync::Mutex<Vec<(String, String, Value)>>>,
}

impl Plugin for ActionProbe {
    fn id(&self) -> &'static str {
        "builtin.action-probe"
    }

    fn apply(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }

    fn call(&mut self, _tool: &str, _arguments: &Value) -> Result<Value, PluginError> {
        Err(PluginError::Invalid("no tools".to_owned()))
    }

    fn handle_action(
        &mut self,
        view: &str,
        action: &str,
        payload: &Value,
    ) -> Result<(), PluginError> {
        self.seen
            .lock()
            .unwrap()
            .push((view.to_owned(), action.to_owned(), payload.clone()));
        Ok(())
    }
}

#[test]
fn dispatch_action_routes_payload_and_unknown_plugin_is_not_found() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(ActionProbe {
        seen: std::sync::Arc::clone(&seen),
    }))
    .unwrap();

    host.dispatch_action(
        "builtin.action-probe",
        "fixture.panel",
        "call",
        &json!({ "method": "refresh", "args": { "n": 1 } }),
    )
    .unwrap();
    let recorded = seen.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, "fixture.panel");
    assert_eq!(recorded[0].1, "call");
    assert_eq!(recorded[0].2["method"], "refresh");

    let error = host
        .dispatch_action("missing.plugin", "v", "call", &Value::Null)
        .unwrap_err();
    assert!(matches!(&error, HostError::NotFound(_)), "{error:?}");
}

// ADC-ELEM-PANEL-HOST-003
#[test]
fn render_panel_default_impl_is_readable_for_plugins_without_views() {
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(NoViewNative)).unwrap();
    let error = host
        .render_panel("builtin.no-view", "anything")
        .unwrap_err();
    assert!(
        error.to_string().contains("does not render views"),
        "{error}"
    );
    assert!(matches!(&error, HostError::Plugin(PluginError::Invalid(_))));
}

// ---- review P2-3：panel id 全局冲突 -----------------------------------

/// 与 `ProbeNative` 用**同一个 panel id** 的第二个插件：原名应被拒绝而不是覆盖。
struct ProbePanelConflict {
    plugin_id: &'static str,
    repeat_panel: bool,
}

impl ProbePanelConflict {
    fn new(plugin_id: &'static str) -> Self {
        Self {
            plugin_id,
            repeat_panel: false,
        }
    }

    fn repeating(plugin_id: &'static str) -> Self {
        Self {
            plugin_id,
            repeat_panel: true,
        }
    }
}

impl Plugin for ProbePanelConflict {
    fn id(&self) -> &'static str {
        self.plugin_id
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(ToolDefinition::new(
            format!("{}_tool", self.plugin_id.replace('.', "_")),
            "conflict probe tool",
            ToolOrigin::Native {
                plugin_id: self.plugin_id.to_owned(),
            },
        ))?;
        ctx.register_panel(PanelContribution::new(
            "probe.panel",
            "Probe Panel",
            "workspace.panel",
        ));
        if self.repeat_panel {
            // 同一插件内重复声明同一 panel id 也必须拒绝。
            ctx.register_panel(PanelContribution::new(
                "probe.panel",
                "Probe Panel Duplicated",
                "workspace.panel",
            ));
        }
        Ok(())
    }

    fn call(&mut self, _tool: &str, _arguments: &Value) -> Result<Value, PluginError> {
        Ok(json!({ "ok": true }))
    }
}

// CPP-WASM-033 / review P2-3：跨插件 panel id 冲突 → 拒绝且原子回滚。
#[test]
fn cpp_wasm_033_cross_plugin_panel_id_conflict_is_rejected_atomically() {
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(ProbeNative::new())).expect("first load");
    let error = host
        .load(Box::new(ProbePanelConflict::new("builtin.conflict")))
        .expect_err("panel id 冲突必须拒绝");
    assert!(
        matches!(&error, HostError::PanelConflict { panel_id, owner } if panel_id == "probe.panel" && owner == "builtin.probe"),
        "error={error:?}"
    );
    // 原子性：第二个插件的工具一条都不能落库，第一个插件的 panel 原样保留。
    assert!(host.registry().get("builtin_conflict_tool").is_none());
    assert!(!host.is_loaded("builtin.conflict"));
    assert_eq!(host.panels().len(), 1);
    assert_eq!(host.panels()[0].plugin_id, "builtin.probe");
}

// review P2-3：同一插件内重复声明 panel id 也拒绝（不能静默丢一条）。
#[test]
fn cpp_panel_002_same_plugin_duplicate_panel_id_is_rejected() {
    let mut host = PluginHost::new(AllowAll);
    let error = host
        .load(Box::new(ProbePanelConflict::repeating("builtin.repeat")))
        .expect_err("同一插件重复 panel id 必须拒绝");
    assert!(
        matches!(&error, HostError::PanelConflict { panel_id, owner } if panel_id == "probe.panel" && owner == "builtin.repeat"),
        "error={error:?}"
    );
    assert!(host.panels().is_empty());
    assert!(!host.is_loaded("builtin.repeat"));
}

// ---- review P1-1：WASM 接入 PluginHost --------------------------------

/// 最简 echo WAT（回显请求 JSON；`WasmPlugin` 测试用）。
const WASM_ECHO_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 4096))
  (func (export "dshr_alloc") (param $len i32) (result i32)
(local $ptr i32)
(local.set $ptr (global.get $heap))
(global.set $heap (i32.add (global.get $heap) (local.get $len)))
(local.get $ptr))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
(i64.or
  (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
  (i64.extend_i32_u (local.get $len))))
)
"#;

fn write_wasm_package(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("schemas")).expect("schemas dir");
    std::fs::write(dir.join("plugin.wasm"), WASM_ECHO_WAT).expect("write wat");
    for suffix in ["input", "output"] {
        std::fs::write(
            dir.join(format!("schemas/echo.{suffix}.json")),
            json!({ "type": "object" }).to_string(),
        )
        .unwrap();
    }
}

fn wasm_manifest() -> Manifest {
    let raw = json!({
        "plugin": { "id": "cpp.wasm.host", "version": "0.1.0", "api": "cordis-portable/v1" },
        "runtime": {
            "kind": "wasm-component",
            "entry": "plugin.wasm",
            "fuel": 2_000_000,
            "max_memory_mb": 32,
            "timeout_ms": 30_000
        },
        "tools": [{
            "name": "echo",
            "description": "echo",
            "input_schema": "schemas/echo.input.json",
            "output_schema": "schemas/echo.output.json"
        }],
        "ui": [{
            "slot": "workspace.panel",
            "id": "cpp.wasm.host.panel",
            "label": "WASM Host Panel",
            "view": "cpp.wasm.host.view"
        }]
    })
    .to_string();
    Manifest::from_json_str(&raw).expect("wasm manifest")
}

fn temp_wasm_package(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "dshr-host-wasm-{tag}-{}-{nanos}",
        std::process::id()
    ));
    write_wasm_package(&dir);
    dir
}

// CPP-WASM-031：load_wasm_plugin 装载 → 工具注册 → 调用 → 卸载。
#[test]
fn cpp_wasm_031_load_wasm_plugin_registers_tool_and_calls() {
    let dir = temp_wasm_package("ok");
    let manifest = wasm_manifest();
    let mut host = PluginHost::new(AllowAll);
    host.load_wasm_plugin(&manifest, &dir, &host_version())
        .expect("load wasm plugin");
    assert!(host.is_loaded("cpp.wasm.host"));
    assert!(host.registry().get("echo").is_some());
    assert_eq!(host.panels().len(), 1);
    assert_eq!(host.panels()[0].panel.id, "cpp.wasm.host.panel");
    assert_eq!(host.panels()[0].panel.view_name(), "cpp.wasm.host.view");

    let value = host
        .call("echo", &json!({ "text": "wasm-host" }))
        .expect("call wasm tool");
    assert_eq!(value["tool"], json!("echo"));
    assert_eq!(value["arguments"]["text"], json!("wasm-host"));

    host.unload("cpp.wasm.host").expect("unload");
    assert!(!host.is_loaded("cpp.wasm.host"));
    assert!(host.registry().is_empty());
    assert!(host.panels().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// CPP-WASM-032：wasm-component 走 native `load_with_manifest` 必须报错（不能静默）。
#[test]
fn cpp_wasm_032_load_with_manifest_rejects_wasm_component() {
    let manifest = wasm_manifest();
    let mut host = PluginHost::new(AllowAll);
    let error = host
        .load_with_manifest(&manifest, Box::new(ProbeNative::new()), &host_version())
        .expect_err("wasm-component 不能被 native 路径静默装载");
    assert!(
        matches!(&error, HostError::Wasm { plugin_id, reason } if plugin_id == "cpp.wasm.host" && reason.contains("load_wasm_plugin")),
        "error={error:?}"
    );
    assert!(error.to_string().contains("load_wasm_plugin"), "{error}");
    assert!(!host.is_loaded("cpp.wasm.host"));
}

// CPP-WASM-034：wasm 装载失败（entry 缺失）→ 结构化错误且宿主无残留。
#[test]
fn cpp_wasm_034_load_wasm_plugin_missing_entry_is_structured_error() {
    let dir = temp_wasm_package("missing");
    let mut manifest = wasm_manifest();
    manifest.runtime.entry = Some("missing.wasm".to_owned());
    let mut host = PluginHost::new(AllowAll);
    let error = host
        .load_wasm_plugin(&manifest, &dir, &host_version())
        .expect_err("entry 缺失必须失败");
    assert!(matches!(&error, HostError::Wasm { .. }), "error={error:?}");
    assert!(
        error.to_string().contains("manifest error")
            || error.to_string().contains("not accessible"),
        "{error}"
    );
    assert!(!host.is_loaded("cpp.wasm.host"));
    assert!(host.registry().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// M4-CONC-001：不同 native 插件的批调用并行；同一插件仍串行（由分组保证）。
#[test]
fn m4_conc_001_native_batch_parallelizes_distinct_plugins() {
    struct SlowPlugin {
        id: &'static str,
        tool: &'static str,
    }
    impl Plugin for SlowPlugin {
        fn id(&self) -> &str {
            self.id
        }
        fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
            ctx.register_tool(ToolDefinition::new(
                self.tool,
                "slow probe",
                ToolOrigin::Native {
                    plugin_id: self.id.to_owned(),
                },
            ))?;
            Ok(())
        }
        fn call(&mut self, _tool: &str, arguments: &Value) -> Result<Value, PluginError> {
            std::thread::sleep(std::time::Duration::from_millis(300));
            Ok(arguments.clone())
        }
    }

    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(SlowPlugin {
        id: "slow.a",
        tool: "slow_a",
    }))
    .expect("load slow.a");
    host.load(Box::new(SlowPlugin {
        id: "slow.b",
        tool: "slow_b",
    }))
    .expect("load slow.b");

    let started = std::time::Instant::now();
    let results = host.call_approved_batch(
        &[
            ("slow_a".to_owned(), json!({ "n": 1 })),
            ("slow_b".to_owned(), json!({ "n": 2 })),
        ],
        2,
    );
    let elapsed = started.elapsed();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    assert!(
        elapsed < std::time::Duration::from_millis(550),
        "distinct plugins should overlap (serial is ~600ms): {elapsed:?}"
    );
}

// M4-CONC-002：同一插件的两个调用必须串行（不能被跨插件并行误伤）。
#[test]
fn m4_conc_002_same_plugin_batch_is_serial() {
    struct SlowPair;
    impl Plugin for SlowPair {
        fn id(&self) -> &str {
            "slow.pair"
        }
        fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
            for tool in ["pair_a", "pair_b"] {
                ctx.register_tool(ToolDefinition::new(
                    tool,
                    "slow pair",
                    ToolOrigin::Native {
                        plugin_id: "slow.pair".to_owned(),
                    },
                ))?;
            }
            Ok(())
        }
        fn call(&mut self, _tool: &str, arguments: &Value) -> Result<Value, PluginError> {
            std::thread::sleep(std::time::Duration::from_millis(300));
            Ok(arguments.clone())
        }
    }

    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(SlowPair)).expect("load slow.pair");
    let started = std::time::Instant::now();
    let results = host.call_approved_batch(
        &[
            ("pair_a".to_owned(), json!({ "n": 1 })),
            ("pair_b".to_owned(), json!({ "n": 2 })),
        ],
        2,
    );
    let elapsed = started.elapsed();
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    assert!(
        elapsed >= std::time::Duration::from_millis(500),
        "same-plugin calls must not overlap: {elapsed:?}"
    );
}

// M4-CONC-003：批调用尊重预先置位的取消位，不启动工具。
#[test]
fn m4_conc_003_batch_respects_cancellation() {
    use std::sync::atomic::AtomicBool;
    let mut host = PluginHost::new(AllowAll);
    let cancel = AtomicBool::new(true);
    let results = host.call_approved_batch_cancellable(
        &[
            ("probe_native".to_owned(), json!({ "text": "x" })),
            ("probe_native".to_owned(), json!({ "text": "y" })),
        ],
        2,
        Some(&cancel),
    );
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|result| matches!(result, Err(message) if message == "cancelled")),
        "{results:?}"
    );
}
