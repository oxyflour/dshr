//! `external-process` 运行时 + 隔离 E2E（spec/cpp-v1.md §12、ADR-0010）。
//!
//! 测试 ID：`CPP-ISO-*`。fixture 是同一 package 里的 `plugin-fixture` 二进制，
//! 通过 `env!("CARGO_BIN_EXE_plugin-fixture")` 定位；所有包/安装目录都在系统临时目录，
//! 用完即删（不写 docs/仓库）。
//!
//! 覆盖的 G3 退路证据：
//! - `CPP-ISO-001..004`：正常生命周期（spawn → ready → 注册 → call → unload，进程退出）；
//! - `CPP-ISO-005..008`：超时 / 超大输出 / 内存超限 / 崩溃四类隔离，宿主存活；
//! - `CPP-ISO-010..013`：entry 路径安全 / 环境清洗 / ui.render / 多插件互不影响；
//! - `CPP-ISO-018/019`：P1-3 读写边界（巨大无换行单行 → E_TOO_LARGE；子进程不读
//!   stdin → 写 watchdog E_TIMEOUT），两者都要求 Job 被杀、宿主存活；
//! - `CPP-ISO-026`：故障实例之后宿主仍能装载/调用正常插件（"宿主存活"产品级证据）；
//! - `CPP-ISO-032/033`：stderr 有界（16 MiB 无换行巨行 → tail ≤64 KiB + E_TIMEOUT + 杀 Job；
//!   5000 条短行 → tail 只保留最近 64 行/≤64 KiB）；
//! - `CPP-ISO-020..021`：G2 端到端（安装 → 装载 → 调用 → 卸载）与信任确认。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dshr_plugin_api::manifest::Manifest;
use dshr_plugin_api::{HostVersion, Plugin, PluginError};
use dshr_plugin_host::external::{
    MAX_STDERR_LINE_BYTES, STDERR_TAIL_BYTES_LIMIT, STDERR_TAIL_LINE_LIMIT,
};
use dshr_plugin_host::SlotTarget;
use dshr_plugin_host::{ExternalPlugin, ExternalPluginSpec, HostError, PluginHost};
use dshr_profile::{
    install_package_trusted, package_digest, Profile, SignatureStatus, TrustPolicy,
};
use dshr_tool_runtime::AllowAll;
use serde_json::json;

/// fixture 可执行文件（Cargo 为集成测试注入的绝对路径）。
const FIXTURE_EXE: &str = env!("CARGO_BIN_EXE_plugin-fixture");
/// 故障注入 fixture（P1-3：巨大单行 / 不读 stdin）。
const FAULT_FIXTURE_EXE: &str = env!("CARGO_BIN_EXE_plugin-fault-fixture");

// ---------------------------------------------------------------------------
// 测试脚手架
// ---------------------------------------------------------------------------

struct TempDir {
    path: PathBuf,
    #[cfg(target_os = "linux")]
    _fixture_guard: std::sync::MutexGuard<'static, ()>,
}

impl TempDir {
    fn new(name: &str) -> Self {
        // A concurrent fork can inherit the writable descriptor of a fixture another
        // test is copying, causing its exec to fail with ETXTBSY even after the copy returns.
        #[cfg(target_os = "linux")]
        let fixture_guard = {
            static FIXTURES: std::sync::Mutex<()> = std::sync::Mutex::new(());
            FIXTURES
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        };
        let path = std::env::temp_dir().join(format!(
            "dshr-cpp-iso-{name}-{}-{}",
            std::process::id(),
            nanos()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self {
            path,
            #[cfg(target_os = "linux")]
            _fixture_guard: fixture_guard,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn host_version() -> HostVersion {
    HostVersion::new(0, 0, 0)
}

fn fixture_entry_name() -> &'static str {
    if cfg!(windows) {
        "plugin-fixture.exe"
    } else {
        "plugin-fixture"
    }
}

/// 构造与 `plugin.toml` 等价的 typed manifest（离线无 toml crate，测试用 JSON 表达）。
fn manifest_for(
    plugin_id: &str,
    entry: &str,
    memory_mb: Option<u64>,
    min_host_version: Option<&str>,
) -> Manifest {
    let mut runtime = json!({ "kind": "external-process", "entry": entry });
    if let Some(memory_mb) = memory_mb {
        runtime["max_memory_mb"] = json!(memory_mb);
    }
    let mut plugin = json!({
        "id": plugin_id,
        "version": "0.1.0",
        "api": "cordis-portable/v1"
    });
    if let Some(min_host_version) = min_host_version {
        plugin["min_host_version"] = json!(min_host_version);
    }
    let raw = json!({ "plugin": plugin, "runtime": runtime }).to_string();
    Manifest::from_json_str(&raw).expect("fixture manifest parses")
}

/// 准备一个可执行包目录（fixture exe；可选 package.json/plugin.toml）。
fn prepare_package(root: &Path, dir_name: &str, package_name: Option<&str>) -> PathBuf {
    let package = root.join(dir_name);
    fs::create_dir_all(&package).expect("create package dir");
    fs::copy(FIXTURE_EXE, package.join(fixture_entry_name())).expect("copy fixture exe");
    if let Some(name) = package_name {
        fs::write(
            package.join("package.json"),
            json!({ "name": name, "version": "0.1.0" }).to_string(),
        )
        .expect("write package.json");
        fs::write(
            package.join("plugin.toml"),
            format!(
                "[plugin]\nid = \"{name}\"\nversion = \"0.1.0\"\napi = \"cordis-portable/v1\"\n\n[runtime]\nkind = \"external-process\"\nentry = \"{}\"\n",
                fixture_entry_name()
            ),
        )
        .expect("write plugin.toml");
    }
    package
}

/// spawn 一个 fixture 插件（默认包目录在 `root/<plugin_id>`）。
fn spawn_fixture(root: &Path, plugin_id: &str, memory_mb: Option<u64>) -> ExternalPlugin {
    let package = prepare_package(root, plugin_id, None);
    let manifest = manifest_for(plugin_id, fixture_entry_name(), memory_mb, None);
    ExternalPlugin::spawn(ExternalPluginSpec::new(
        manifest,
        package,
        host_version(),
        1,
    ))
    .expect("fixture plugin spawns")
}

/// 复制故障 fixture 为 `fault-<mode>[.exe]`（行为按文件名选择），并启动。
fn spawn_fault(root: &Path, mode: &str, plugin_id: &str) -> ExternalPlugin {
    let package = root.join(format!("fault-{mode}"));
    fs::create_dir_all(&package).expect("create fault package dir");
    let entry = if cfg!(windows) {
        format!("fault-{mode}.exe")
    } else {
        format!("fault-{mode}")
    };
    fs::copy(FAULT_FIXTURE_EXE, package.join(&entry)).expect("copy fault fixture exe");
    let manifest = manifest_for(plugin_id, &entry, None, None);
    ExternalPlugin::spawn(ExternalPluginSpec::new(
        manifest,
        package,
        host_version(),
        1,
    ))
    .expect("fault fixture spawns")
}

/// 轮询 stderr tail 直到满足 `predicate` 或超时，返回最后一次快照。
fn wait_for_stderr(
    plugin: &mut ExternalPlugin,
    predicate: impl Fn(&[String]) -> bool,
    timeout: Duration,
) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let tail = plugin.stderr_tail();
        if predicate(&tail) || Instant::now() >= deadline {
            return tail;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    // 注意：本机 sandbox 下 `tasklist` 会返回 "Access denied"（M2 的 sidecar-guard
    // 测试用的是同一命令，CI 不受影响）。这里改用 PowerShell 的 `Get-Process`，
    // 它对自身子进程可靠，且不需要额外 crate。
    let script = format!("(Get-Process -Id {pid} -ErrorAction SilentlyContinue).Id");
    let output = match Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
    {
        Ok(output) => output,
        Err(_) => return false,
    };
    String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
}

#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string()])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn wait_process_dead(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_alive(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn plugin_error_text(error: &HostError) -> String {
    match error {
        HostError::Plugin(error) => error.to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// CPP-ISO-001..004：正常生命周期
// ---------------------------------------------------------------------------

#[test]
fn cpp_iso_001_spawn_ready_registers_tools_and_panels() {
    let root = TempDir::new("lifecycle");
    let plugin = spawn_fixture(root.path(), "cpp.iso.fixture", None);
    assert_eq!(plugin.plugin_id(), "cpp.iso.fixture");
    assert_eq!(plugin.version(), "0.1.0");
    assert_eq!(plugin.generation(), 1);
    assert_eq!(plugin.tools().len(), 6);
    assert_eq!(plugin.panels().len(), 2);

    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin)).expect("plugin loads");
    assert!(host.is_loaded("cpp.iso.fixture"));
    for tool in ["echo", "slow", "big", "hog", "crash", "env"] {
        assert!(
            host.registry().get(tool).is_some(),
            "tool {tool} registered"
        );
        assert_eq!(
            host.registry().get(tool).unwrap().origin.kind(),
            "native",
            "external-process 工具仍走 Native origin 以复用 Registry"
        );
    }
    assert_eq!(host.panels().len(), 2);
    assert_eq!(host.loaded_plugins(), vec!["cpp.iso.fixture"]);
}

#[test]
fn cpp_iso_002_call_echo_returns_model_visible_result() {
    let root = TempDir::new("echo");
    let plugin = spawn_fixture(root.path(), "cpp.iso.fixture", None);
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin)).unwrap();

    let value = host
        .call("echo", &json!({ "text": "hello g3" }))
        .expect("echo call");
    assert_eq!(value["value"]["echo"], json!("hello g3"));
    assert_eq!(value["is_error"], json!(false));
    assert_eq!(value["content"][0]["type"], json!("json"));

    let error = host.call("echo", &json!({})).unwrap_err();
    assert!(
        matches!(error, HostError::Registry(_)),
        "schema 校验由 Registry 负责: {error:?}"
    );
}

#[test]
fn cpp_iso_003_unload_exits_process_and_is_not_found_afterwards() {
    let root = TempDir::new("unload");
    let plugin = spawn_fixture(root.path(), "cpp.iso.fixture", None);
    let pid = plugin.pid();
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin)).unwrap();
    assert!(process_alive(pid), "spawn 后进程应存活");

    host.unload("cpp.iso.fixture").expect("unload");
    assert!(
        wait_process_dead(pid, Duration::from_secs(5)),
        "pid {pid} 应被杀死"
    );
    assert!(!host.is_loaded("cpp.iso.fixture"));
    assert!(host.registry().is_empty());
    assert!(matches!(
        host.unload("cpp.iso.fixture"),
        Err(HostError::NotFound(_))
    ));
}

#[test]
fn cpp_iso_004_health_check_and_ui_render_roundtrip() {
    let root = TempDir::new("render");
    let mut plugin = spawn_fixture(root.path(), "cpp.iso.fixture", None);
    plugin.health_check().expect("first ping");
    plugin.health_check().expect("second ping");

    let tree = plugin.render_view("fixture.one").expect("ui.render");
    assert_eq!(tree["type"], json!("Text"));
    assert_eq!(tree["a11yLabel"], json!("fixture one"));
    assert!(
        tree["props"]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("fixture one"),
        "tree={tree}"
    );
    let unknown = plugin.render_view("nope").unwrap_err();
    assert!(unknown.to_string().contains("E_SCHEMA"), "{unknown}");
    plugin.unload();
    assert!(!plugin.is_alive());
}

// ---------------------------------------------------------------------------
// CPP-ISO-005..008：G3 故障注入（超时 / 输出 / 内存 / 崩溃）
// ---------------------------------------------------------------------------

#[test]
fn cpp_iso_005_tool_timeout_kills_job_and_host_survives() {
    let root = TempDir::new("timeout");
    let plugin = spawn_fixture(root.path(), "cpp.iso.fixture", None);
    let pid = plugin.pid();
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin)).unwrap();

    let error = host
        .call("slow", &json!({ "ms": 5_000 }))
        .expect_err("slow 工具应在 300ms 看门狗超时");
    let text = plugin_error_text(&error);
    assert!(text.contains("E_TIMEOUT"), "err={text}");
    assert!(
        wait_process_dead(pid, Duration::from_secs(5)),
        "超时后 Job 应被杀"
    );

    // 失败 generation 上的调用继续报错（进程已死），但宿主不崩。
    assert!(host.call("echo", &json!({ "text": "x" })).is_err());
    host.unload("cpp.iso.fixture")
        .expect("unload dead generation");

    // 宿主存活：新 generation 可装载并调用。
    let fresh = spawn_fixture(root.path(), "cpp.iso.fixture", None);
    let fresh_pid = fresh.pid();
    host.load(Box::new(fresh)).expect("reload new generation");
    let value = host.call("echo", &json!({ "text": "after" })).unwrap();
    assert_eq!(value["value"]["echo"], json!("after"));
    host.unload("cpp.iso.fixture").unwrap();
    assert!(wait_process_dead(fresh_pid, Duration::from_secs(5)));
}

#[test]
fn cpp_iso_006_oversized_result_is_e_too_large_and_plugin_survives() {
    let root = TempDir::new("big");
    let plugin = spawn_fixture(root.path(), "cpp.iso.fixture", None);
    let pid = plugin.pid();
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin)).unwrap();

    let error = host
        .call("big", &json!({}))
        .expect_err("1.2 MiB 结果必须被拒");
    let text = plugin_error_text(&error);
    assert!(text.contains("E_TOO_LARGE"), "err={text}");
    assert!(process_alive(pid), "输出超限不应杀进程（SDK 侧结算）");

    // spec §12.4：输出超限不得阻塞其它调用。
    let value = host
        .call("echo", &json!({ "text": "still alive" }))
        .expect("plugin survives oversized output");
    assert_eq!(value["value"]["echo"], json!("still alive"));
    host.unload("cpp.iso.fixture").unwrap();
    assert!(wait_process_dead(pid, Duration::from_secs(5)));
}

#[test]
fn cpp_iso_007_memory_limit_kills_process_and_host_survives() {
    let root = TempDir::new("hog");
    // Job Object 内存上限由 manifest max_memory_mb=64 映射。
    let plugin = spawn_fixture(root.path(), "cpp.iso.fixture", Some(64));
    let pid = plugin.pid();
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin)).unwrap();

    let error = host
        .call("hog", &json!({}))
        .expect_err("512MiB 分配必须被 Job 限制");
    let text = plugin_error_text(&error);
    // 超限的观测点取决于 OS/负载：Job 内存触发、进程退出、或等待结果超时都可能先发生。
    // 断言的是"调用失败 + 进程被终止 + 宿主继续"，而不是某一条具体错误码。
    assert!(
        text.contains("E_INTERNAL")
            || text.contains("E_QUOTA")
            || text.contains("E_TIMEOUT")
            || text.contains("exited"),
        "err={text}"
    );
    assert!(
        wait_process_dead(pid, Duration::from_secs(15)),
        "超过 64MB 的进程应被 Job Object 终止 (pid {pid})"
    );

    host.unload("cpp.iso.fixture")
        .expect("unload after OOM kill");
    let fresh = spawn_fixture(root.path(), "cpp.iso.fixture", Some(64));
    let fresh_pid = fresh.pid();
    host.load(Box::new(fresh)).expect("host survives OOM");
    let value = host.call("echo", &json!({ "text": "oom" })).unwrap();
    assert_eq!(value["value"]["echo"], json!("oom"));
    host.unload("cpp.iso.fixture").unwrap();
    assert!(wait_process_dead(fresh_pid, Duration::from_secs(10)));
}

#[test]
fn cpp_iso_008_crash_invalidates_generation_but_host_survives() {
    let root = TempDir::new("crash");
    let plugin = spawn_fixture(root.path(), "cpp.iso.fixture", None);
    let pid = plugin.pid();
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin)).unwrap();

    let error = host
        .call("crash", &json!({}))
        .expect_err("crash 工具必须失败");
    let text = plugin_error_text(&error);
    assert!(
        text.contains("E_INTERNAL") || text.contains("exited"),
        "err={text}"
    );
    assert!(wait_process_dead(pid, Duration::from_secs(5)));

    // generation 失效：在途后的调用必须报错，而不是挂起。
    assert!(host.call("echo", &json!({ "text": "x" })).is_err());
    host.unload("cpp.iso.fixture")
        .expect("unload crashed generation");

    let fresh = spawn_fixture(root.path(), "cpp.iso.fixture", None);
    let fresh_pid = fresh.pid();
    host.load(Box::new(fresh)).expect("host survives crash");
    let value = host.call("echo", &json!({ "text": "crash" })).unwrap();
    assert_eq!(value["value"]["echo"], json!("crash"));
    host.unload("cpp.iso.fixture").unwrap();
    assert!(wait_process_dead(fresh_pid, Duration::from_secs(5)));
}

// ---------------------------------------------------------------------------
// CPP-ISO-009..014：安全边界与运行时行为
// ---------------------------------------------------------------------------

#[test]
fn cpp_iso_009_entry_path_escape_is_rejected() {
    let root = TempDir::new("escape");
    let package = prepare_package(root.path(), "pkg", None);
    for bad_entry in ["../evil.exe", "../../evil.exe", "/absolute/evil.exe"] {
        let manifest = manifest_for("cpp.iso.evil", bad_entry, None, None);
        let error = ExternalPlugin::spawn(ExternalPluginSpec::new(
            manifest,
            &package,
            host_version(),
            1,
        ))
        .expect_err("逃逸 entry 必须拒绝");
        let text = error.to_string();
        assert!(
            text.contains("inside the package") || text.contains("relative"),
            "entry={bad_entry} err={text}"
        );
    }
}

#[test]
fn cpp_iso_010_missing_entry_and_wrong_runtime_kind_are_rejected() {
    let root = TempDir::new("entry");
    let package = prepare_package(root.path(), "pkg", None);

    let manifest = manifest_for("cpp.iso.missing", "missing.exe", None, None);
    let error = ExternalPlugin::spawn(ExternalPluginSpec::new(
        manifest,
        &package,
        host_version(),
        1,
    ))
    .expect_err("不存在的 entry 必须拒绝");
    assert!(error.to_string().contains("not accessible"), "{error}");

    let wrong_kind = Manifest::from_json_str(
        &json!({
            "plugin": { "id": "cpp.iso.static", "version": "0.1.0", "api": "cordis-portable/v1" },
            "runtime": { "kind": "rust-static" }
        })
        .to_string(),
    )
    .unwrap();
    let error = ExternalPlugin::spawn(ExternalPluginSpec::new(
        wrong_kind,
        &package,
        host_version(),
        1,
    ))
    .expect_err("非 external-process 必须拒绝");
    assert!(error.to_string().contains("external-process"), "{error}");
}

#[cfg(windows)]
#[test]
fn cpp_iso_011_non_executable_entry_is_rejected() {
    let root = TempDir::new("nonexe");
    let package = prepare_package(root.path(), "pkg", None);
    fs::write(package.join("notes.txt"), "not a program").unwrap();
    let manifest = manifest_for("cpp.iso.nonexe", "notes.txt", None, None);
    let error = ExternalPlugin::spawn(ExternalPluginSpec::new(
        manifest,
        &package,
        host_version(),
        1,
    ))
    .expect_err("非 .exe entry 必须拒绝");
    assert!(error.to_string().contains("executable"), "{error}");
}

#[test]
fn cpp_iso_012_child_environment_is_sanitized() {
    let root = TempDir::new("env");
    // 宿主环境里放一个凭据类变量，子进程不得继承（sidecar-guard sanitize）。
    std::env::set_var("DEEPSEEK_API_KEY", "sk-should-not-leak");
    let plugin = spawn_fixture(root.path(), "cpp.iso.fixture", None);
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin)).unwrap();

    let value = host.call("env", &json!({})).expect("env tool");
    assert_eq!(value["value"]["has_deepseek_key"], json!(false));
    assert_eq!(value["value"]["plugin_id"], json!("cpp.iso.fixture"));
    assert_eq!(value["value"]["generation"], json!("1"));
    host.unload("cpp.iso.fixture").unwrap();
}

#[test]
fn cpp_iso_013_unknown_tool_direct_call_returns_unknown_tool() {
    let root = TempDir::new("unknown");
    let mut plugin = spawn_fixture(root.path(), "cpp.iso.fixture", None);
    let error = plugin
        .call("nope", &json!({}))
        .expect_err("未知工具必须报 UnknownTool");
    assert!(matches!(
        error,
        PluginError::UnknownTool { ref plugin, ref tool } if plugin == "cpp.iso.fixture" && tool == "nope"
    ));
    plugin.unload();
}

#[test]
fn cpp_iso_014_two_plugins_are_isolated() {
    let root = TempDir::new("isolated");
    let plugin_a = spawn_fixture(root.path(), "cpp.iso.a", None);
    let mut plugin_b = spawn_fixture(root.path(), "cpp.iso.b", None);
    let pid_a = plugin_a.pid();
    let pid_b = plugin_b.pid();

    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin_a)).unwrap();
    host.unload("cpp.iso.a").unwrap();
    assert!(wait_process_dead(pid_a, Duration::from_secs(5)));
    assert!(process_alive(pid_b), "卸载 A 不得影响 B");

    let value = plugin_b
        .call("echo", &json!({ "text": "b" }))
        .expect("B 仍可调用");
    assert_eq!(value["value"]["echo"], json!("b"));
    plugin_b.unload();
    assert!(wait_process_dead(pid_b, Duration::from_secs(5)));
}

#[test]
fn cpp_iso_015_api_and_host_version_are_checked_at_spawn() {
    let root = TempDir::new("version");
    let package = prepare_package(root.path(), "pkg", None);

    let old_host = manifest_for("cpp.iso.oldhost", fixture_entry_name(), None, Some("9.9.9"));
    let error = ExternalPlugin::spawn(ExternalPluginSpec::new(
        old_host,
        &package,
        host_version(),
        1,
    ))
    .expect_err("min_host_version 过高必须拒绝");
    assert!(
        matches!(&error, HostError::Plugin(PluginError::HostTooOld { .. })),
        "err={error:?}"
    );

    // api 不匹配：绕过 from_json_str 的 api 检查后，spawn 的 validate 仍拒绝。
    let raw = json!({
        "plugin": { "id": "cpp.iso.api", "version": "0.1.0", "api": "cordis-portable/v2" },
        "runtime": { "kind": "external-process", "entry": fixture_entry_name() }
    })
    .to_string();
    let bad_api: Manifest = serde_json::from_str(&raw).unwrap();
    let error = ExternalPlugin::spawn(ExternalPluginSpec::new(
        bad_api,
        &package,
        host_version(),
        1,
    ))
    .expect_err("api 不匹配必须拒绝");
    assert!(
        matches!(
            &error,
            HostError::Plugin(PluginError::UnsupportedApi { .. })
        ),
        "err={error:?}"
    );
}

// ---------------------------------------------------------------------------
// CPP-ISO-020..021：G2 端到端（安装 → 装载 → 调用 → 卸载）
// ---------------------------------------------------------------------------

/// 构造一个带 `dshr-trust.json`（摘要由 `package_digest` 计算）的可安装包。
fn build_installable_package(root: &Path, package_name: &str) -> PathBuf {
    let package = prepare_package(root, "dist", Some(package_name));
    let digest = package_digest(&package).expect("package digest");
    assert_eq!(digest.len(), 64, "sha256 hex");
    fs::write(
        package.join("dshr-trust.json"),
        json!({
            "schema_version": 1,
            "plugin_id": package_name,
            "version": "0.1.0",
            "package": { "digest_sha256": digest }
        })
        .to_string(),
    )
    .expect("write trust file");
    package
}

#[test]
fn cpp_iso_020_g2_end_to_end_install_load_call_unload() {
    let root = TempDir::new("g2");
    let package_name = "cpp.iso.fixture";
    let package = build_installable_package(root.path(), package_name);

    let sidecar_dir = root.path().join("sidecar");
    fs::create_dir_all(&sidecar_dir).unwrap();
    let mut profile = Profile::load(root.path().join("profile")).unwrap();
    let policy = TrustPolicy::default();
    let report =
        install_package_trusted(&sidecar_dir, &package, &mut profile, &policy, |preview| {
            assert_eq!(preview.name, package_name);
            assert_eq!(preview.version, "0.1.0");
            assert_eq!(preview.signature_status, SignatureStatus::Unsigned);
            assert!(preview.first_install);
            true
        })
        .expect("trusted install");
    assert!(report.user_confirmed, "未签名首装必须经确认");
    assert_eq!(report.trust.signature_status, SignatureStatus::Unsigned);

    // 安装把包复制到 sidecar/node_modules/<name> 并写入 profile。
    let installed_dir = report.install.destination.clone();
    assert!(installed_dir.join("plugin.toml").is_file());
    assert!(installed_dir.join("package.json").is_file());
    assert!(installed_dir.join(fixture_entry_name()).is_file());
    assert!(profile
        .entries()
        .iter()
        .any(|entry| entry.name == package_name && entry.enabled));

    // 装载：manifest 与 plugin.toml 等价（离线无 toml parser，用 JSON typed 表达）。
    let manifest = manifest_for(package_name, fixture_entry_name(), None, None);
    let plugin = ExternalPlugin::spawn(ExternalPluginSpec::new(
        manifest,
        &installed_dir,
        host_version(),
        1,
    ))
    .expect("spawn installed plugin");
    let pid = plugin.pid();
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin)).expect("load installed plugin");
    assert!(host.registry().get("echo").is_some());

    // 调用。
    let value = host
        .call("echo", &json!({ "text": "g2-closed-loop" }))
        .expect("call installed plugin");
    assert_eq!(value["value"]["echo"], json!("g2-closed-loop"));

    // 卸载：注册表清空 + 进程退出。
    host.unload(package_name).expect("unload installed plugin");
    assert!(!host.is_loaded(package_name));
    assert!(host.registry().is_empty());
    assert!(wait_process_dead(pid, Duration::from_secs(5)));
}

#[test]
fn cpp_iso_021_trust_confirmation_blocks_unsigned_install() {
    let root = TempDir::new("g2-deny");
    let package_name = "cpp.iso.denied";
    let package = build_installable_package(root.path(), package_name);
    let sidecar_dir = root.path().join("sidecar");
    fs::create_dir_all(&sidecar_dir).unwrap();
    let mut profile = Profile::load(root.path().join("profile")).unwrap();
    let policy = TrustPolicy::default();

    let error = install_package_trusted(&sidecar_dir, &package, &mut profile, &policy, |_| false)
        .expect_err("未确认必须拒绝安装");
    assert_eq!(error.code(), "E_TRUST_NOT_CONFIRMED");
    assert!(!sidecar_dir.join("node_modules").join(package_name).exists());
    // profile 里可能有宿主内置的默认条目；只断言本插件没被写入。
    assert!(!profile
        .entries()
        .iter()
        .any(|entry| entry.name == package_name));
}

// ADC-ELEM-PANEL-HOST-004：`PluginHost::render_panel` 走 external-process 的 `ui.render`。
#[test]
fn adc_elem_panel_host_004_render_panel_through_external_plugin() {
    let root = TempDir::new("adc-panel");
    let plugin = spawn_fixture(root.path(), "cpp.adc.fixture", None);
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin)).unwrap();

    // panel 元数据携带 view（来自 PanelDecl），与注册 id 分开。
    let views: Vec<String> = host
        .panels()
        .iter()
        .map(|registration| registration.panel.view_name().to_owned())
        .collect();
    assert!(
        views.contains(&"fixture.adc".to_owned()),
        "panels 应携带 ADC view: {views:?}"
    );
    assert!(
        views.contains(&"fixture.unknown".to_owned()),
        "panels 应携带未知元素演示 view: {views:?}"
    );

    let tree = host
        .render_panel("cpp.adc.fixture", "fixture.adc")
        .expect("render_panel");
    assert_eq!(tree["type"], json!("Card"));
    assert!(tree["children"]
        .as_array()
        .is_some_and(|items| !items.is_empty()));

    // 未知 view：外部进程返回 schema 错误，宿主映射成可读插件错误。
    let error = host.render_panel("cpp.adc.fixture", "nope").unwrap_err();
    assert!(error.to_string().contains("nope"), "{error}");

    // 未知插件：NotFound（可读）。
    let missing = host
        .render_panel("missing.plugin", "fixture.adc")
        .unwrap_err();
    assert!(matches!(&missing, HostError::NotFound(_)), "{missing}");

    host.unload("cpp.adc.fixture").unwrap();
}

// M4-ADC-003（M4 W3）：ui.action 真正到达 external-process，下一次 ui.render 看到状态变化。
#[test]
fn m4_adc_003_dispatch_action_reaches_plugin_and_updates_view() {
    let root = TempDir::new("adc-action");
    let plugin = spawn_fixture(root.path(), "cpp.adc.fixture", None);
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(plugin)).unwrap();

    let before = host
        .render_panel("cpp.adc.fixture", "fixture.adc")
        .expect("render before action");
    let text_before = before["children"][0]["props"]["value"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(text_before.contains("actions=0"), "{text_before}");

    host.dispatch_action(
        "cpp.adc.fixture",
        "fixture.adc",
        "call",
        &json!({ "method": "refresh", "args": {} }),
    )
    .expect("ui.action dispatch");

    let after = host
        .render_panel("cpp.adc.fixture", "fixture.adc")
        .expect("render after action");
    let text_after = after["children"][0]["props"]["value"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text_after.contains("actions=1"),
        "plugin 应收到 ui.action 并更新状态: {text_after}"
    );
    let registrations = host.slot_registrations("conversation.input.left");
    assert_eq!(registrations.len(), 1);
    let registration = registrations[0];
    assert_eq!(registration.plugin_id.as_deref(), Some("cpp.adc.fixture"));
    assert_eq!(registration.view.as_deref(), Some("fixture.quick"));
    let target = SlotTarget::from(registration);
    host.dispatch_slot_action(target, "call", &json!({ "method": "refresh", "args": {} }))
        .expect("slot action dispatch");
    let slot_view = host
        .render_panel("cpp.adc.fixture", "fixture.quick")
        .unwrap();
    assert_eq!(slot_view["props"]["label"], "actions=2");

    // 未知插件仍然可读报错。
    let error = host
        .dispatch_action("missing.plugin", "fixture.adc", "call", &json!({}))
        .unwrap_err();
    assert!(matches!(&error, HostError::NotFound(_)), "{error:?}");

    host.unload("cpp.adc.fixture").unwrap();
    assert!(host
        .slot_registrations("conversation.input.left")
        .is_empty());
    assert!(host
        .dispatch_slot_action(target, "call", &json!({}))
        .is_err());
}

#[test]
fn tool_slots_send_context_to_sdk_and_reject_mismatched_or_stale_bindings() {
    let root = TempDir::new("adc-tool-views");
    let plugin_id = "cpp.adc.toolviews";
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(spawn_fixture(root.path(), plugin_id, None)))
        .unwrap();
    let arguments = json!({"text": "fixture context"});
    let value = host.call("echo", &arguments).unwrap();
    let context = json!({"call_seq": 7, "tool": "echo", "arguments": arguments, "value": value});
    let targets = host.tool_slot_targets("echo");
    assert_eq!(targets.len(), 2);
    assert_eq!(host.tool_slot_targets("slow"), vec![targets[1]]);
    let render = host.render_tool_slot(targets[0], &context).unwrap();
    assert_eq!(
        render["props"]["value"],
        "fixture.tool:7:echo:\"fixture context\":\"fixture context\""
    );
    let extra = host.render_tool_slot(targets[1], &context).unwrap();
    assert!(extra["props"]["value"]
        .as_str()
        .unwrap()
        .starts_with("fixture.extra:7:"));
    let mut wrong_tool = context.clone();
    wrong_tool["tool"] = json!("slow");
    assert!(host.render_tool_slot(targets[0], &wrong_tool).is_err());
    let mut failed = context.clone();
    failed["value"]["is_error"] = json!(true);
    assert!(host.render_tool_slot(targets[0], &failed).is_err());
    assert!(host
        .dispatch_slot_action(targets[0], "call", &json!({}))
        .is_err());
    let mut oversized = context.clone();
    oversized["value"]["value"] = json!("x".repeat(128 * 1024));
    assert!(host
        .render_tool_slot(targets[0], &oversized)
        .unwrap_err()
        .to_string()
        .contains("128 KiB"));
    host.unload(plugin_id).unwrap();
    assert!(host.render_tool_slot(targets[0], &context).is_err());
}

#[test]
fn registered_commands_and_menu_items_dispatch_their_run_names_to_the_child() {
    let root = TempDir::new("adc-commands");
    let plugin_id = "cpp.adc.commands";
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(spawn_fixture(root.path(), plugin_id, None)))
        .unwrap();
    let command = SlotTarget::from(host.slot_registrations("command")[0]);
    let menu = SlotTarget::from(host.slot_registrations("menu.item")[0]);
    host.dispatch_registered_action(command).unwrap();
    host.dispatch_registered_action(menu).unwrap();
    let rendered = host.render_panel(plugin_id, "fixture.quick").unwrap();
    assert_eq!(rendered["props"]["label"], "actions=2");
    host.unload(plugin_id).unwrap();
    assert!(host.dispatch_registered_action(command).is_err());
    assert!(host.dispatch_registered_action(menu).is_err());
}

#[test]
fn file_preview_delivers_only_the_selected_context_to_the_matching_view() {
    use dshr_plugin_host::{FileViewContext, VIEWER_TAB};
    let root = TempDir::new("adc-file-preview");
    let plugin_id = "cpp.adc.viewer";
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(spawn_fixture(root.path(), plugin_id, None)))
        .unwrap();
    let target = SlotTarget::from(host.slot_registrations(VIEWER_TAB)[0]);
    let mut context = FileViewContext {
        name: "report.TXT".to_owned(),
        file_type: "txt".to_owned(),
        size_bytes: 4,
        text: Some("data".to_owned()),
        truncated: false,
    };
    let tree = host.render_file_slot(target, &context).unwrap();
    assert_eq!(tree["props"]["value"], "report.TXT:txt:data");
    context.file_type = "md".to_owned();
    assert!(host.render_file_slot(target, &context).is_err());
    assert!(host
        .dispatch_slot_action(target, "call", &json!({}))
        .is_err());
    context.file_type = "txt".to_owned();
    host.unload(plugin_id).unwrap();
    assert!(host.render_file_slot(target, &context).is_err());
}

#[test]
fn conversation_node_uses_the_publishing_plugins_key_and_strips_private_metadata() {
    use dshr_plugin_host::ChatNodeContext;

    let root = TempDir::new("adc-chat-node");
    let plugin_id = "cpp.adc.node";
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(spawn_fixture(root.path(), plugin_id, None)))
        .unwrap();
    let mut result = host
        .call("echo", &json!({"text": "hello", "publish_node": true}))
        .unwrap();
    let input = result.clone();
    let node = host.take_tool_chat_node("echo", &mut result).unwrap();
    assert_eq!(
        node,
        json!({"key": "fixture.note", "fallback": "saved note", "data": "hello"})
    );
    assert_eq!(result["value"]["echo"], "hello");
    assert!(result["meta"].get("adc_node").is_none());

    let mut bad = input.clone();
    bad["meta"]["adc_node"]["key"] = json!("other.note");
    assert!(host.take_tool_chat_node("echo", &mut bad).is_none());
    assert!(bad["meta"].get("adc_node").is_none());
    let mut bad = input.clone();
    bad["meta"]["adc_node"]["data"] = json!("x".repeat(16 * 1024));
    assert!(host.take_tool_chat_node("echo", &mut bad).is_none());
    let mut failed = input;
    failed["is_error"] = json!(true);
    assert!(host.take_tool_chat_node("echo", &mut failed).is_none());
    assert!(failed["meta"].get("adc_node").is_none());

    let (target, key) = host.chat_node_targets("echo")[0];
    let mut context = ChatNodeContext {
        key: key.to_owned(),
        call_seq: 5u64.try_into().unwrap(),
        data: node["data"].clone(),
    };
    let tree = host
        .render_chat_node_slot(target, "echo", &context)
        .unwrap();
    assert_eq!(tree["props"]["value"], "fixture.note:5:hello");
    context.key = "other.note".to_owned();
    assert!(host
        .render_chat_node_slot(target, "echo", &context)
        .is_err());
    context.key = "fixture.note".to_owned();
    host.unload(plugin_id).unwrap();
    assert!(host
        .render_chat_node_slot(target, "echo", &context)
        .is_err());
}

// ---------------------------------------------------------------------------
// P1-3：external-process 读写边界（巨大单行 / 子进程不读 stdin）
// ---------------------------------------------------------------------------

// CPP-ISO-018：子进程输出 >1MiB 的无换行单行 → E_TOO_LARGE、Job 被杀、宿主存活。
#[test]
fn cpp_iso_018_huge_single_line_is_rejected_and_job_killed() {
    let root = TempDir::new("fault-huge-line");
    let mut plugin = spawn_fault(root.path(), "huge-line", "cpp.iso.fault.huge");
    let pid = plugin.pid();

    let error = plugin
        .call("fault_probe", &json!({ "text": "x" }))
        .expect_err("超长帧必须失败");
    let text = error.to_string();
    assert!(text.contains("E_TOO_LARGE"), "err={text}");
    assert!(!plugin.is_alive(), "输出超限后 Job 必须被杀");
    assert!(
        wait_process_dead(pid, Duration::from_secs(5)),
        "pid {pid} 应被杀死"
    );

    // 宿主侧实例状态机不 panic：后续调用返回明确的 E_INTERNAL（generation 已失效）。
    let again = plugin
        .call("fault_probe", &json!({}))
        .expect_err("失效 generation 必须报错");
    assert!(again.to_string().contains("E_INTERNAL"), "{again}");
}

// CPP-ISO-019：子进程不读 stdin + 大请求 → 写 watchdog E_TIMEOUT，宿主存活、Job 被杀。
#[test]
fn cpp_iso_019_child_not_reading_stdin_hits_write_watchdog() {
    let root = TempDir::new("fault-no-stdin");
    let mut plugin = spawn_fault(root.path(), "no-stdin", "cpp.iso.fault.stdin");
    let pid = plugin.pid();

    // ~900 KiB 参数：合法帧（<1MiB），但足以填满管道让 write_all 阻塞。
    let big = "x".repeat(900 * 1024);
    let started = Instant::now();
    let error = plugin
        .call("fault_probe", &json!({ "text": big }))
        .expect_err("不读 stdin 的子进程必须触发写 watchdog");
    let text = error.to_string();
    assert!(text.contains("E_TIMEOUT"), "err={text}");
    assert!(
        started.elapsed() >= Duration::from_millis(1_000),
        "必须真的等过写 watchdog（不是立即失败）: {:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "不能在 per-tool timeout（120s）上等: {:?}",
        started.elapsed()
    );
    assert!(!plugin.is_alive(), "写超时后 Job 必须被杀");
    assert!(
        wait_process_dead(pid, Duration::from_secs(5)),
        "pid {pid} 应被杀死"
    );
}

// CPP-ISO-026：故障实例之后，宿主仍能启动并调用正常插件（G3"宿主存活"）。
#[test]
fn cpp_iso_026_host_survives_fault_and_loads_healthy_plugin() {
    let root = TempDir::new("fault-recover");
    {
        let mut faulty = spawn_fault(root.path(), "huge-line", "cpp.iso.fault.recover");
        let _ = faulty.call("fault_probe", &json!({ "text": "x" }));
        drop(faulty);
    }

    // 全新的正常 fixture 仍能 spawn/装载/调用。
    let healthy = spawn_fixture(root.path(), "cpp.iso.healthy", None);
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(healthy)).expect("healthy plugin loads");
    let output = host
        .call("echo", &json!({ "text": "still-alive" }))
        .expect("host survives and dispatches");
    assert!(output.to_string().contains("still-alive"), "{output}");
    host.unload("cpp.iso.healthy").expect("unload");
}

// ---------------------------------------------------------------------------
// P1-3 残余：stderr 有界（巨行 / 大量短行 tail 双上限）
// ---------------------------------------------------------------------------

// CPP-ISO-032：stderr 16 MiB 无换行巨行 → tail 内存有界（≤64 KiB、单行截断），
// 调用路径返回结构化 E_TIMEOUT（不 OOM/不 panic），Job 按设计被杀，宿主存活并
// 能继续装载/调用正常插件。
#[test]
fn cpp_iso_032_huge_stderr_line_is_bounded_and_timeout_kills() {
    let root = TempDir::new("fault-stderr-huge");
    let mut plugin = spawn_fault(root.path(), "stderr-huge", "cpp.iso.fault.stderr.huge");
    let pid = plugin.pid();
    let started = Instant::now();

    // 等宿主把 16 MiB 巨行有界读完（出现截断标记），期间内存只保留 ≤ MAX 行前缀。
    let tail = wait_for_stderr(
        &mut plugin,
        |tail| tail.iter().any(|line| line.contains("stderr truncated")),
        Duration::from_secs(15),
    );
    assert!(
        tail.iter().any(|line| line.contains("stderr truncated")),
        "必须观察到截断标记: {tail:?}"
    );
    let retained_bytes: usize = tail.iter().map(String::len).sum();
    assert!(
        retained_bytes <= STDERR_TAIL_BYTES_LIMIT,
        "tail {retained_bytes} 字节超过上限 {STDERR_TAIL_BYTES_LIMIT}"
    );
    assert!(
        tail.iter()
            .all(|line| line.len() <= MAX_STDERR_LINE_BYTES + 128),
        "单行必须截断（上限 {MAX_STDERR_LINE_BYTES}）"
    );
    assert!(
        plugin.is_alive(),
        "stderr 巨行应被 drain+丢弃，不应阻塞或杀死进程"
    );

    // 16 MiB stderr 之后：子进程不读 stdin → 大请求填满管道，写 watchdog 结算 E_TIMEOUT
    // （结构化错误，而不是 OOM/panic；也不是等 120s 的 per-tool timeout）。
    let big = "x".repeat(900 * 1024);
    let error = plugin
        .call("fault_probe", &json!({ "text": big }))
        .expect_err("不读 stdin 必须超时");
    assert!(error.to_string().contains("E_TIMEOUT"), "{error}");
    assert!(
        wait_process_dead(pid, Duration::from_secs(10)),
        "写超时后 Job 必须被杀（按设计）"
    );
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(60), "耗时过长: {elapsed:?}");
    eprintln!(
        "[CPP-ISO-032] stderr_input=16MiB retained_lines={} retained_bytes={retained_bytes} elapsed={elapsed:?}",
        tail.len()
    );

    // 宿主存活：故障实例之后仍能装载/调用正常插件。
    let healthy = spawn_fixture(root.path(), "cpp.iso.stderr.healthy", None);
    let mut host = PluginHost::new(AllowAll);
    host.load(Box::new(healthy)).expect("healthy plugin loads");
    let output = host
        .call("echo", &json!({ "text": "after-stderr-fault" }))
        .expect("host still dispatches");
    assert!(
        output.to_string().contains("after-stderr-fault"),
        "{output}"
    );
    host.unload("cpp.iso.stderr.healthy").expect("unload");
}

// CPP-ISO-033：stderr 5000 条短行 → tail 只保留最近 STDERR_TAIL_LINE_LIMIT 行，
// 总字节也受 STDERR_TAIL_BYTES_LIMIT 约束（行/字节双上限）。
#[test]
fn cpp_iso_033_stderr_tail_retains_only_recent_flood_lines() {
    let root = TempDir::new("fault-stderr-flood");
    let mut plugin = spawn_fault(root.path(), "stderr-flood", "cpp.iso.fault.stderr.flood");
    let pid = plugin.pid();
    let started = Instant::now();

    let tail = wait_for_stderr(
        &mut plugin,
        |tail| tail.last().is_some_and(|line| line.contains("flood-4999")),
        Duration::from_secs(15),
    );
    assert_eq!(
        tail.len(),
        STDERR_TAIL_LINE_LIMIT,
        "tail 必须只保留最近 {STDERR_TAIL_LINE_LIMIT} 行: {:?}",
        tail.first()
    );
    assert!(
        tail.first().is_some_and(|line| line.contains("flood-4936")),
        "最旧保留行应为 flood-4936: {:?}",
        tail.first()
    );
    assert!(
        tail.last().is_some_and(|line| line.contains("flood-4999")),
        "最新行应为 flood-4999"
    );
    let retained_bytes: usize = tail.iter().map(String::len).sum();
    assert!(
        retained_bytes <= STDERR_TAIL_BYTES_LIMIT,
        "tail {retained_bytes} 字节超过上限 {STDERR_TAIL_BYTES_LIMIT}"
    );
    eprintln!(
        "[CPP-ISO-033] flood_lines=5000 retained_lines={} retained_bytes={retained_bytes} elapsed={:?}",
        tail.len(),
        started.elapsed()
    );

    drop(plugin);
    assert!(
        wait_process_dead(pid, Duration::from_secs(10)),
        "drop/unload 后 Job 必须终止"
    );
}
