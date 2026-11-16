//! M4 W5：实验性 ComponentPlugin 的加载/apply/call 闭环（probe ABI）。

use dshr_plugin_api::{Plugin, PluginContext};
use dshr_plugin_wasm::ComponentPlugin;
use serde_json::json;

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

/// M4-COMPONENT-008：probe 输出超过 max_output_bytes 时 call 失败（输出配额）。
#[test]
fn m4_component_008_component_output_limit() {
    let mut plugin = ComponentPlugin::new("probe.component", "0.1.0", RUN_WAT)
        .expect("component plugin constructs")
        .with_max_output_bytes(1);
    let error = plugin
        .call("component.run", &json!({}))
        .expect_err("tiny output limit must reject result");
    let message = error.to_string();
    assert!(
        message.contains("exceeds limit"),
        "unexpected error: {message}"
    );
}
const LOOP_WAT: &str = r#"
(component
  (core module $m
    (func (export "run") (result i32)
      (loop $l (br $l))
      i32.const 0))
  (core instance $i (instantiate $m))
  (func (export "run") (result u32)
    (canon lift (core func $i "run")))
)
"#;

/// M4-COMPONENT-009：ComponentPlugin 的 fuel 上限让死循环 call 失败。
#[test]
fn m4_component_009_component_plugin_fuel_limit() {
    let mut plugin = ComponentPlugin::new("probe.loop", "0.1.0", LOOP_WAT)
        .expect("component plugin constructs")
        .with_fuel_limit(1_000);
    let error = plugin
        .call("component.run", &json!({}))
        .expect_err("infinite loop must exhaust fuel");
    let message = error.to_string();
    assert!(
        message.contains("component trap") || message.contains("fuel"),
        "unexpected error: {message}"
    );
}

/// M4-COMPONENT-007：ComponentPlugin 注册 `component.run`，call 返回 42。
#[test]
fn m4_component_007_component_plugin_round_trip() {
    let mut plugin = ComponentPlugin::new("probe.component", "0.1.0", RUN_WAT)
        .expect("component plugin constructs");
    assert_eq!(plugin.id(), "probe.component");
    assert_eq!(plugin.version(), "0.1.0");

    let mut ctx = PluginContext::new("probe.component");
    plugin.apply(&mut ctx).expect("apply registers tool");
    let (tools, _panels) = ctx.take_registrations();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "component.run");

    let result = plugin
        .call("component.run", &json!({}))
        .expect("component call");
    assert_eq!(result["value"], 42);
}
