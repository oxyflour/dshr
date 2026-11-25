use crate::*;

/// 自检：external-process 插件产品路径（安装 → profile 装载 → 工具调用 → 卸载 → 重载）。
/// external-process fixture 插件的一次性准备（复制 exe → 写 manifest/包摘要 → 信任安装）。
///
/// `--selftest-external-plugin` 与 `--selftest-adc-panel` 共用；失败已按 `tag` 前缀打印，
/// 调用方直接返回 1 即可。
struct FixturePluginSetup {
    profile: Profile,
    sidecar: PathBuf,
    installed: bool,
}

fn prepare_fixture_plugin(
    root: &std::path::Path,
    plugin_id: &str,
    tag: &str,
) -> Option<FixturePluginSetup> {
    let _ = std::fs::remove_dir_all(root);
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if std::fs::create_dir_all(&package).is_err() {
        eprintln!("[{tag}] 创建目录失败");
        return None;
    }
    let fixture_name = if cfg!(windows) {
        "plugin-fixture.exe"
    } else {
        "plugin-fixture"
    };
    let Some(fixture) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(fixture_name)))
    else {
        eprintln!("[{tag}] 无法定位 fixture 可执行文件");
        return None;
    };
    if !fixture.is_file() {
        eprintln!(
            "[{tag}] 缺少 {}；先运行 `cargo build -p dshr-plugin-host --bins`",
            fixture.display()
        );
        return None;
    }
    if std::fs::copy(&fixture, package.join(fixture_name)).is_err() {
        eprintln!("[{tag}] 复制 fixture 失败");
        return None;
    }
    let manifest = format!(
        r#"
[plugin]
id = "{plugin_id}"
version = "0.1.0"
api = "cordis-portable/v1"
description = "external-process selftest fixture"

[runtime]
kind = "external-process"
entry = "{fixture_name}"
max_memory_mb = 128
timeout_ms = 60000

[permissions]
workspace_read = false
workspace_write = false
network = false
"#
    );
    let package_json = json!({ "name": plugin_id, "version": "0.1.0" });
    let _ = std::fs::write(package.join("plugin.toml"), manifest);
    let _ = std::fs::write(package.join("package.json"), package_json.to_string());
    let Ok(digest) = package_digest(&package) else {
        eprintln!("[{tag}] 计算包摘要失败");
        return None;
    };
    let trust = json!({
        "schema_version": 1,
        "plugin_id": plugin_id,
        "version": "0.1.0",
        "package": { "digest_sha256": digest }
    });
    let _ = std::fs::write(package.join("dshr-trust.json"), trust.to_string());

    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[{tag}] 加载 profile 失败: {err}");
            return None;
        }
    };
    let policy = TrustPolicy {
        allow_downgrade: false,
        require_confirm_for_unsigned: true,
        store: TrustStore::default(),
        now_ms: None,
    };
    let install = match install_package_trusted(&sidecar, &package, &mut profile, &policy, |_| true)
    {
        Ok(report) => report,
        Err(err) => {
            eprintln!("[{tag}] 安装失败（{}）: {err}", err.code());
            return None;
        }
    };
    let installed = install.install.destination.join("plugin.toml").is_file();
    Some(FixturePluginSetup {
        profile,
        sidecar,
        installed,
    })
}

pub(crate) fn selftest_external_plugin() -> i32 {
    let root = PathBuf::from(".tmp/external-plugin-selftest");
    let Some(setup) = prepare_fixture_plugin(&root, "selftest.external", "selftest-external")
    else {
        return 1;
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let approvals = ApprovalBroker::new();
    let mut state = ChatState::open_diagnostics_with_options(
        root.join("session.jsonl"),
        SESSION_ID,
        &cancel,
        approvals,
        false,
        false,
    );
    let errors =
        load_enabled_external_plugins(&mut state.plugins, &setup.profile, &[&setup.sidecar]);
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("[selftest-external] {error}");
        }
        println!("[selftest-external] FAIL（装载失败）");
        return 1;
    }
    if !state.plugins.is_loaded("selftest.external") {
        println!("[selftest-external] FAIL（插件未装载）");
        return 1;
    }
    let echo = state.plugins.call("echo", &json!({ "text": "hi" }));
    let echo_ok = echo
        .as_ref()
        .map(|value| value.to_string().contains("hi"))
        .unwrap_or(false);
    let unloaded = state.plugins.unload("selftest.external").is_ok();
    let reload_errors =
        load_enabled_external_plugins(&mut state.plugins, &setup.profile, &[&setup.sidecar]);
    let reload_ok = reload_errors.is_empty()
        && state.plugins.is_loaded("selftest.external")
        && state
            .plugins
            .call("echo", &json!({ "text": "again" }))
            .map(|value| value.to_string().contains("again"))
            .unwrap_or(false);
    // 面板声明也随 ready 注册（ADC panel 的宿主侧元数据）。
    let panel_ok = state
        .plugins
        .panels()
        .iter()
        .any(|panel| panel.plugin_id == "selftest.external");
    let installed_ok = setup.installed;
    let ok = echo_ok && unloaded && reload_ok && panel_ok && installed_ok;
    println!(
        "[selftest-external] installed_ok={installed_ok} echo_ok={echo_ok} unloaded={unloaded} reload_ok={reload_ok} panel_ok={panel_ok}"
    );
    if ok {
        println!("[selftest-external] PASS");
        0
    } else {
        println!("[selftest-external] FAIL");
        1
    }
}

/// `--selftest-cpp-component`：把真实 wasm32-wasip2 guest 写进临时包，
/// 经 manifest loader 注册 `echo` 并调用，验证产品侧 cpp-v1 装载路径。
pub(crate) fn selftest_cpp_component() -> i32 {
    const GUEST: &[u8] = include_bytes!(
        "../../../../../../crates/plugin-wasm/tests/fixtures/cpp-component-guest.wasm"
    );
    let root = data_root().join("selftests").join("cpp-component");
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(&root) {
        eprintln!(
            "[selftest-cpp-component] FAIL：创建临时目录 {} 失败: {error}",
            root.display()
        );
        return 1;
    }
    if let Err(error) = std::fs::write(root.join("plugin.wasm"), GUEST) {
        eprintln!("[selftest-cpp-component] FAIL：写 guest 失败: {error}");
        return 1;
    }
    let manifest_raw = serde_json::json!({
        "plugin": {
            "id": "selftest.component",
            "version": "0.1.0",
            "api": "cordis-portable/v1"
        },
        "runtime": {
            "kind": "wasm-component",
            "entry": "plugin.wasm",
            "timeout_ms": 30000
        },
        "tools": []
    })
    .to_string();
    let manifest = match Manifest::from_json_str(&manifest_raw) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("[selftest-cpp-component] FAIL：manifest: {error}");
            return 1;
        }
    };
    let mut plugins = PluginHost::new(AllowAll);
    if let Err(error) =
        plugins.load_cpp_component_plugin_from_manifest(&manifest, &root, &host_sdk_version())
    {
        eprintln!("[selftest-cpp-component] FAIL：装载: {error}");
        return 1;
    }
    if plugins.registry().get("echo").is_none() {
        eprintln!("[selftest-cpp-component] FAIL：guest 未注册 echo");
        return 1;
    }
    let value = match plugins.call_approved("echo", &serde_json::json!({ "text": "hello" })) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("[selftest-cpp-component] FAIL：调用: {error}");
            return 1;
        }
    };
    if value["content"][0]["text"] != "{\"text\":\"hello\"}" {
        eprintln!("[selftest-cpp-component] FAIL：结果: {value}");
        return 1;
    }
    println!("[selftest-cpp-component] PASS registry=echo value={value}");
    let _ = std::fs::remove_dir_all(&root);
    0
}

/// `--selftest-cpp-install`：走真实 `install_package` 复制到 sidecar 目录，
/// 再从 profile 装载 cpp-v1 component，验证安装 → profile → loader 链路。
pub(crate) fn selftest_cpp_install() -> i32 {
    const GUEST: &[u8] = include_bytes!(
        "../../../../../../crates/plugin-wasm/tests/fixtures/cpp-component-guest.wasm"
    );
    let root = data_root().join("selftests").join("cpp-install");
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(root.join("pkg")) {
        eprintln!("[selftest-cpp-install] FAIL：创建目录: {error}");
        return 1;
    }
    let package = root.join("pkg");
    if let Err(error) = std::fs::write(package.join("plugin.wasm"), GUEST) {
        eprintln!("[selftest-cpp-install] FAIL：写 guest: {error}");
        return 1;
    }
    let manifest_raw = r#"
[plugin]
id = "selftest.installed"
version = "0.1.0"
api = "cordis-portable/v1"
description = "cpp component selftest"

[runtime]
kind = "wasm-component"
entry = "plugin.wasm"
timeout_ms = 30000
"#;
    if let Err(error) = std::fs::write(package.join("plugin.toml"), manifest_raw) {
        eprintln!("[selftest-cpp-install] FAIL：写 plugin.toml: {error}");
        return 1;
    }
    let package_json = serde_json::json!({
        "name": "selftest.installed",
        "version": "0.1.0",
        "private": true
    })
    .to_string();
    if let Err(error) = std::fs::write(package.join("package.json"), package_json) {
        eprintln!("[selftest-cpp-install] FAIL：写 package.json: {error}");
        return 1;
    }
    let profile_dir = root.join("profile");
    let sidecar_dir = root.join("sidecar");
    if let Err(error) = std::fs::create_dir_all(&sidecar_dir) {
        eprintln!("[selftest-cpp-install] FAIL：创建 sidecar: {error}");
        return 1;
    }
    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("[selftest-cpp-install] FAIL：profile: {error}");
            return 1;
        }
    };
    if let Err(error) = install_package(&sidecar_dir, &package, &mut profile) {
        eprintln!("[selftest-cpp-install] FAIL：安装: {error}");
        return 1;
    }
    let mut plugins = PluginHost::new(AllowAll);
    let errors = load_enabled_external_plugins(&mut plugins, &profile, &[&sidecar_dir]);
    if !errors.is_empty() {
        eprintln!("[selftest-cpp-install] FAIL：装载: {errors:?}");
        return 1;
    }
    let value = match plugins.call_approved("echo", &serde_json::json!({ "text": "installed" })) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("[selftest-cpp-install] FAIL：调用: {error}");
            return 1;
        }
    };
    if value["content"][0]["text"] != "{\"text\":\"installed\"}" {
        eprintln!("[selftest-cpp-install] FAIL：结果: {value}");
        return 1;
    }
    println!("[selftest-cpp-install] PASS value={value}");
    let _ = std::fs::remove_dir_all(&root);
    0
}

/// `--selftest-cpp-cli-install`：走真实 `install_plugin_with_options`（信任策略 +
/// 预览确认 + profile 写入），再从 profile 装载并调用 component。
pub(crate) fn selftest_cpp_cli_install() -> i32 {
    const GUEST: &[u8] = include_bytes!(
        "../../../../../../crates/plugin-wasm/tests/fixtures/cpp-component-guest.wasm"
    );
    let root = data_root().join("selftests").join("cpp-cli-install");
    let _ = std::fs::remove_dir_all(&root);
    let package = root.join("pkg");
    if let Err(error) = std::fs::create_dir_all(&package) {
        eprintln!("[selftest-cpp-cli-install] FAIL：创建目录: {error}");
        return 1;
    }
    if let Err(error) = std::fs::write(package.join("plugin.wasm"), GUEST) {
        eprintln!("[selftest-cpp-cli-install] FAIL：写 guest: {error}");
        return 1;
    }
    let manifest_raw = r#"
[plugin]
id = "selftest.cli.installed"
version = "0.1.0"
api = "cordis-portable/v1"
description = "cpp component CLI install selftest"

[runtime]
kind = "wasm-component"
entry = "plugin.wasm"
timeout_ms = 30000
"#;
    if let Err(error) = std::fs::write(package.join("plugin.toml"), manifest_raw) {
        eprintln!("[selftest-cpp-cli-install] FAIL：写 plugin.toml: {error}");
        return 1;
    }
    let package_json = serde_json::json!({
        "name": "selftest.cli.installed",
        "version": "0.1.0",
        "private": true
    })
    .to_string();
    if let Err(error) = std::fs::write(package.join("package.json"), package_json) {
        eprintln!("[selftest-cpp-cli-install] FAIL：写 package.json: {error}");
        return 1;
    }
    let sidecar_dir = root.join("sidecar");
    let profile_dir = root.join("profile");
    let options = InstallCliOptions {
        package: package.clone(),
        yes_install: true,
        allow_downgrade: false,
    };
    if install_plugin_with_options(&options, &sidecar_dir, &profile_dir) != 0 {
        eprintln!("[selftest-cpp-cli-install] FAIL：install_plugin_with_options");
        return 1;
    }
    let profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("[selftest-cpp-cli-install] FAIL：profile: {error}");
            return 1;
        }
    };
    let mut plugins = PluginHost::new(AllowAll);
    let errors = load_enabled_external_plugins(&mut plugins, &profile, &[&sidecar_dir]);
    if !errors.is_empty() {
        eprintln!("[selftest-cpp-cli-install] FAIL：装载: {errors:?}");
        return 1;
    }
    let value = match plugins.call_approved("echo", &serde_json::json!({ "text": "cli" })) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("[selftest-cpp-cli-install] FAIL：调用: {error}");
            return 1;
        }
    };
    if value["content"][0]["text"] != "{\"text\":\"cli\"}" {
        eprintln!("[selftest-cpp-cli-install] FAIL：结果: {value}");
        return 1;
    }
    println!("[selftest-cpp-cli-install] PASS value={value}");
    let _ = std::fs::remove_dir_all(&root);
    0
}

pub(crate) fn selftest_wasm() -> i32 {
    let root = data_root().join("selftests").join("wasm-host");
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(&root) {
        eprintln!(
            "[selftest-wasm] FAIL：创建临时目录 {} 失败: {error}",
            root.display()
        );
        return 1;
    }
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if let Err(error) = std::fs::create_dir_all(package.join("schemas")) {
        eprintln!("[selftest-wasm] FAIL：创建包目录失败: {error}");
        return 1;
    }
    let _ = std::fs::create_dir_all(&sidecar);

    // 1) WAT fixture：'e'=echo 回显；'u'=ui.render 返回 data 段元素树；其它（trap）=死循环。
    let wat = r#"
(module
  (memory (export "memory") 1)
  (data (i32.const 1024) "{\"type\":\"Text\",\"props\":{\"text\":\"wasm\"}}")
  (global $heap (mut i32) (i32.const 4096))
  (func (export "dshr_alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap))
    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
    (local.get $ptr))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (local $tag i32)
    (local.set $tag (i32.load8_u offset=9 (local.get $ptr)))
    (if (i32.eq (local.get $tag) (i32.const 117))
      (then (return (i64.or
        (i64.shl (i64.extend_i32_u (i32.const 1024)) (i64.const 32))
        (i64.const 39)))))
    (if (i32.eq (local.get $tag) (i32.const 101))
      (then (return (i64.or
        (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
        (i64.extend_i32_u (local.get $len))))))
    (loop $spin (br $spin))
    (i64.const -1))
)
"#;
    let plugin_id = "selftest.wasm";
    let manifest = r#"
[plugin]
id = "selftest.wasm"
version = "0.1.0"
api = "cordis-portable/v1"
description = "wasm-component selftest fixture"

[runtime]
kind = "wasm-component"
entry = "plugin.wasm"
fuel = 2000000
max_memory_mb = 32
timeout_ms = 30000

[[tools]]
name = "echo"
description = "echo the request JSON"
input_schema = "schemas/echo.input.json"
output_schema = "schemas/echo.output.json"

[[tools]]
name = "trap"
description = "spin until the fuel watchdog kills the invocation"
input_schema = "schemas/trap.input.json"
output_schema = "schemas/trap.output.json"

[[ui]]
slot = "workspace.panel"
id = "selftest.wasm.panel"
label = "WASM Panel"
view = "selftest.wasm.view"
location = "right"
"#;
    let writes = [
        (package.join("plugin.wasm"), wat.as_bytes().to_vec()),
        (package.join("plugin.toml"), manifest.as_bytes().to_vec()),
        (
            package.join("package.json"),
            json!({ "name": plugin_id, "version": "0.1.0" })
                .to_string()
                .into_bytes(),
        ),
    ];
    for (path, bytes) in writes {
        if let Err(error) = std::fs::write(&path, bytes) {
            eprintln!("[selftest-wasm] FAIL：写 {} 失败: {error}", path.display());
            return 1;
        }
    }
    for name in ["echo", "trap"] {
        for direction in ["input", "output"] {
            let schema = json!({ "type": "object" });
            if let Err(error) = std::fs::write(
                package.join(format!("schemas/{name}.{direction}.json")),
                schema.to_string(),
            ) {
                eprintln!("[selftest-wasm] FAIL：写 schema 失败: {error}");
                return 1;
            }
        }
    }

    // 2) 信任安装（与产品安装路径一致）。
    let Ok(digest) = package_digest(&package) else {
        eprintln!("[selftest-wasm] FAIL：计算包摘要失败");
        return 1;
    };
    let trust = json!({
        "schema_version": 1,
        "plugin_id": plugin_id,
        "version": "0.1.0",
        "package": { "digest_sha256": digest }
    });
    let _ = std::fs::write(package.join("dshr-trust.json"), trust.to_string());

    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("[selftest-wasm] FAIL：加载 profile 失败: {error}");
            return 1;
        }
    };
    let policy = TrustPolicy {
        allow_downgrade: false,
        require_confirm_for_unsigned: true,
        store: TrustStore::default(),
        now_ms: None,
    };
    let install = match install_package_trusted(&sidecar, &package, &mut profile, &policy, |_| true)
    {
        Ok(report) => report,
        Err(error) => {
            eprintln!(
                "[selftest-wasm] FAIL：安装失败（{}）: {error}",
                error.code()
            );
            return 1;
        }
    };
    let installed_ok = install.install.destination.join("plugin.toml").is_file();

    // 3) 从 profile 装载（产品路径：loader 内部分发到 WASM 分支）。
    let mut plugins = PluginHost::new(ApprovalPolicy::desktop());
    let errors = load_enabled_external_plugins(&mut plugins, &profile, &[&sidecar]);
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("[selftest-wasm] {error}");
        }
        println!("[selftest-wasm] FAIL（装载失败）");
        return 1;
    }
    let loaded = plugins.is_loaded(plugin_id);
    let echo_ok = plugins
        .call("echo", &json!({ "text": "wasm" }))
        .ok()
        .and_then(|value| {
            value
                .get("arguments")
                .and_then(|arguments| arguments.get("text"))
                .and_then(|text| text.as_str())
                .map(str::to_owned)
        })
        .as_deref()
        == Some("wasm");
    let panel_ok = plugins
        .panels()
        .iter()
        .any(|panel| panel.plugin_id == plugin_id && panel.panel.id == "selftest.wasm.panel")
        && plugins
            .panels()
            .iter()
            .any(|panel| panel.panel.view_name() == "selftest.wasm.view");
    let render_ok = plugins
        .render_panel(plugin_id, "selftest.wasm.view")
        .map(|tree| tree["type"] == json!("Text"))
        .unwrap_or(false);

    // 4) trap：fuel 看门狗杀本次调用，宿主/插件存活（再 call echo 成功）。
    let trap = plugins.call("trap", &json!({}));
    let trap_isolated = trap
        .as_ref()
        .err()
        .map(|error| {
            let text = error.to_string();
            text.contains("E_QUOTA") || text.contains("E_TIMEOUT")
        })
        .unwrap_or(false);
    let survived = plugins
        .call("echo", &json!({ "text": "alive" }))
        .map(|value| value.to_string().contains("alive"))
        .unwrap_or(false);
    let unloaded = plugins.unload(plugin_id).is_ok() && !plugins.is_loaded(plugin_id);
    let ok = installed_ok
        && loaded
        && echo_ok
        && panel_ok
        && render_ok
        && trap_isolated
        && survived
        && unloaded;
    println!(
        "[selftest-wasm] installed_ok={installed_ok} loaded={loaded} echo_ok={echo_ok} panel_ok={panel_ok} render_ok={render_ok} trap_isolated={trap_isolated} survived={survived} unloaded={unloaded}"
    );
    let _ = std::fs::remove_dir_all(&root);
    if ok {
        println!("[selftest-wasm] PASS");
        0
    } else {
        println!("[selftest-wasm] FAIL");
        1
    }
}

/// `--selftest-adc-panel`：ADC panel 产品链路（无窗口，验证到 `AnyElement` 构造层）。
///
/// 链路：external fixture 装载 → `PluginHost::render_panel` → `adc_panel_body` →
/// GPUI 元素。断言：合法树 0 诊断、未知元素 → Placeholder + 诊断、未知 view /
/// 未知插件 / 不实现 view 都是可读错误、面板缓存可全部构造且无 panic。
pub(crate) fn selftest_adc_panel() -> i32 {
    let root = PathBuf::from(".tmp/adc-panel-selftest");
    let Some(setup) = prepare_fixture_plugin(&root, "selftest.adc", "selftest-adc-panel") else {
        return 1;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics_with_options(
        root.join("session.jsonl"),
        SESSION_ID,
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );
    let errors =
        load_enabled_external_plugins(&mut state.plugins, &setup.profile, &[&setup.sidecar]);
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("[selftest-adc-panel] {error}");
        }
        println!("[selftest-adc-panel] FAIL（装载失败）");
        return 1;
    }
    if !state.plugins.is_loaded("selftest.adc") {
        println!("[selftest-adc-panel] FAIL（插件未装载）");
        return 1;
    }
    // 装载后刷新面板缓存——产品面（右侧 panel）渲染读的就是这份缓存。
    state.refresh_adc_panels();
    let theme = Theme::light();
    let mut ok = true;

    // 1) 合法树：render_panel → adc_panel_body，0 诊断、元素构造成功。
    let legal_tree = match state.plugins.render_panel("selftest.adc", "fixture.adc") {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("[selftest-adc-panel] render_panel 失败: {error}");
            println!("[selftest-adc-panel] FAIL");
            return 1;
        }
    };
    let (legal_element, legal_diagnostics) = adc_panel_body(&theme, &legal_tree);
    drop(legal_element);
    let legal_ok = legal_diagnostics.is_empty();
    ok &= legal_ok;

    // 2) 未知元素（插件返回 Sparkline）：Placeholder + 诊断，不 panic。
    let unknown_tree = match state
        .plugins
        .render_panel("selftest.adc", "fixture.unknown")
    {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("[selftest-adc-panel] 未知元素 view 渲染失败: {error}");
            println!("[selftest-adc-panel] FAIL");
            return 1;
        }
    };
    let (unknown_element, unknown_diagnostics) = adc_panel_body(&theme, &unknown_tree);
    drop(unknown_element);
    let unknown_ok = unknown_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::UnknownElement);
    ok &= unknown_ok;

    // 3) 未知 view / 未知插件 / 不实现 view：可读错误，不 panic。
    let unknown_view_ok = state.plugins.render_panel("selftest.adc", "nope").is_err();
    let unknown_plugin_ok = state
        .plugins
        .render_panel("missing.plugin", "fixture.adc")
        .as_ref()
        .is_err_and(|error| error.to_string().contains("missing.plugin"));
    let default_view_ok = state
        .plugins
        .render_panel("builtin.probe-write", "anything")
        .as_ref()
        .is_err_and(|error| error.to_string().contains("does not render views"));
    ok &= unknown_view_ok && unknown_plugin_ok && default_view_ok;

    // 4) 面板缓存：至少 2 条 workspace.panel（fixture 声明了 right/bottom 两条），
    //    每条都能构造元素；失败走错误占位（这里不应失败）。
    let ordinary_views = state.adc_panels.iter().filter(|panel| {
        panel
            .slot
            .as_ref()
            .is_none_or(|slot| slot.slot_id != dshr_plugin_host::VIEWER_TAB)
    });
    let panels = ordinary_views.clone().count();
    let mut rendered = 0usize;
    let mut panel_errors = 0usize;
    let mut diag_total = 0usize;
    for panel in ordinary_views {
        match &panel.tree {
            Some(tree) => {
                let (element, diagnostics) = adc_panel_body(&theme, tree);
                drop(element);
                diag_total += diagnostics.len();
                rendered += 1;
            }
            None => panel_errors += 1,
        }
    }
    let panels_ok = panels >= 2 && rendered == panels && panel_errors == 0;
    ok &= panels_ok;

    println!(
        "[selftest-adc-panel] panels={panels} rendered={rendered} panel_errors={panel_errors} legal_diags={} unknown_diag={unknown_ok} diag_total={diag_total} unknown_view={unknown_view_ok} unknown_plugin={unknown_plugin_ok} default_view={default_view_ok}",
        legal_diagnostics.len()
    );
    if ok {
        println!("[selftest-adc-panel] PASS");
        0
    } else {
        println!("[selftest-adc-panel] FAIL");
        1
    }
}
