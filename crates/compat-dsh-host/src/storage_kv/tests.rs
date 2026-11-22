#![cfg(test)]

use super::*;
use crate::service::HostServiceRouter;
use dshr_compat_dsh_protocol::{
    Envelope, CAP_SERVICE_PROXY, KIND_HOST_SERVICE_REQUEST, PROTOCOL_MAX,
};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const PLUGIN: &str = "example.dsh.tool";

fn temp_root(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "dshr-storage-kv-{name}-{}-{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn service(name: &str) -> (StorageKvService, PathBuf) {
    let root = temp_root(name);
    let service = StorageKvService::open(&root).expect("storage service opens");
    (service, root)
}

fn ctx(plugin: &str) -> ServiceCallContext {
    ServiceCallContext {
        plugin_id: plugin.to_owned(),
        generation: "node-0001".to_owned(),
        session_id: None,
        call_id: None,
        tool_request_id: None,
        depth: 0,
    }
}

fn open_args(
    name: &str,
    version: u32,
    compatible_versions: &[u32],
    layout: StorageLayout,
    tables: &[&str],
    has_global: bool,
) -> Value {
    json!({ "unit": {
        "name": name,
        "version": version,
        "compatible_versions": compatible_versions,
        "layout": layout.as_str(),
        "tables": tables,
        "has_global": has_global,
    }})
}

fn open_unit(
    service: &mut StorageKvService,
    plugin: &str,
    args: Value,
) -> Result<String, ServiceError> {
    let reply = service.dispatch("unit.open", &args, &ctx(plugin))?;
    Ok(reply["unit_id"]
        .as_str()
        .expect("unit_id in reply")
        .to_owned())
}

fn put(
    service: &mut StorageKvService,
    plugin: &str,
    unit_id: &str,
    table: &str,
    key: &str,
    value: Value,
) -> Result<(), ServiceError> {
    service
        .dispatch(
            "unit.putRecord",
            &json!({ "unit_id": unit_id, "table": table, "key": key, "value": value }),
            &ctx(plugin),
        )
        .map(|_| ())
}

fn load_all(
    service: &mut StorageKvService,
    plugin: &str,
    unit_id: &str,
) -> Result<Value, ServiceError> {
    service.dispatch("unit.loadAll", &json!({ "unit_id": unit_id }), &ctx(plugin))
}

fn storage_code(error: &ServiceError) -> &str {
    error
        .detail
        .as_ref()
        .and_then(|detail| detail.get("storage_code"))
        .and_then(Value::as_str)
        .unwrap_or("<none>")
}

fn read_medium(root: &Path, plugin: &str, unit: &str) -> Value {
    let path = root.join(plugin).join(format!("{unit}.json"));
    serde_json::from_slice(&std::fs::read(&path).expect("medium file")).expect("medium json")
}

/// RPC-L2-301：`unit.open` 幂等；介质文件形状符合 spec §13.8。
#[test]
fn rpc_l2_301_open_is_idempotent_per_descriptor() {
    let (mut service, root) = service("301");
    let args = open_args("todos", 1, &[], StorageLayout::Single, &["items"], false);
    let first = open_unit(&mut service, PLUGIN, args.clone()).expect("open");
    let second = open_unit(&mut service, PLUGIN, args).expect("reopen");
    assert_eq!(first, second, "同 descriptor 重开必须返回同 unit_id");

    let file = read_medium(&root, PLUGIN, "todos");
    assert_eq!(file["version"], 1);
    assert_eq!(file["layout"], "single");
    assert_eq!(file["compatible_versions"], json!([]));
    assert!(file["tables"]["items"].is_object());
    assert!(file["global"].is_null());
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-302：`putRecord` → `loadAll` 往返一致（single layout）。
#[test]
fn rpc_l2_302_put_record_round_trips_in_single_layout() {
    let (mut service, root) = service("302");
    let unit_id = open_unit(
        &mut service,
        PLUGIN,
        open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
    )
    .expect("open");
    put(
        &mut service,
        PLUGIN,
        &unit_id,
        "items",
        "a",
        json!({ "n": 1 }),
    )
    .expect("put a");
    put(
        &mut service,
        PLUGIN,
        &unit_id,
        "items",
        "b",
        json!({ "n": 2 }),
    )
    .expect("put b");

    let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
    assert_eq!(snapshot["tables"]["items"]["a"]["n"], 1);
    assert_eq!(snapshot["tables"]["items"]["b"]["n"], 2);
    assert!(
        snapshot["global"].is_null(),
        "未写 global 时必须显式返回 null（官方 domain 用 null 触发 initial）"
    );

    let file = read_medium(&root, PLUGIN, "todos");
    assert_eq!(file["tables"]["items"]["a"]["n"], 1);
    assert_eq!(file["version"], 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-303：`per-record` layout 的读写往返（两种 layout 语义各一例）。
#[test]
fn rpc_l2_303_per_record_layout_round_trips() {
    let (mut service, root) = service("303");
    let unit_id = open_unit(
        &mut service,
        PLUGIN,
        open_args(
            "index",
            3,
            &[1, 2],
            StorageLayout::PerRecord,
            &["pages", "meta"],
            false,
        ),
    )
    .expect("open");
    put(&mut service, PLUGIN, &unit_id, "pages", "p1", json!("one")).expect("put");
    put(&mut service, PLUGIN, &unit_id, "pages", "p2", json!("two")).expect("put");
    put(&mut service, PLUGIN, &unit_id, "meta", "count", json!(2)).expect("put");

    let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
    assert_eq!(snapshot["tables"]["pages"]["p1"], "one");
    assert_eq!(snapshot["tables"]["pages"]["p2"], "two");
    assert_eq!(snapshot["tables"]["meta"]["count"], 2);
    assert_eq!(read_medium(&root, PLUGIN, "index")["layout"], "per-record");
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-304：版本戳不兼容 → `E_INTERNAL` + `detail.storage_code="version-mismatch"`；
/// 声明 `compatible_versions` 后可读，写入时原子升级版本戳。
#[test]
fn rpc_l2_304_version_stamp_mismatch_and_upgrade() {
    let (mut writer, root) = service("304");
    let unit_id = open_unit(
        &mut writer,
        PLUGIN,
        open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
    )
    .expect("open v1");
    put(&mut writer, PLUGIN, &unit_id, "items", "a", json!(1)).expect("put");
    drop(writer);

    let mut reader = StorageKvService::open(&root).expect("reopen root");
    let error = open_unit(
        &mut reader,
        PLUGIN,
        open_args("todos", 2, &[], StorageLayout::Single, &["items"], false),
    )
    .expect_err("version 2 without compat must be rejected");
    assert_eq!(error.code, "E_INTERNAL");
    assert!(!error.retryable);
    assert_eq!(storage_code(&error), "version-mismatch");

    let upgraded = open_unit(
        &mut reader,
        PLUGIN,
        open_args("todos", 2, &[1], StorageLayout::Single, &["items"], false),
    )
    .expect("version 2 accepting version 1");
    let snapshot = load_all(&mut reader, PLUGIN, &upgraded).expect("loadAll");
    assert_eq!(
        snapshot["tables"]["items"]["a"], 1,
        "兼容版本必须能读旧数据"
    );
    put(&mut reader, PLUGIN, &upgraded, "items", "b", json!(2)).expect("put");
    assert_eq!(
        read_medium(&root, PLUGIN, "todos")["version"],
        2,
        "写操作原子打上当前版本戳"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-305：`deleteRecord` / `deleteRecords` 幂等、计数正确、缺 key 不报错。
#[test]
fn rpc_l2_305_deletes_are_idempotent() {
    let (mut service, root) = service("305");
    let unit_id = open_unit(
        &mut service,
        PLUGIN,
        open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
    )
    .expect("open");
    for key in ["a", "b", "c"] {
        put(&mut service, PLUGIN, &unit_id, "items", key, json!(key)).expect("put");
    }

    let reply = service
        .dispatch(
            "unit.deleteRecord",
            &json!({ "unit_id": unit_id, "table": "items", "key": "a" }),
            &ctx(PLUGIN),
        )
        .expect("delete a");
    assert_eq!(reply["deleted"], 1);
    let reply = service
        .dispatch(
            "unit.deleteRecord",
            &json!({ "unit_id": unit_id, "table": "items", "key": "a" }),
            &ctx(PLUGIN),
        )
        .expect("delete a again");
    assert_eq!(reply["deleted"], 0, "删不存在的 key 不报错");

    let reply = service
        .dispatch(
            "unit.deleteRecords",
            &json!({ "unit_id": unit_id, "table": "items", "keys": ["b", "missing"] }),
            &ctx(PLUGIN),
        )
        .expect("deleteRecords");
    assert_eq!(reply["deleted"], 1);
    let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
    assert!(snapshot["tables"]["items"].get("b").is_none());
    assert_eq!(snapshot["tables"]["items"]["c"], "c");
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-306：`setGlobal` / `global` 语义；未写时 loadAll 不带 global；
/// `has_global:false` → `facet-unsupported`。
#[test]
fn rpc_l2_306_global_semantics() {
    let (mut service, root) = service("306");
    let unit_id = open_unit(
        &mut service,
        PLUGIN,
        open_args("state", 1, &[], StorageLayout::Single, &["kv"], true),
    )
    .expect("open with global");
    let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
    assert!(
        snapshot["global"].is_null(),
        "未写 global 时返回 null（官方 domain 据此回落到 initial）"
    );

    service
        .dispatch(
            "unit.setGlobal",
            &json!({ "unit_id": unit_id, "value": { "theme": "dark" } }),
            &ctx(PLUGIN),
        )
        .expect("setGlobal");
    let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
    assert_eq!(snapshot["global"]["theme"], "dark");

    let error = service
        .dispatch(
            "unit.setGlobal",
            &json!({ "unit_id": unit_id, "value": null }),
            &ctx(PLUGIN),
        )
        .expect_err("null is the never-written sentinel");
    assert_eq!(error.code, "E_SERVICE_ARGS");

    let no_global = open_unit(
        &mut service,
        PLUGIN,
        open_args("no_global", 1, &[], StorageLayout::Single, &[], false),
    )
    .expect("open without global");
    let error = service
        .dispatch(
            "unit.setGlobal",
            &json!({ "unit_id": no_global, "value": 1 }),
            &ctx(PLUGIN),
        )
        .expect_err("facet unsupported");
    assert_eq!(error.code, "E_INTERNAL");
    assert_eq!(storage_code(&error), "facet-unsupported");
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-307：`loadAll` > 768 KiB → `E_TOO_LARGE`，已有记录不被截断。
#[test]
fn rpc_l2_307_oversized_load_all_is_too_large() {
    let (mut service, root) = service("307");
    let unit_id = open_unit(
        &mut service,
        PLUGIN,
        open_args("big", 1, &[], StorageLayout::Single, &["items"], false),
    )
    .expect("open");
    let payload = "x".repeat(20_000);
    for index in 0..40 {
        put(
            &mut service,
            PLUGIN,
            &unit_id,
            "items",
            &format!("k{index}"),
            json!(payload),
        )
        .expect("put");
    }
    let error = load_all(&mut service, PLUGIN, &unit_id).expect_err("must exceed 768 KiB");
    assert_eq!(error.code, "E_TOO_LARGE");

    let keys: Vec<String> = (0..40).map(|index| format!("k{index}")).collect();
    let reply = service
        .dispatch(
            "unit.deleteRecords",
            &json!({ "unit_id": unit_id, "table": "items", "keys": keys }),
            &ctx(PLUGIN),
        )
        .expect("delete all");
    assert_eq!(reply["deleted"], 40, "超限读取不得截断/丢失记录");
    let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("small again");
    assert!(snapshot["tables"]["items"].as_object().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-308：同 unit 并发 `putRecord` 串行化，不丢写（per-unit mutex + 原子写）。
#[test]
fn rpc_l2_308_concurrent_puts_do_not_lose_writes() {
    let root = temp_root("308");
    let mut handles = Vec::new();
    for thread_index in 0..2 {
        let root = root.clone();
        handles.push(thread::spawn(move || {
            let mut service = StorageKvService::open(&root).expect("open");
            let unit_id = open_unit(
                &mut service,
                PLUGIN,
                open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
            )
            .expect("unit.open");
            for index in 0..40 {
                put(
                    &mut service,
                    PLUGIN,
                    &unit_id,
                    "items",
                    &format!("t{thread_index}-k{index}"),
                    json!(index),
                )
                .expect("put");
            }
        }));
    }
    for handle in handles {
        handle.join().expect("writer thread");
    }

    let mut verifier = StorageKvService::open(&root).expect("open");
    let unit_id = open_unit(
        &mut verifier,
        PLUGIN,
        open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
    )
    .expect("reopen");
    let snapshot = load_all(&mut verifier, PLUGIN, &unit_id).expect("loadAll");
    assert_eq!(
        snapshot["tables"]["items"].as_object().unwrap().len(),
        80,
        "并发写不能丢记录"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-309：跨插件作用域拒绝（同 unit 名各写各的域，不能借 unit_id 越权）。
#[test]
fn rpc_l2_309_cross_plugin_scopes_are_denied() {
    let (mut service, root) = service("309");
    let plugin_a = "example.dsh.tool";
    let plugin_b = "example.other.tool";
    let unit_a = open_unit(
        &mut service,
        plugin_a,
        open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
    )
    .expect("open A");
    put(
        &mut service,
        plugin_a,
        &unit_a,
        "items",
        "a",
        json!("secret"),
    )
    .expect("put A");

    let error = load_all(&mut service, plugin_b, &unit_a).expect_err("must deny");
    assert_eq!(error.code, "E_SERVICE_DENIED");

    let unit_b = open_unit(
        &mut service,
        plugin_b,
        open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
    )
    .expect("open B");
    assert_ne!(unit_a, unit_b, "不同插件作用域必须分配不同 unit");
    let snapshot = load_all(&mut service, plugin_b, &unit_b).expect("loadAll B");
    assert!(snapshot["tables"]["items"].as_object().unwrap().is_empty());
    assert!(
        root.join(plugin_a).join("todos.json").exists()
            && root.join(plugin_b).join("todos.json").exists()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-310：`close` 幂等；关闭后操作 → `closed`；重开/重启后能读回旧数据。
#[test]
fn rpc_l2_310_close_and_restart_keep_data() {
    let (mut service, root) = service("310");
    let unit_id = open_unit(
        &mut service,
        PLUGIN,
        open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
    )
    .expect("open");
    put(&mut service, PLUGIN, &unit_id, "items", "a", json!(1)).expect("put");
    for _ in 0..2 {
        service
            .dispatch("unit.close", &json!({ "unit_id": unit_id }), &ctx(PLUGIN))
            .expect("close is idempotent");
    }
    let error = load_all(&mut service, PLUGIN, &unit_id).expect_err("closed unit");
    assert_eq!(error.code, "E_SERVICE_UNAVAILABLE");
    assert_eq!(storage_code(&error), "closed");

    // 同实例重开：unit_id 不变、数据读回。
    let reopened = open_unit(
        &mut service,
        PLUGIN,
        open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
    )
    .expect("reopen");
    assert_eq!(reopened, unit_id);
    let snapshot = load_all(&mut service, PLUGIN, &unit_id).expect("loadAll");
    assert_eq!(snapshot["tables"]["items"]["a"], 1);
    drop(service);

    // 重启（新 service 实例）后持久化仍在。
    let mut restarted = StorageKvService::open(&root).expect("restart");
    let unit_id = open_unit(
        &mut restarted,
        PLUGIN,
        open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
    )
    .expect("open after restart");
    let snapshot = load_all(&mut restarted, PLUGIN, &unit_id).expect("loadAll");
    assert_eq!(snapshot["tables"]["items"]["a"], 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-311：descriptor/参数非法 → `E_SERVICE_ARGS`；未知 unit → `unknown-state`。
#[test]
fn rpc_l2_311_invalid_descriptor_and_unknown_unit() {
    let (mut service, root) = service("311");
    for args in [
        open_args("Bad-Name", 1, &[], StorageLayout::Single, &[], false),
        open_args("todos", 1, &[1], StorageLayout::Single, &[], false),
        open_args(
            "todos",
            1,
            &[],
            StorageLayout::Single,
            &["Bad-Table"],
            false,
        ),
        json!({ "unit": { "name": "todos", "version": -1 } }),
    ] {
        let error = open_unit(&mut service, PLUGIN, args).expect_err("invalid descriptor");
        assert_eq!(error.code, "E_SERVICE_ARGS", "{error:?}");
    }

    let error = load_all(&mut service, PLUGIN, "unit-999999").expect_err("unknown unit");
    assert_eq!(error.code, "E_SERVICE_UNAVAILABLE");
    assert_eq!(storage_code(&error), "unknown-state");

    let unit_id = open_unit(
        &mut service,
        PLUGIN,
        open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
    )
    .expect("open");
    let error = put(&mut service, PLUGIN, &unit_id, "Bad-Table", "a", json!(1))
        .expect_err("invalid table name");
    assert_eq!(error.code, "E_SERVICE_ARGS");
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-312：同 scope 不同 descriptor 的并发 open 冲突 → `already-open`。
#[test]
fn rpc_l2_312_descriptor_conflict_is_already_open() {
    let (mut service, root) = service("312");
    let args = open_args("todos", 1, &[], StorageLayout::Single, &["items"], false);
    let unit_id = open_unit(&mut service, PLUGIN, args).expect("open");
    let different_layout = open_args("todos", 1, &[], StorageLayout::PerRecord, &["items"], false);
    let error = open_unit(&mut service, PLUGIN, different_layout).expect_err("conflict");
    assert_eq!(error.code, "E_INTERNAL");
    assert_eq!(storage_code(&error), "already-open");

    // 关闭后可用新 descriptor 重开（介质按新 descriptor 对齐）。
    service
        .dispatch("unit.close", &json!({ "unit_id": unit_id }), &ctx(PLUGIN))
        .expect("close");
    let reopened = open_unit(
        &mut service,
        PLUGIN,
        open_args("todos", 1, &[], StorageLayout::PerRecord, &["items"], false),
    )
    .expect("reopen with new layout");
    assert_eq!(reopened, unit_id);
    let _ = std::fs::remove_dir_all(&root);
}

/// RPC-L2-313：经 `HostServiceRouter` 的真实往返（args_schema/白名单/结果上限都生效）。
#[test]
fn rpc_l2_313_round_trips_through_router() {
    let root = temp_root("313");
    let mut router = HostServiceRouter::new(
        "node-0001",
        PROTOCOL_MAX,
        vec![CAP_SERVICE_PROXY.to_owned()],
    )
    .expect("router");
    router
        .register(Box::new(StorageKvService::open(&root).expect("storage")))
        .expect("register storage-kv");
    router.register_plugin_inject(PLUGIN, vec![STORAGE_KV_V1.to_owned()]);

    let call = |request_id: &str, method: &str, args: Value| {
        Envelope::host(
            KIND_HOST_SERVICE_REQUEST,
            request_id,
            json!({ "method": method, "args": args }),
        )
        .with_version(PROTOCOL_MAX)
        .with_generation("node-0001")
        .with_plugin(PLUGIN)
        .with_service_key(STORAGE_KV_V1)
        .with_depth(1)
    };

    let reply = router.handle_request(&call(
        "req-313a",
        "unit.open",
        open_args("todos", 1, &[], StorageLayout::Single, &["items"], false),
    ));
    assert!(reply.error.is_none(), "unit.open failed: {reply:?}");
    let unit_id = reply.payload["result"]["unit_id"]
        .as_str()
        .expect("unit_id")
        .to_owned();

    let reply = router.handle_request(&call(
        "req-313b",
        "unit.putRecord",
        json!({ "unit_id": unit_id, "table": "items", "key": "a", "value": { "n": 1 } }),
    ));
    assert!(reply.error.is_none(), "unit.putRecord failed: {reply:?}");

    let reply = router.handle_request(&call(
        "req-313c",
        "unit.loadAll",
        json!({ "unit_id": unit_id }),
    ));
    assert!(reply.error.is_none(), "unit.loadAll failed: {reply:?}");
    assert_eq!(reply.payload["result"]["tables"]["items"]["a"]["n"], 1);
    let _ = std::fs::remove_dir_all(&root);
}
