#![cfg(test)]

use super::*;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dshr_plugin_api::{capability, Manifest};
use serde_json::json;

// ------------------------------------------------------------------
// 测试辅助
// ------------------------------------------------------------------

fn key(raw: &str) -> ServiceKey {
    ServiceKey::parse(raw).expect("测试里的 service key 必须合法")
}

fn log() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(Vec::new()))
}

fn labels(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    log.lock().unwrap().clone()
}

fn record(log: &Arc<Mutex<Vec<String>>>, entry: &str) -> impl FnOnce() + Send + 'static {
    let log = Arc::clone(log);
    let entry = entry.to_owned();
    move || log.lock().unwrap().push(entry)
}

fn provider(plugin_id: &str, fiber_id: u64, generation: u64) -> ServiceProvider {
    ServiceProvider {
        plugin_id: plugin_id.to_owned(),
        fiber_id,
        generation,
    }
}

fn manifest_with_inject(required: &[&str], optional: &[&str]) -> Manifest {
    let value = json!({
        "plugin": {
            "id": "demo.manifest",
            "version": "0.1.0",
            "api": "cordis-portable/v1"
        },
        "runtime": { "kind": "rust-static" },
        "inject": { "required": required, "optional": optional }
    });
    Manifest::from_json_str(&value.to_string()).expect("测试 manifest 必须可解析")
}

/// 装载一个只发布某个服务的 provider fiber。
fn load_provider(host: &mut FiberHost, plugin_id: &str, raw_key: &str) -> LoadReport {
    let raw_key = raw_key.to_owned();
    host.load(FiberSpec::new(plugin_id), move |ctx| {
        ctx.publish(raw_key.as_str(), Arc::new(format!("value-of-{raw_key}")))?;
        Ok(())
    })
    .expect("provider 应装载成功")
}

fn dependency_consumer(plugin_id: &str, required: &[&str]) -> FiberSpec {
    let mut spec = FiberSpec::new(plugin_id);
    for raw in required {
        spec.add_required(raw).unwrap();
    }
    spec
}

// ------------------------------------------------------------------
// CPP-FIBER-*：状态机与生命周期
// ------------------------------------------------------------------

#[test]
fn cpp_fiber_001_legal_transition_matrix_matches_spec() {
    use FiberState::*;
    let legal = [
        (Pending, Loading),
        (Pending, Unloading),
        (Loading, Active),
        (Loading, Failed),
        (Active, Pending),
        (Active, Unloading),
        (Failed, Pending),
        (Failed, Unloading),
        (Unloading, Disposed),
    ];
    assert_eq!(legal.len(), 9);
    for (from, to) in legal {
        assert!(from.can_transition_to(to), "{from} -> {to} 应合法");
    }
    for from in FiberState::ALL {
        let edges = FiberState::ALL
            .into_iter()
            .filter(|to| from.can_transition_to(*to))
            .count();
        assert_eq!(
            edges,
            if from == Disposed {
                0
            } else {
                legal.iter().filter(|(f, _)| *f == from).count()
            }
        );
    }
}

#[test]
fn cpp_fiber_002_illegal_transitions_are_rejected_without_state_change() {
    use FiberState::*;
    let mut fiber = Fiber::new(7, "demo.state", 1);
    for (from, to) in [
        (Pending, Active),
        (Pending, Disposed),
        (Pending, Pending),
        (Loading, Unloading),
        (Loading, Pending),
        (Active, Loading),
        (Active, Failed),
        (Active, Active),
        (Failed, Active),
        (Unloading, Active),
        (Unloading, Pending),
        (Disposed, Pending),
        (Disposed, Loading),
    ] {
        fiber.state = from;
        let err = fiber.transition(to).expect_err("非法迁移必须报错");
        assert_eq!(
            err,
            PluginError::InvalidTransition {
                from: from.as_str(),
                to: to.as_str(),
            }
        );
        assert_eq!(fiber.state(), from, "非法迁移不得改变状态");
    }
}

#[test]
fn cpp_fiber_003_dependency_pause_and_resume_transitions() {
    let mut fiber = Fiber::new(1, "demo.pause", 1);
    fiber.transition(FiberState::Loading).unwrap();
    fiber.transition(FiberState::Active).unwrap();
    fiber.transition(FiberState::Pending).unwrap();
    assert!(fiber.state().is_stopped());
    // 服务恢复后的最小"自动重启"：Pending -> Loading -> Active（不重跑 apply）。
    fiber.transition(FiberState::Loading).unwrap();
    fiber.transition(FiberState::Active).unwrap();
    assert_eq!(fiber.state(), FiberState::Active);
}

#[test]
fn cpp_fiber_004_pending_can_be_unloaded_into_terminal_state() {
    let mut fiber = Fiber::new(1, "demo.never-loaded", 1);
    assert_eq!(fiber.state(), FiberState::Pending);
    fiber.transition(FiberState::Unloading).unwrap();
    fiber.transition(FiberState::Disposed).unwrap();
    assert!(fiber.state().is_terminal());
    assert!(fiber.transition(FiberState::Loading).is_err());
}

#[test]
fn cpp_fiber_005_state_names_are_stable_and_complete() {
    let names: Vec<&str> = FiberState::ALL.iter().map(|state| state.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "Pending",
            "Loading",
            "Active",
            "Failed",
            "Unloading",
            "Disposed"
        ]
    );
    assert_eq!(FiberState::ALL.len(), 6);
    assert_eq!(FiberState::Disposed.to_string(), "Disposed");
    assert!(FiberState::Disposed.is_terminal());
    assert!(!FiberState::Active.is_terminal());
}

#[test]
fn cpp_fiber_006_load_succeeds_into_active_with_generation() {
    let mut host = FiberHost::new();
    let report = host
        .load(FiberSpec::new("demo.load"), |_ctx| Ok(()))
        .unwrap();
    assert_eq!(report.state, FiberState::Active);
    assert_eq!(report.fiber_id, 1);
    assert_eq!(report.generation, 1);
    assert_eq!(host.fiber_state("demo.load"), Some(FiberState::Active));
    let fiber = host.fiber("demo.load").unwrap();
    assert_eq!(fiber.plugin_id(), "demo.load");
    assert_eq!(fiber.id(), 1);
    assert_eq!(fiber.generation(), 1);
    assert!(fiber.failure().is_none());
    assert!(fiber.last_dispose().is_none());
}

#[test]
fn cpp_fiber_007_duplicate_load_is_rejected_while_active() {
    let mut host = FiberHost::new();
    host.load(FiberSpec::new("demo.dup"), |_ctx| Ok(()))
        .unwrap();
    let err = host
        .load(FiberSpec::new("demo.dup"), |_ctx| Ok(()))
        .unwrap_err();
    assert_eq!(
        err,
        PluginError::AlreadyLoaded {
            plugin_id: "demo.dup".to_owned()
        }
    );
    assert_eq!(host.fiber_count(), 1);
}

#[test]
fn cpp_fiber_008_apply_failure_marks_failed_and_rolls_back_effects_lifo() {
    let mut host = FiberHost::new();
    let log = log();
    let err = host
        .load(FiberSpec::new("demo.broken"), {
            let log = Arc::clone(&log);
            move |ctx| {
                ctx.on_cleanup("first", record(&log, "first"));
                ctx.on_cleanup("second", record(&log, "second"));
                Err(PluginError::Invalid("boom".to_owned()))
            }
        })
        .unwrap_err();
    assert_eq!(err.code(), "E_INTERNAL");
    assert_eq!(labels(&log), vec!["second", "first"], "失败必须逆序回滚");
    let fiber = host.fiber("demo.broken").unwrap();
    assert_eq!(fiber.state(), FiberState::Failed);
    assert_eq!(fiber.failure(), Some(&err));
    assert_eq!(
        fiber.last_dispose().unwrap().labels(),
        vec!["second", "first"]
    );
    assert_eq!(host.fiber_state("demo.broken"), Some(FiberState::Failed));
}

#[test]
fn cpp_fiber_009_unload_disposes_effects_lifo_and_is_terminal() {
    let mut host = FiberHost::new();
    let log = log();
    host.load(FiberSpec::new("demo.unload"), {
        let log = Arc::clone(&log);
        move |ctx| {
            ctx.on_cleanup("a", record(&log, "a"));
            ctx.on_cleanup("b", record(&log, "b"));
            ctx.on_cleanup("c", record(&log, "c"));
            Ok(())
        }
    })
    .unwrap();
    assert!(labels(&log).is_empty(), "Effect 只在卸载/回滚时执行");

    let report = host.unload("demo.unload").unwrap();
    assert_eq!(report.effects.labels(), vec!["c", "b", "a"]);
    assert!(report.effects.all_ok());
    assert_eq!(labels(&log), vec!["c", "b", "a"]);
    assert_eq!(host.fiber_state("demo.unload"), Some(FiberState::Disposed));
    assert_eq!(report.generation, 1);
}

#[test]
fn cpp_fiber_010_second_unload_is_not_loaded_and_does_not_rerun_effects() {
    let mut host = FiberHost::new();
    let log = log();
    host.load(FiberSpec::new("demo.unload-twice"), {
        let log = Arc::clone(&log);
        move |ctx| {
            ctx.on_cleanup("only", record(&log, "only"));
            Ok(())
        }
    })
    .unwrap();
    host.unload("demo.unload-twice").unwrap();
    assert_eq!(labels(&log), vec!["only"]);

    let err = host.unload("demo.unload-twice").unwrap_err();
    assert_eq!(
        err,
        PluginError::NotLoaded {
            plugin_id: "demo.unload-twice".to_owned()
        }
    );
    assert_eq!(labels(&log), vec!["only"], "重复卸载不得重跑 Effect");
    assert_eq!(
        host.unload("demo.never-existed").unwrap_err(),
        PluginError::NotLoaded {
            plugin_id: "demo.never-existed".to_owned()
        }
    );
}

#[test]
fn cpp_fiber_011_reload_after_dispose_gets_new_generation() {
    let mut host = FiberHost::new();
    let first = host
        .load(FiberSpec::new("demo.reload"), |_ctx| Ok(()))
        .unwrap();
    host.unload("demo.reload").unwrap();
    let second = host
        .load(FiberSpec::new("demo.reload"), |_ctx| Ok(()))
        .unwrap();
    assert_eq!(second.generation, first.generation + 1);
    assert_eq!(second.fiber_id, first.fiber_id + 1);
    assert_eq!(host.fiber_state("demo.reload"), Some(FiberState::Active));
}

#[test]
fn cpp_fiber_012_missing_required_service_fails_then_retry_succeeds() {
    let mut host = FiberHost::new();
    let spec = dependency_consumer("demo.needs-storage", &["storage/v1"]);
    let err = host.load(spec, |_ctx| Ok(())).unwrap_err();
    assert_eq!(
        err,
        PluginError::ServiceMissing {
            key: "storage/v1".to_owned()
        }
    );
    assert_eq!(err.code(), "E_SERVICE_MISSING");
    assert!(err.is_retryable());
    let fiber = host.fiber("demo.needs-storage").unwrap();
    assert_eq!(fiber.state(), FiberState::Failed);
    assert_eq!(fiber.failure(), Some(&err));
    let failed_generation = fiber.generation();

    // 服务到位后允许重试（新 generation），无需先卸载 Failed Fiber。
    load_provider(&mut host, "demo.storage", "storage/v1");
    let report = host
        .load(
            dependency_consumer("demo.needs-storage", &["storage/v1"]),
            |_ctx| Ok(()),
        )
        .unwrap();
    assert!(report.generation > failed_generation);
    assert_eq!(
        host.fiber_state("demo.needs-storage"),
        Some(FiberState::Active)
    );
}

#[test]
fn cpp_fiber_013_apply_timeout_rolls_back_and_marks_failed() {
    let mut host = FiberHost::new();
    host.set_apply_timeout(Duration::from_millis(0));
    let log = log();
    let err = host
        .load(FiberSpec::new("demo.slow"), {
            let log = Arc::clone(&log);
            move |ctx| {
                ctx.on_cleanup("rollback", record(&log, "rollback"));
                thread::sleep(Duration::from_millis(5));
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(err.code(), "E_APPLY_TIMEOUT");
    assert!(err.is_retryable());
    assert_eq!(labels(&log), vec!["rollback"]);
    assert_eq!(host.fiber_state("demo.slow"), Some(FiberState::Failed));
}

#[test]
fn cpp_fiber_014_missing_capability_fails_apply_and_rolls_back() {
    let mut host = FiberHost::new(); // 空 capability 集
    let log = log();
    let err = host
        .load(FiberSpec::new("demo.no-cap"), {
            let log = Arc::clone(&log);
            move |ctx| {
                ctx.on_cleanup("rollback", record(&log, "rollback"));
                ctx.require_capability(capability::TOOL_STREAM)?;
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(
        err,
        PluginError::CapabilityMissing {
            capability: capability::TOOL_STREAM.to_owned()
        }
    );
    assert_eq!(err.code(), "E_PERMISSION");
    assert_eq!(labels(&log), vec!["rollback"]);
    assert_eq!(host.fiber_state("demo.no-cap"), Some(FiberState::Failed));
}

#[test]
fn cpp_fiber_015_host_v1_capabilities_allow_apply() {
    let mut host = FiberHost::with_capabilities(CapabilitySet::host_v1());
    let report = host
        .load(FiberSpec::new("demo.with-cap"), |ctx| {
            assert!(ctx.has_capability(capability::UI_CONTRIBUTE));
            ctx.require_capability(capability::UI_CONTRIBUTE)?;
            assert!(!ctx.is_cancelled());
            Ok(())
        })
        .unwrap();
    assert_eq!(report.state, FiberState::Active);
    assert_eq!(host.capabilities().len(), capability::ALL_V1.len());
}

#[test]
fn cpp_fiber_016_load_with_manifest_uses_manifest_id_and_inject() {
    let mut host = FiberHost::new();
    load_provider(&mut host, "demo.storage", "storage/v1");
    let manifest = manifest_with_inject(&["storage/v1"], &["telemetry/v1"]);
    let report = host.load_with_manifest(&manifest, |_ctx| Ok(())).unwrap();
    assert_eq!(report.state, FiberState::Active);
    let fiber = host
        .fiber("demo.manifest")
        .expect("manifest id 即 fiber id");
    assert_eq!(fiber.required_services(), &[key("storage/v1")]);
    assert_eq!(fiber.optional_services(), &[key("telemetry/v1")]);
    assert_eq!(
        host.dependent_plugins(&key("storage/v1")),
        vec!["demo.manifest".to_owned()]
    );
}

#[test]
fn cpp_fiber_017_fiber_spec_from_manifest_validates_service_keys() {
    let manifest = manifest_with_inject(&["storage/v1"], &["optional/v2"]);
    let spec = FiberSpec::from_manifest(&manifest).unwrap();
    assert_eq!(spec.plugin_id(), "demo.manifest");
    assert_eq!(spec.required(), &[key("storage/v1")]);
    assert_eq!(spec.optional(), &[key("optional/v2")]);

    let bad = manifest_with_inject(&["Bad Key"], &[]);
    let err = FiberSpec::from_manifest(&bad).unwrap_err();
    assert_eq!(err.code(), "E_MANIFEST");
    assert!(matches!(err, PluginError::InvalidServiceKey { .. }));
}

#[test]
fn cpp_fiber_018_load_rejects_manifest_service_missing_before_apply_runs() {
    let mut host = FiberHost::new();
    let manifest = manifest_with_inject(&["storage/v1"], &[]);
    let mut applied = false;
    let err = host
        .load_with_manifest(&manifest, |_ctx| {
            applied = true;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(err.code(), "E_SERVICE_MISSING");
    assert!(!applied, "必需服务缺失时不得执行 apply");
}

#[test]
fn cpp_fiber_019_fiber_records_public_accessors() {
    let mut host = FiberHost::new();
    load_provider(&mut host, "demo.provider", "kv/v1");
    let fiber = host.fiber("demo.provider").unwrap();
    assert_eq!(fiber.published_services(), &[key("kv/v1")]);
    assert_eq!(fiber.effects().len(), 0);
    assert_eq!(fiber.resumed_count(), 0);
    assert!(fiber.pause_reason().is_none());
    assert!(host.fiber("nope").is_none());
    assert_eq!(host.fiber_state("nope"), None);
    assert_eq!(host.fibers().count(), 1);
}

#[test]
fn cpp_fiber_020_loading_and_active_fibers_block_reload() {
    let mut host = FiberHost::new();
    host.load(FiberSpec::new("demo.block"), |_ctx| Ok(()))
        .unwrap();
    assert!(matches!(
        host.load(FiberSpec::new("demo.block"), |_ctx| Ok(())),
        Err(PluginError::AlreadyLoaded { .. })
    ));
    // Failed 状态允许重试（见 CPP-FIBER-012），Disposed 允许重装（见 CPP-FIBER-011）。
}

#[test]
fn cpp_fiber_021_apply_can_publish_services_and_inspect_optional_ones() {
    let mut host = FiberHost::new();
    let report = host
        .load(FiberSpec::new("demo.publish"), |ctx| {
            let published = ctx.publish("tool-registry/v1", Arc::new(9u32))?;
            assert_eq!(published.as_str(), "tool-registry/v1");
            assert!(ctx.try_get::<String>("storage/v1").is_none());
            assert_eq!(ctx.fiber_id(), 1);
            assert_eq!(ctx.generation(), 1);
            assert_eq!(ctx.plugin_id(), "demo.publish");
            let _ = ctx.on_cleanup("noop", || {});
            Ok(())
        })
        .unwrap();
    assert_eq!(report.state, FiberState::Active);
    assert!(host.services().contains(&key("tool-registry/v1")));
    assert_eq!(
        host.services().get::<u32>(&key("tool-registry/v1")),
        Some(Arc::new(9))
    );
    assert_eq!(
        host.services().provider(&key("tool-registry/v1")).unwrap(),
        &provider("demo.publish", 1, 1)
    );
}

#[test]
fn cpp_fiber_022_cancellation_token_is_exposed_to_apply() {
    let mut host = FiberHost::new();
    host.load(FiberSpec::new("demo.cancel"), |ctx| {
        assert!(!ctx.cancellation().is_cancelled(), "装载期间未取消");
        ctx.cancellation().cancel();
        assert!(ctx.is_cancelled());
        Ok(())
    })
    .unwrap();
    assert_eq!(host.fiber_state("demo.cancel"), Some(FiberState::Active));
}

// ------------------------------------------------------------------
// CPP-SVC-*：Service key / 服务表 / 依赖暂停恢复
// ------------------------------------------------------------------

#[test]
fn cpp_svc_001_service_key_parses_name_and_major() {
    let parsed = ServiceKey::parse("tool-registry/v12").unwrap();
    assert_eq!(parsed.name(), "tool-registry");
    assert_eq!(parsed.major(), 12);
    assert_eq!(parsed.as_str(), "tool-registry/v12");
    assert_eq!(parsed.to_string(), "tool-registry/v12");
    assert_eq!(
        "storage/v1".parse::<ServiceKey>().unwrap(),
        key("storage/v1")
    );
}

#[test]
fn cpp_svc_002_service_key_rejects_invalid_shapes() {
    for raw in [
        "",
        "storage",
        "storage/v",
        "storage/vX",
        "storage/v-1",
        "Storage/v1",
        "stor age/v1",
        "storage/v1/extra",
        "storage/v99999999999999999999",
    ] {
        let err = ServiceKey::parse(raw).expect_err("非法 key 必须拒绝");
        assert!(
            matches!(err, PluginError::InvalidServiceKey { .. }),
            "`{raw}` 应返回 InvalidServiceKey"
        );
        assert_eq!(err.code(), "E_MANIFEST");
    }
}

#[test]
fn cpp_svc_003_publish_get_and_type_erase() {
    let mut table = ServiceTable::new();
    assert!(table.is_empty());
    table
        .publish(
            key("storage/v1"),
            Arc::new(String::from("persisted")),
            provider("demo.p", 1, 1),
        )
        .unwrap();
    assert_eq!(table.len(), 1);
    assert!(table.contains(&key("storage/v1")));
    assert_eq!(
        table.get::<String>(&key("storage/v1")).as_deref(),
        Some(&String::from("persisted"))
    );
    assert!(table.get::<u32>(&key("storage/v1")).is_none());
    assert!(table.get_erased(&key("storage/v1")).is_some());
    assert!(table.get::<String>(&key("missing/v1")).is_none());
    assert_eq!(
        table.keys().map(ServiceKey::as_str).collect::<Vec<_>>(),
        vec!["storage/v1"]
    );
}

#[test]
fn cpp_svc_004_duplicate_key_is_conflict() {
    let mut table = ServiceTable::new();
    let owner = provider("demo.a", 1, 1);
    table
        .publish(key("storage/v1"), Arc::new(1u32), owner.clone())
        .unwrap();
    let err = table
        .publish(key("storage/v1"), Arc::new(2u32), owner)
        .unwrap_err();
    assert_eq!(
        err,
        PluginError::ServiceConflict {
            key: "storage/v1".to_owned()
        }
    );
    assert_eq!(err.code(), "E_SERVICE_CONFLICT");
    assert!(!err.is_retryable());
    assert_eq!(table.get::<u32>(&key("storage/v1")), Some(Arc::new(1)));
}

#[test]
fn cpp_svc_005_revoke_is_idempotent() {
    let mut table = ServiceTable::new();
    table
        .publish(key("kv/v1"), Arc::new(()), provider("demo.a", 1, 1))
        .unwrap();
    assert!(table.revoke(&key("kv/v1")));
    assert!(!table.revoke(&key("kv/v1")));
    assert!(table.is_empty());
}

#[test]
fn cpp_svc_006_revoke_if_owned_only_removes_matching_provider() {
    let mut table = ServiceTable::new();
    table
        .publish(key("kv/v1"), Arc::new(()), provider("demo.a", 1, 1))
        .unwrap();
    assert!(!table.revoke_if_owned(&key("kv/v1"), &provider("demo.b", 2, 2)));
    assert!(table.contains(&key("kv/v1")));
    assert!(table.revoke_if_owned(&key("kv/v1"), &provider("demo.a", 1, 1)));
    assert!(table.is_empty());
}

#[test]
fn cpp_svc_007_apply_failure_rolls_back_published_services() {
    let mut host = FiberHost::new();
    let err = host
        .load(FiberSpec::new("demo.bad-provider"), |ctx| {
            ctx.publish("kv/v1", Arc::new(1u32))?;
            Err(PluginError::Invalid("boom".to_owned()))
        })
        .unwrap_err();
    assert_eq!(err.code(), "E_INTERNAL");
    assert!(host.services().is_empty(), "失败装载不得留下服务");
    assert_eq!(
        host.fiber_state("demo.bad-provider"),
        Some(FiberState::Failed)
    );
}

#[test]
fn cpp_svc_008_unload_revokes_published_services() {
    let mut host = FiberHost::new();
    host.load(FiberSpec::new("demo.unpublish"), |ctx| {
        ctx.publish("a/v1", Arc::new(1u32))?;
        ctx.publish("b/v1", Arc::new(2u32))?;
        Ok(())
    })
    .unwrap();
    let report = host.unload("demo.unpublish").unwrap();
    assert_eq!(
        report.revoked_services,
        vec![key("a/v1"), key("b/v1")],
        "撤销顺序应与发布顺序一致（服务发布不是 Effect）"
    );
    assert!(host.services().is_empty());
}

#[test]
fn cpp_svc_009_revoke_service_pauses_dependent_fiber() {
    let mut host = FiberHost::new();
    load_provider(&mut host, "demo.store", "storage/v1");
    host.load(
        dependency_consumer("demo.reader", &["storage/v1"]),
        |_ctx| Ok(()),
    )
    .unwrap();
    assert_eq!(host.fiber_state("demo.reader"), Some(FiberState::Active));

    let paused = host.revoke_service(&key("storage/v1"));
    assert_eq!(paused, vec!["demo.reader".to_owned()]);
    assert_eq!(host.fiber_state("demo.reader"), Some(FiberState::Pending));
    assert_eq!(
        host.fiber("demo.reader").unwrap().pause_reason(),
        Some(&PauseReason::ServiceMissing(key("storage/v1")))
    );
    assert_eq!(
        host.dependent_plugins(&key("storage/v1")),
        vec!["demo.reader".to_owned()]
    );
}

#[test]
fn cpp_svc_010_provider_unload_pauses_and_republish_resumes_dependent() {
    let mut host = FiberHost::new();
    load_provider(&mut host, "demo.store", "storage/v1");
    host.load(
        dependency_consumer("demo.reader", &["storage/v1"]),
        |_ctx| Ok(()),
    )
    .unwrap();

    let report = host.unload("demo.store").unwrap();
    assert_eq!(report.paused_dependents, vec!["demo.reader".to_owned()]);
    assert_eq!(host.fiber_state("demo.reader"), Some(FiberState::Pending));

    // 服务恢复 -> 依赖 Fiber 自动回到 Active（最小切片：状态迁移，不重跑 apply）。
    load_provider(&mut host, "demo.store", "storage/v1");
    assert_eq!(host.fiber_state("demo.reader"), Some(FiberState::Active));
    assert_eq!(host.fiber("demo.reader").unwrap().resumed_count(), 1);
    assert!(host.fiber("demo.reader").unwrap().pause_reason().is_none());
}

#[test]
fn cpp_svc_011_resume_requires_all_required_services() {
    let mut host = FiberHost::new();
    load_provider(&mut host, "demo.a", "a/v1");
    load_provider(&mut host, "demo.b", "b/v1");
    host.load(
        dependency_consumer("demo.both", &["a/v1", "b/v1"]),
        |_ctx| Ok(()),
    )
    .unwrap();
    assert_eq!(host.fiber_state("demo.both"), Some(FiberState::Active));

    host.unload("demo.a").unwrap();
    host.unload("demo.b").unwrap();
    assert_eq!(host.fiber_state("demo.both"), Some(FiberState::Pending));

    load_provider(&mut host, "demo.a", "a/v1");
    assert_eq!(
        host.fiber_state("demo.both"),
        Some(FiberState::Pending),
        "只恢复一个服务时不得激活"
    );
    load_provider(&mut host, "demo.b", "b/v1");
    assert_eq!(host.fiber_state("demo.both"), Some(FiberState::Active));
}

#[test]
fn cpp_svc_012_optional_service_missing_does_not_block() {
    let mut host = FiberHost::new();
    let mut spec = FiberSpec::new("demo.optional");
    spec.add_optional("telemetry/v1").unwrap();
    let mut saw_optional = false;
    host.load(spec, |ctx| {
        saw_optional = ctx.try_get::<String>("telemetry/v1").is_none();
        Ok(())
    })
    .unwrap();
    assert!(saw_optional);
    assert_eq!(host.fiber_state("demo.optional"), Some(FiberState::Active));
}

#[test]
fn cpp_svc_013_dependency_edges_are_cleared_on_unload() {
    let mut host = FiberHost::new();
    load_provider(&mut host, "demo.store", "storage/v1");
    host.load(
        dependency_consumer("demo.reader", &["storage/v1"]),
        |_ctx| Ok(()),
    )
    .unwrap();
    assert_eq!(
        host.dependent_plugins(&key("storage/v1")),
        vec!["demo.reader".to_owned()]
    );

    host.unload("demo.reader").unwrap();
    assert!(host.dependent_plugins(&key("storage/v1")).is_empty());
    assert!(host.services().contains(&key("storage/v1")));
}

#[test]
fn cpp_svc_014_same_fiber_cannot_publish_same_key_twice() {
    let mut host = FiberHost::new();
    let err = host
        .load(FiberSpec::new("demo.twice"), |ctx| {
            ctx.publish("kv/v1", Arc::new(1u32))?;
            ctx.publish("kv/v1", Arc::new(2u32))?;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(err.code(), "E_SERVICE_CONFLICT");
    assert!(host.services().is_empty(), "失败装载必须回滚已发布服务");
}

// ------------------------------------------------------------------
// CPP-EFF-*：Effect 栈（LIFO / 幂等 / 失败与超时隔离）
// ------------------------------------------------------------------

#[test]
fn cpp_eff_001_dispose_runs_strictly_lifo() {
    let mut stack = EffectStack::new();
    let log = log();
    stack.push("first", record(&log, "first"));
    stack.push("second", record(&log, "second"));
    stack.push("third", record(&log, "third"));
    assert_eq!(stack.labels(), vec!["first", "second", "third"]);
    assert_eq!(stack.len(), 3);

    let report = stack.dispose();
    assert_eq!(labels(&log), vec!["third", "second", "first"]);
    assert_eq!(report.labels(), vec!["third", "second", "first"]);
    assert!(report.all_ok());
    assert_eq!(report.failed(), 0);
    assert_eq!(report.timed_out(), 0);
}

#[test]
fn cpp_eff_002_dispose_is_idempotent() {
    let mut stack = EffectStack::new();
    let log = log();
    stack.push("only", record(&log, "only"));
    assert!(!stack.is_disposed());
    let first = stack.dispose();
    assert_eq!(first.len(), 1);
    assert!(stack.is_disposed());
    assert!(stack.is_empty());

    let second = stack.dispose();
    assert!(second.is_empty(), "第二次 dispose 不得重复执行");
    assert_eq!(labels(&log), vec!["only"]);
}

#[test]
fn cpp_eff_003_single_panic_does_not_interrupt_cleanup() {
    let mut stack = EffectStack::new();
    let log = log();
    stack.push("before", record(&log, "before"));
    stack.push("panics", || panic!("cleanup exploded"));
    stack.push("after", record(&log, "after"));

    let report = stack.dispose();
    assert_eq!(
        labels(&log),
        vec!["after", "before"],
        "panic 之后的 Effect 仍必须执行"
    );
    assert_eq!(report.labels(), vec!["after", "panics", "before"]);
    assert_eq!(report.failed(), 1);
    assert!(!report.all_ok());
    match &report.entries()[1].outcome {
        EffectOutcome::Panicked(message) => assert!(message.contains("cleanup exploded")),
        other => panic!("预期 Panicked，实际 {other:?}"),
    }
}

#[test]
fn cpp_eff_004_single_timeout_is_recorded_and_cleanup_continues() {
    let mut stack = EffectStack::with_timeout(Duration::from_millis(10));
    let log = log();
    stack.push("slow", {
        let log = Arc::clone(&log);
        move || {
            thread::sleep(Duration::from_millis(250));
            log.lock().unwrap().push("slow".to_owned());
        }
    });
    stack.push("quick", record(&log, "quick"));

    let report = stack.dispose();
    assert_eq!(report.timed_out(), 1);
    assert_eq!(report.labels(), vec!["quick", "slow"]);
    assert_eq!(labels(&log), vec!["quick"], "超时后必须继续清理下一个");
    assert!(report.entries()[1].outcome.is_timed_out());
}

#[test]
fn cpp_eff_005_effect_ids_and_registration_labels_are_recorded() {
    let mut stack = EffectStack::new();
    let first = stack.push("a", || {});
    let second = stack.push("b", || {});
    assert!(second > first);
    assert_eq!(stack.labels(), vec!["a", "b"]);
    let report = stack.dispose();
    assert_eq!(report.entries()[0].id, second);
    assert_eq!(report.entries()[0].label, "b");
    assert_eq!(report.entries()[1].id, first);
}

#[test]
fn cpp_eff_006_empty_dispose_is_ok() {
    let mut stack = EffectStack::new();
    let report = stack.dispose();
    assert!(report.is_empty());
    assert!(report.all_ok());
    assert!(stack.is_disposed());
}

#[test]
fn cpp_eff_007_timeout_is_configurable() {
    let mut stack = EffectStack::with_timeout(Duration::from_millis(50));
    assert_eq!(stack.timeout(), Duration::from_millis(50));
    stack.set_timeout(Duration::from_millis(75));
    assert_eq!(stack.timeout(), Duration::from_millis(75));
    // 默认值来自 spec §6.3。
    assert_eq!(
        EffectStack::new().timeout(),
        Duration::from_millis(EFFECT_DISPOSE_TIMEOUT_MS)
    );
}

#[test]
fn cpp_eff_008_apply_failure_dispose_report_is_kept_on_fiber() {
    let mut host = FiberHost::new();
    let log = log();
    let _ = host
        .load(FiberSpec::new("demo.report"), {
            let log = Arc::clone(&log);
            move |ctx| {
                ctx.on_cleanup("one", record(&log, "one"));
                ctx.on_cleanup("two", record(&log, "two"));
                Err(PluginError::Invalid("rollback".to_owned()))
            }
        })
        .unwrap_err();
    let report = host
        .fiber("demo.report")
        .unwrap()
        .last_dispose()
        .expect("失败装载必须留下回滚报告");
    assert!(report.all_ok());
    assert_eq!(report.labels(), vec!["two", "one"]);
    assert_eq!(labels(&log), vec!["two", "one"]);
}

#[test]
fn cpp_eff_009_unload_report_is_all_ok_for_cooperative_effects() {
    let mut host = FiberHost::new();
    host.load(FiberSpec::new("demo.effects"), |ctx| {
        ctx.on_cleanup("x", || {});
        ctx.on_cleanup("y", || {});
        Ok(())
    })
    .unwrap();
    let report = host.unload("demo.effects").unwrap();
    assert_eq!(report.effects.len(), 2);
    assert!(report.effects.all_ok());
    assert_eq!(report.effects.labels(), vec!["y", "x"]);
}

#[test]
fn cpp_eff_010_effects_of_different_fibers_are_independent() {
    let mut host = FiberHost::new();
    let log = log();
    host.load(FiberSpec::new("demo.one"), {
        let log = Arc::clone(&log);
        move |ctx| {
            ctx.on_cleanup("one", record(&log, "one"));
            Ok(())
        }
    })
    .unwrap();
    host.load(FiberSpec::new("demo.two"), {
        let log = Arc::clone(&log);
        move |ctx| {
            ctx.on_cleanup("two", record(&log, "two"));
            Ok(())
        }
    })
    .unwrap();

    host.unload("demo.one").unwrap();
    assert_eq!(labels(&log), vec!["one"], "不得触碰其它 Fiber 的 Effect");
    host.unload("demo.two").unwrap();
    assert_eq!(labels(&log), vec!["one", "two"]);
}
