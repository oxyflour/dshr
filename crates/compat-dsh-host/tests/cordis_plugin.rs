//! 真实 Cordis 插件：由 Cordis 运行时（DSH 内嵌的同一版本 4.0.5）装载，
//! 通过宿主提供的 `bridge` 服务注册工具，再经我们自己的 NDJSON 协议调用。
//!
//! 这条测试回答的是"compatibility, not captivity"的第一步：
//! 插件写的是 Cordis API，而不是本项目自定义的 probe 形状。

use std::path::PathBuf;
use std::time::Duration;

use dshr_compat_dsh_host::SidecarHost;
use serde_json::json;

fn cordis_paths() -> (PathBuf, PathBuf) {
    let root = std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .expect("workspace root");
    (
        root.join("sidecar/cordis/sidecar-cordis.mjs"),
        root.join("sidecar/cordis/bridge-plugin.mjs"),
    )
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[test]
fn cordis_plugin_registers_and_invokes_through_the_bridge() {
    if !node_available() {
        eprintln!("SKIP: node not found in PATH");
        return;
    }
    let (script, plugin) = cordis_paths();
    let mut host = SidecarHost::spawn(&script, &plugin).expect("cordis sidecar starts");

    println!(
        "[cordis] ready={:?} generation={}",
        host.ready_latency(),
        host.generation()
    );
    let names: Vec<_> = host
        .registrations()
        .iter()
        .map(|reg| reg.name.as_str())
        .collect();
    println!("[cordis] registrations={names:?}");

    assert_eq!(names, vec!["cordis_echo"], "Cordis 插件注册了一个工具");

    let outcome = host
        .invoke("cordis_echo", json!({ "n": 1 }), Duration::from_secs(5))
        .expect("invoke succeeds");

    assert_eq!(outcome.chunks, vec!["cordis-chunk-0", "cordis-chunk-1"]);
    assert_eq!(outcome.terminal.kind, "tool.result");
    assert_eq!(outcome.terminal.payload["is_error"], false);
    assert_eq!(
        outcome.terminal.payload["content"][0]["value"]["cordis"]["n"],
        1
    );
    println!("[cordis] invoke elapsed={:?}", outcome.elapsed);
}
