//! WASM 主机隔离测试（`CPP-WASM-*`，G3 主方案）。
//!
//! 全部用内联 WAT fixture（不需要 wasm32 target、不联网）。覆盖四类配额：
//! fuel 耗尽、epoch 墙钟超时、StoreLimits 内存超限、输出超限；以及 ABI/恢复/并发。

use std::sync::Arc;
use std::time::{Duration, Instant};

use dshr_plugin_api::Manifest;
use dshr_plugin_wasm::{
    WasmEngineConfig, WasmError, WasmPluginRuntime, ABI_VERSION, DEFAULT_MAX_CONCURRENCY,
    DEFAULT_MAX_MEMORY_MB, DEFAULT_TIMEOUT_MS, MAX_FUEL, MAX_MEMORY_MB, WASMTIME_VERSION,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// WAT fixtures
// ---------------------------------------------------------------------------

/// 回显：`dshr_invoke` 把入参 JSON 原样打包返回（宿主能读回 request JSON）。
const ECHO_WAT: &str = r#"
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

/// 固定返回 `{"ok":true}`（11 字节，存放在 data 段）。
const FIXED_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (data (i32.const 1024) "{\"ok\":true}")
  (func (export "dshr_alloc") (param $len i32) (result i32) (i32.const 2048))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (i64.or
      (i64.shl (i64.extend_i32_u (i32.const 1024)) (i64.const 32))
      (i64.const 11)))
)
"#;

/// 死循环：用于 fuel 耗尽 / epoch 超时。
const SPIN_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "dshr_alloc") (param $len i32) (result i32) (i32.const 4096))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (loop $spin (br $spin))
    (i64.const 0))
)
"#;

/// 不断 `memory.grow`：用于 StoreLimits 内存上限。
const MEMHOG_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "dshr_alloc") (param $len i32) (result i32) (i32.const 4096))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (loop $grow
      (drop (memory.grow (i32.const 1)))
      (br $grow))
    (i64.const 0))
)
"#;

/// 返回 1 MiB 结果长度（但不真正写内存）：用于输出上限。
const BIGOUT_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "dshr_alloc") (param $len i32) (result i32) (i32.const 4096))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (i64.or
      (i64.shl (i64.extend_i32_u (i32.const 0)) (i64.const 32))
      (i64.extend_i32_u (i32.const 1048576))))
)
"#;

const GUEST_ERR_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "dshr_alloc") (param $len i32) (result i32) (i32.const 4096))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (i64.const -1))
)
"#;

const TRAP_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "dshr_alloc") (param $len i32) (result i32) (i32.const 4096))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (unreachable)
    (i64.const 0))
)
"#;

const NO_MEMORY_WAT: &str = r#"
(module
  (func (export "dshr_alloc") (param $len i32) (result i32) (i32.const 0))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (i64.const 0))
)
"#;

const NO_EXPORTS_WAT: &str = r#"
(module
  (memory (export "memory") 1)
)
"#;

const NO_ALLOC_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (i64.const 0))
)
"#;

const BAD_ALLOC_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "dshr_alloc") (param $len i32) (result i32) (i32.const -1))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (i64.const 0))
)
"#;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn base_config() -> WasmEngineConfig {
    WasmEngineConfig::default().with_epoch_tick_ms(5)
}

fn echo_runtime() -> WasmPluginRuntime {
    WasmPluginRuntime::from_wat(ECHO_WAT, base_config()).expect("echo module compiles")
}

fn spin_config(fuel: u64, timeout_ms: u64) -> WasmEngineConfig {
    WasmEngineConfig {
        fuel,
        timeout_ms,
        epoch_tick_ms: 5,
        ..base_config()
    }
}

fn wasm_manifest(fuel: Option<u64>, memory_mb: Option<u64>, timeout_ms: Option<u64>) -> Manifest {
    let mut runtime = json!({ "kind": "wasm-component", "entry": "plugin.wasm" });
    if let Some(fuel) = fuel {
        runtime["fuel"] = json!(fuel);
    }
    if let Some(memory_mb) = memory_mb {
        runtime["max_memory_mb"] = json!(memory_mb);
    }
    if let Some(timeout_ms) = timeout_ms {
        runtime["timeout_ms"] = json!(timeout_ms);
    }
    let raw = json!({
        "plugin": { "id": "cpp.wasm.fixture", "version": "0.1.0", "api": "cordis-portable/v1" },
        "runtime": runtime
    })
    .to_string();
    Manifest::from_json_str(&raw).expect("fixture manifest parses")
}

fn wait_until_active(runtime: &WasmPluginRuntime, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while runtime.active_calls() == 0 {
        assert!(Instant::now() < deadline, "调用应在超时前进入在途状态");
        std::thread::sleep(Duration::from_millis(2));
    }
}

// ---------------------------------------------------------------------------
// CPP-WASM-001..008：加载 / ABI / 正常调用
// ---------------------------------------------------------------------------

#[test]
fn cpp_wasm_001_wat_module_validates() {
    assert!(WasmPluginRuntime::validate_module(ECHO_WAT.as_bytes()).is_ok());
    assert!(WasmPluginRuntime::validate_module(FIXED_WAT.as_bytes()).is_ok());
    assert_eq!(ABI_VERSION, "dshr-wasm-core/v1");
    assert_eq!(WASMTIME_VERSION, "50.0.1");
}

#[test]
fn cpp_wasm_002_invalid_module_is_rejected() {
    let error = WasmPluginRuntime::validate_module(b"this is definitely not wasm")
        .expect_err("非 wasm 内容必须拒绝");
    assert!(
        matches!(error, WasmError::InvalidModule(_)),
        "error={error:?}"
    );
    assert_eq!(error.code(), "E_PROTOCOL");

    let error = WasmPluginRuntime::from_wat("(module (broken", base_config())
        .expect_err("非法 WAT 必须拒绝");
    assert!(matches!(error, WasmError::InvalidModule(_)));
}

#[test]
fn cpp_wasm_003_echo_invoke_roundtrips_json() {
    let runtime = echo_runtime();
    let args = json!({ "text": "hello wasm" });
    let value = runtime.invoke("echo", &args).expect("echo invoke");
    assert_eq!(value["tool"], json!("echo"));
    assert_eq!(value["arguments"], args);
    assert_eq!(runtime.active_calls(), 0, "调用结束后并发计数必须归还");
}

#[test]
fn cpp_wasm_004_guest_can_return_fixed_json() {
    let runtime = WasmPluginRuntime::from_wat(FIXED_WAT, base_config()).unwrap();
    let value = runtime.invoke("fixed", &json!({})).expect("fixed invoke");
    assert_eq!(value, json!({ "ok": true }));
}

#[test]
fn cpp_wasm_005_missing_dshr_invoke_is_abi_error() {
    // 能编译（语法合法），但 ABI 导出不完整 → invoke 返回结构化 E_PROTOCOL。
    let runtime = WasmPluginRuntime::from_wat(NO_EXPORTS_WAT, base_config()).unwrap();
    let error = runtime.invoke("echo", &json!({})).unwrap_err();
    assert!(matches!(error, WasmError::Abi(_)), "error={error:?}");
    assert_eq!(error.code(), "E_PROTOCOL");
}

#[test]
fn cpp_wasm_006_missing_memory_is_abi_error() {
    let runtime = WasmPluginRuntime::from_wat(NO_MEMORY_WAT, base_config()).unwrap();
    let error = runtime.invoke("echo", &json!({})).unwrap_err();
    assert!(matches!(error, WasmError::Abi(_)), "error={error:?}");
    assert!(error.to_string().contains("memory"), "error={error}");
}

#[test]
fn cpp_wasm_007_missing_alloc_and_negative_alloc_are_abi_errors() {
    let runtime = WasmPluginRuntime::from_wat(NO_ALLOC_WAT, base_config()).unwrap();
    let error = runtime.invoke("echo", &json!({})).unwrap_err();
    assert!(error.to_string().contains("dshr_alloc"), "error={error}");

    let runtime = WasmPluginRuntime::from_wat(BAD_ALLOC_WAT, base_config()).unwrap();
    let error = runtime.invoke("echo", &json!({})).unwrap_err();
    assert!(matches!(error, WasmError::Abi(_)), "error={error:?}");
}

#[test]
fn cpp_wasm_008_runtime_info_matches_config() {
    let runtime = echo_runtime();
    let info = runtime.info();
    assert_eq!(info.abi_version, ABI_VERSION);
    assert_eq!(info.wasmtime_version, WASMTIME_VERSION);
    assert_eq!(info.fuel, WasmEngineConfig::default().fuel);
    assert_eq!(info.max_concurrency, DEFAULT_MAX_CONCURRENCY);
    assert_eq!(info.max_output_bytes, 1_048_576);
    assert_eq!(runtime.trap_count(), 0);
}

// ---------------------------------------------------------------------------
// CPP-WASM-009..013：四类配额隔离（fuel / epoch / 内存 / 输出）
// ---------------------------------------------------------------------------

#[test]
fn cpp_wasm_009_fuel_exhaustion_is_quota_error() {
    let runtime = WasmPluginRuntime::from_wat(SPIN_WAT, spin_config(100_000, 30_000)).unwrap();
    let started = Instant::now();
    let error = runtime.invoke("spin", &json!({})).unwrap_err();
    assert!(
        matches!(error, WasmError::OutOfFuel),
        "error={error:?} elapsed={:?}",
        started.elapsed()
    );
    assert_eq!(error.code(), "E_QUOTA");
    assert!(error.is_quota());
    assert_eq!(runtime.trap_count(), 1, "trap 计数应 +1");
    // 宿主（同一 runtime）存活：下一次调用重新建 Store，正常返回。
    let runtime = WasmPluginRuntime::from_wat(ECHO_WAT, base_config()).unwrap();
    assert_eq!(
        runtime.invoke("echo", &json!({ "ok": 1 })).unwrap()["arguments"]["ok"],
        json!(1)
    );
}

#[test]
fn cpp_wasm_010_epoch_timeout_is_timeout_error() {
    // fuel 很大（1e12）保证先撞墙钟而不是燃料。
    let runtime =
        WasmPluginRuntime::from_wat(SPIN_WAT, spin_config(1_000_000_000_000, 100)).unwrap();
    let started = Instant::now();
    let error = runtime.invoke("spin", &json!({})).unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        matches!(error, WasmError::Timeout { .. }),
        "error={error:?} elapsed={elapsed:?}"
    );
    assert_eq!(error.code(), "E_TIMEOUT");
    assert!(
        elapsed >= Duration::from_millis(80) && elapsed < Duration::from_secs(10),
        "epoch 超时应接近 100ms，实际 {elapsed:?}"
    );
    assert_eq!(runtime.trap_count(), 1);
}

#[test]
fn cpp_wasm_011_memory_limit_is_quota_error() {
    let config = WasmEngineConfig {
        max_memory_bytes: 1024 * 1024, // 1 MiB（16 pages），初始 1 page
        fuel: 2_000_000_000,
        timeout_ms: 10_000,
        epoch_tick_ms: 5,
        ..base_config()
    };
    let runtime = WasmPluginRuntime::from_wat(MEMHOG_WAT, config).unwrap();
    let error = runtime.invoke("hog", &json!({})).unwrap_err();
    assert!(
        matches!(error, WasmError::MemoryLimit(_)),
        "error={error:?}"
    );
    assert_eq!(error.code(), "E_QUOTA");
    assert!(error.is_quota());
}

#[test]
fn cpp_wasm_012_output_limit_is_too_large() {
    let config = WasmEngineConfig {
        max_output_bytes: 4 * 1024,
        ..base_config()
    };
    let runtime = WasmPluginRuntime::from_wat(BIGOUT_WAT, config).unwrap();
    let error = runtime.invoke("big", &json!({})).unwrap_err();
    match &error {
        WasmError::OutputTooLarge { size, limit } => {
            assert_eq!(*size, 1_048_576);
            assert_eq!(*limit, 4 * 1024);
        }
        other => panic!("预期 OutputTooLarge，实际 {other:?}"),
    }
    assert_eq!(error.code(), "E_TOO_LARGE");
}

#[test]
fn cpp_wasm_013_guest_error_and_trap_are_reported_without_panicking() {
    let runtime = WasmPluginRuntime::from_wat(GUEST_ERR_WAT, base_config()).unwrap();
    let error = runtime.invoke("err", &json!({})).unwrap_err();
    assert!(matches!(error, WasmError::Guest(_)), "error={error:?}");
    assert_eq!(error.code(), "E_INTERNAL");

    let runtime = WasmPluginRuntime::from_wat(TRAP_WAT, base_config()).unwrap();
    let error = runtime.invoke("trap", &json!({})).unwrap_err();
    assert!(matches!(error, WasmError::Trap(_)), "error={error:?}");
    assert_eq!(error.code(), "E_INTERNAL");
    assert_eq!(runtime.trap_count(), 1);
}

// ---------------------------------------------------------------------------
// CPP-WASM-014..018：恢复 / 并发 / 宿主存活 / manifest 映射
// ---------------------------------------------------------------------------

#[test]
fn cpp_wasm_014_invoke_after_trap_reuses_runtime() {
    // 同一 runtime 的每次 invoke 都新建 Store：前一次 trap 不影响后一次。
    let runtime = WasmPluginRuntime::from_wat(TRAP_WAT, base_config()).unwrap();
    assert!(runtime.invoke("trap", &json!({})).is_err());
    assert!(runtime.invoke("trap", &json!({})).is_err());
    assert_eq!(runtime.trap_count(), 2, "每次 trap 都应计数");
}

#[test]
fn cpp_wasm_015_relaunch_rebuilds_engine_and_module() {
    let mut runtime = WasmPluginRuntime::from_wat(ECHO_WAT, base_config()).unwrap();
    assert_eq!(
        runtime.invoke("echo", &json!({ "a": 1 })).unwrap()["arguments"]["a"],
        json!(1)
    );
    runtime.relaunch().expect("relaunch");
    assert_eq!(
        runtime.invoke("echo", &json!({ "a": 2 })).unwrap()["arguments"]["a"],
        json!(2)
    );

    // trap 后 relaunch 仍可工作（trap 计数保留为诊断）。
    let mut spinning = WasmPluginRuntime::from_wat(SPIN_WAT, spin_config(100_000, 30_000)).unwrap();
    assert!(spinning.invoke("spin", &json!({})).is_err());
    assert_eq!(spinning.trap_count(), 1);
    spinning.relaunch().expect("relaunch after trap");
    assert!(spinning.invoke("spin", &json!({})).is_err());
    assert_eq!(spinning.trap_count(), 2);
}

#[test]
fn cpp_wasm_016_concurrency_limit_returns_backpressure() {
    let config = WasmEngineConfig {
        max_concurrency: 1,
        ..spin_config(1_000_000_000_000, 300)
    };
    let runtime = Arc::new(WasmPluginRuntime::from_wat(SPIN_WAT, config).unwrap());

    let worker = {
        let runtime = Arc::clone(&runtime);
        std::thread::spawn(move || runtime.invoke("spin", &json!({})))
    };
    wait_until_active(&runtime, Duration::from_secs(2));

    let error = runtime.invoke("echo", &json!({})).unwrap_err();
    assert!(
        matches!(error, WasmError::Backpressure { limit: 1 }),
        "error={error:?}"
    );
    assert_eq!(error.code(), "E_BACKPRESSURE");

    let result = worker.join().expect("worker thread");
    assert!(
        matches!(result, Err(WasmError::Timeout { .. })),
        "{result:?}"
    );
    // 槽位已归还：后续调用正常。
    assert_eq!(runtime.active_calls(), 0);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(runtime.active_calls(), 0);
}

#[test]
fn cpp_wasm_017_trap_in_one_runtime_does_not_affect_another() {
    let spinning = WasmPluginRuntime::from_wat(SPIN_WAT, spin_config(100_000, 30_000)).unwrap();
    let echo = WasmPluginRuntime::from_wat(ECHO_WAT, base_config()).unwrap();
    assert!(spinning.invoke("spin", &json!({})).is_err());
    let value = echo.invoke("echo", &json!({ "alive": true })).unwrap();
    assert_eq!(value["arguments"]["alive"], json!(true));
    assert_eq!(spinning.trap_count(), 1);
    assert_eq!(echo.trap_count(), 0);
}

#[test]
fn cpp_wasm_018_from_manifest_maps_quota_fields() {
    let manifest = wasm_manifest(Some(3_000_000_000), Some(256), Some(12_000));
    let config = WasmEngineConfig::from_manifest(&manifest).expect("map manifest");
    assert_eq!(config.fuel, 3_000_000_000);
    assert_eq!(config.max_memory_bytes, 256 * 1024 * 1024);
    assert_eq!(config.timeout_ms, 12_000);
    assert_eq!(config.max_output_bytes, 1_048_576);
    assert_eq!(config.max_concurrency, 4);

    let defaults = WasmEngineConfig::from_manifest(&wasm_manifest(None, None, None)).unwrap();
    assert_eq!(defaults.fuel, 2_000_000_000);
    assert_eq!(
        defaults.max_memory_bytes,
        DEFAULT_MAX_MEMORY_MB * 1024 * 1024
    );
    assert_eq!(defaults.timeout_ms, DEFAULT_TIMEOUT_MS);
}

#[test]
fn cpp_wasm_019_from_manifest_rejects_wrong_kind_and_out_of_range() {
    // 非 wasm-component：拒绝（rust-static 由 plugin-host 的 native 路径处理）。
    let raw = json!({
        "plugin": { "id": "cpp.wasm.bad", "version": "0.1.0", "api": "cordis-portable/v1" },
        "runtime": { "kind": "rust-static" }
    })
    .to_string();
    let manifest = Manifest::from_json_str(&raw).unwrap();
    let error = WasmEngineConfig::from_manifest(&manifest).unwrap_err();
    assert_eq!(error.code(), "E_MANIFEST");

    // fuel 超上限 / 内存超上限：manifest schema 层可能已拦，这里直接测 from_runtime。
    let mut runtime = wasm_manifest(Some(MAX_FUEL), Some(MAX_MEMORY_MB), None)
        .runtime
        .clone();
    assert!(WasmEngineConfig::from_runtime(&runtime).is_ok());
    runtime.fuel = Some(MAX_FUEL + 1);
    let error = WasmEngineConfig::from_runtime(&runtime).unwrap_err();
    assert_eq!(error.code(), "E_MANIFEST");
    runtime.fuel = None;
    runtime.max_memory_mb = Some(MAX_MEMORY_MB + 1);
    assert_eq!(
        WasmEngineConfig::from_runtime(&runtime).unwrap_err().code(),
        "E_MANIFEST"
    );
    runtime.max_memory_mb = Some(0);
    assert!(WasmEngineConfig::from_runtime(&runtime).is_err());
}

#[test]
fn cpp_wasm_020_from_file_loads_wat() {
    let path = std::env::temp_dir().join(format!(
        "dshr-wasm-fixture-{}-{}.wat",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, ECHO_WAT).expect("write wat fixture");
    let runtime = WasmPluginRuntime::from_file(&path, base_config()).expect("load wat file");
    let value = runtime.invoke("echo", &json!({ "from": "file" })).unwrap();
    assert_eq!(value["arguments"]["from"], json!("file"));
    let _ = std::fs::remove_file(&path);

    let error = WasmPluginRuntime::from_file(
        std::env::temp_dir().join("dshr-wasm-does-not-exist.wasm"),
        base_config(),
    )
    .expect_err("不存在的文件必须报错");
    assert!(matches!(error, WasmError::InvalidModule(_)));
}
