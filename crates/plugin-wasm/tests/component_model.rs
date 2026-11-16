//! M4 W5：WASM Component Model 的最小 canonical ABI 探针。
//!
//! 这里只验证宿主能加载/实例化 component，并通过 canonical ABI 调用一个导出函数；
//! 真正的 `cpp-component/v1` 插件 ABI 仍待实现（见 spec/cpp-v1-component.wit）。
//! 用内联 WAT 避免引入 wasm-tools / 预编译 fixture。

#![cfg(test)]

use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};
use wit_parser::{Resolve, WorldKey};

const COMPONENT_WAT: &str = r#"
(component
  (core module $m
    (func (export "run") (result i32)
      i32.const 42))
  (core instance $i (instantiate $m))
  (func (export "run") (result u32)
    (canon lift (core func $i "run")))
)
"#;

/// M4-COMPONENT-001：component-model 能加载、实例化并走 canonical ABI 返回 u32。
#[test]
fn m4_component_001_canonical_abi_round_trip() {
    let engine = Engine::default();
    let component = Component::new(&engine, COMPONENT_WAT).expect("component parses");
    let linker = Linker::new(&engine);
    let mut store = Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component instantiates");
    let run = instance
        .get_typed_func::<(), (u32,)>(&mut store, "run")
        .expect("run export typed");
    let (value,) = run.call(&mut store, ()).expect("canonical call");
    assert_eq!(value, 42);
}

/// M4-COMPONENT-004：component 导出函数受 fuel 配额约束，死循环必须 trap。
#[test]
fn m4_component_004_component_fuel_limit_traps() {
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
    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config).expect("fuel engine");
    let component = Component::new(&engine, LOOP_WAT).expect("component parses");
    let linker = Linker::new(&engine);
    let mut store = Store::new(&engine, ());
    store.set_fuel(10_000).expect("set fuel");
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component instantiates");
    let run = instance
        .get_typed_func::<(), (u32,)>(&mut store, "run")
        .expect("run export typed");
    let result = run.call(&mut store, ());
    assert!(result.is_err(), "infinite loop should exhaust component fuel");
}

/// M4-COMPONENT-005：component 死循环受 epoch 中断约束，外部 increment_epoch 后 trap。
#[test]
fn m4_component_005_component_epoch_interrupts() {
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
    let mut config = Config::new();
    config.epoch_interruption(true);
    let engine = Engine::new(&config).expect("epoch engine");
    let component = Component::new(&engine, LOOP_WAT).expect("component parses");
    let linker = Linker::new(&engine);
    let mut store = Store::new(&engine, ());
    store.set_epoch_deadline(1);
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component instantiates");
    let run = instance
        .get_typed_func::<(), (u32,)>(&mut store, "run")
        .expect("run export typed");
    let ticker = {
        let engine = engine.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            engine.increment_epoch();
        })
    };
    let result = run.call(&mut store, ());
    ticker.join().expect("ticker");
    assert!(result.is_err(), "infinite loop should be epoch-interrupted");
}

/// M4-COMPONENT-006：component 核心线性内存增长受 StoreLimits 约束，超限即 trap。
#[test]
fn m4_component_006_component_memory_limit_traps() {
    const GROW_WAT: &str = r#"
    (component
      (core module $m
        (memory (export "memory") 1)
        (func (export "run") (result i32)
          i32.const 100
          memory.grow
          i32.const -1
          i32.eq
          (if (then unreachable))
          i32.const 42))
      (core instance $i (instantiate $m))
      (func (export "run") (result u32)
        (canon lift (core func $i "run")))
    )
    "#;
    let engine = Engine::default();
    let component = Component::new(&engine, GROW_WAT).expect("component parses");
    let linker = Linker::new(&engine);
    let limits = wasmtime::StoreLimitsBuilder::new()
        .memory_size(64 * 1024)
        .build();
    let mut store = Store::new(&engine, limits);
    store.limiter(|limits| limits);
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component instantiates");
    let run = instance
        .get_typed_func::<(), (u32,)>(&mut store, "run")
        .expect("run export typed");
    let result = run.call(&mut store, ());
    assert!(result.is_err(), "memory.grow over StoreLimits should trap");
}

/// M4-COMPONENT-002：`docs/spec/cpp-v1.wit` 可解析，world `plugin` 导入 host、
/// 导出 tool-provider（W5 契约冻结前的语法/形状校验）。
#[test]
fn m4_component_002_wit_contract_parses() {
    let mut resolve = Resolve::default();
    let package = resolve
        .push_str(
            "cpp-v1.wit",
            include_str!("../../../docs/spec/cpp-v1.wit"),
        )
        .expect("cpp-v1.wit parses");
    let world = resolve
        .select_world(&[package], Some("plugin"))
        .expect("world plugin exists");
    let world = &resolve.worlds[world];
    assert!(
        world.imports.keys().any(|key| match key {
            WorldKey::Name(name) => name == "host",
            WorldKey::Interface(id) => resolve.interfaces[*id].name.as_deref() == Some("host"),
        }),
        "host import missing"
    );
    assert!(
        world.exports.keys().any(|key| match key {
            WorldKey::Name(name) => name == "tool-provider",
            WorldKey::Interface(id) => {
                resolve.interfaces[*id].name.as_deref() == Some("tool-provider")
            }
        }),
        "tool-provider export missing"
    );
}
