use std::path::PathBuf;

use crate::{
    diagnostics, export_session_cli, install_plugin_cli, prepare_selftest_cwd, retention_plan_cli,
    runtime_components, trust_key_cli, uninstall_plugin_cli,
};

pub(super) fn dispatch(args: &[String]) {
    if let Some(index) = args
        .iter()
        .position(|arg| arg == "--install-runtime-component")
    {
        let result = args
            .get(index + 1)
            .and_then(|name| runtime_components::Component::parse(name))
            .ok_or_else(|| "用法：--install-runtime-component <python|creator-build>".to_owned())
            .and_then(runtime_components::install);
        match result {
            Ok(path) => {
                println!("{}", path.display());
                std::process::exit(0);
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    // 3. 自检的 `.tmp` 相对路径与用户 CWD 解耦（只读 CWD 下也能跑）。
    prepare_selftest_cwd(args);
    if let Some(index) = args.iter().position(|arg| arg == "--live-chat") {
        let script: Vec<String> = args.iter().skip(index + 1).cloned().collect();
        if script.is_empty() {
            println!("[live-chat] 需要至少一条消息");
            std::process::exit(1);
        }
        std::process::exit(diagnostics::live_chat(&script));
    }
    if let Some(index) = args.iter().position(|arg| arg == "--live") {
        let text = args
            .get(index + 1)
            .cloned()
            .unwrap_or_else(|| "用一句话介绍你自己".to_owned());
        std::process::exit(diagnostics::live_turn(&text));
    }

    if std::env::args().any(|arg| arg == "--selftest-live-20") {
        std::process::exit(diagnostics::selftest_live_20());
    }
    if std::env::args().any(|arg| arg == "--selftest-stream") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_stream());
    }
    if std::env::args().any(|arg| arg == "--selftest-context") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_context());
    }
    if std::env::args().any(|arg| arg == "--selftest-long-context") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_long_context());
    }
    if std::env::args().any(|arg| arg == "--selftest-spill") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_spill());
    }
    if std::env::args().any(|arg| arg == "--selftest-approval") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_approval());
    }
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|arg| arg == "--selftest-crash-child") {
        let path = args.get(index + 1).cloned().unwrap_or_default();
        std::process::exit(diagnostics::selftest_crash_child(&path));
    }
    if args.iter().any(|arg| arg == "--selftest-crash-recovery") {
        std::process::exit(diagnostics::selftest_crash_recovery());
    }
    if args.iter().any(|arg| arg == "--selftest-window-recovery") {
        std::process::exit(diagnostics::selftest_window_recovery());
    }
    if args.iter().any(|arg| arg == "--selftest-corrupt-session") {
        std::process::exit(diagnostics::selftest_corrupt_session());
    }
    if std::env::args().any(|arg| arg == "--selftest-recovery") {
        std::process::exit(diagnostics::selftest_recovery());
    }
    if std::env::args().any(|arg| arg == "--selftest-sidecar-recovery") {
        std::process::exit(diagnostics::selftest_sidecar_recovery());
    }
    if std::env::args().any(|arg| arg == "--selftest-cancel") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_cancel());
    }
    if std::env::args().any(|arg| arg == "--selftest-sandbox") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_sandbox());
    }
    if std::env::args().any(|arg| arg == "--selftest-profile") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_profile());
    }
    if std::env::args().any(|arg| arg == "--selftest-loader") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_loader());
    }
    if std::env::args().any(|arg| arg == "--selftest-install") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_install());
    }
    if std::env::args().any(|arg| arg == "--selftest-retry") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_retry());
    }
    if std::env::args().any(|arg| arg == "--selftest-perf") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_perf());
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--seed-perf-session") {
        let dir = std::env::args()
            .nth(index + 1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".tmp/sessions"));
        std::process::exit(diagnostics::seed_perf_session(&dir));
    }
    if std::env::args().any(|arg| arg == "--selftest-trust-install") {
        std::process::exit(diagnostics::selftest_trust_install());
    }
    if std::env::args().any(|arg| arg == "--selftest-plugin-lifecycle") {
        std::process::exit(diagnostics::selftest_plugin_lifecycle());
    }
    if std::env::args().any(|arg| arg == "--selftest-external-plugin") {
        std::process::exit(diagnostics::selftest_external_plugin());
    }
    if std::env::args().any(|arg| arg == "--selftest-clipboard") {
        std::process::exit(diagnostics::selftest_clipboard());
    }
    if std::env::args().any(|arg| arg == "--selftest-window") {
        std::process::exit(diagnostics::selftest_window());
    }
    if std::env::args().any(|arg| arg == "--selftest-cpp-cli-install") {
        std::process::exit(diagnostics::selftest_cpp_cli_install());
    }
    if std::env::args().any(|arg| arg == "--selftest-cpp-install") {
        std::process::exit(diagnostics::selftest_cpp_install());
    }
    if std::env::args().any(|arg| arg == "--selftest-cpp-component") {
        std::process::exit(diagnostics::selftest_cpp_component());
    }
    if std::env::args().any(|arg| arg == "--selftest-wasm") {
        std::process::exit(diagnostics::selftest_wasm());
    }
    if std::env::args().any(|arg| arg == "--selftest-adc-panel") {
        std::process::exit(diagnostics::selftest_adc_panel());
    }
    if std::env::args().any(|arg| arg == "--selftest-defaults") {
        std::process::exit(diagnostics::selftest_defaults());
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--install-plugin") {
        let Some(path) = std::env::args().nth(index + 1) else {
            eprintln!(
                "用法: dshr --install-plugin <本地包目录> [--yes-install] [--allow-downgrade]"
            );
            std::process::exit(2);
        };
        std::process::exit(install_plugin_cli(PathBuf::from(path)));
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--uninstall-plugin") {
        let Some(name) = std::env::args().nth(index + 1) else {
            eprintln!("用法: dshr --uninstall-plugin <插件名>");
            std::process::exit(2);
        };
        std::process::exit(uninstall_plugin_cli(name));
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--trust-key") {
        let rest: Vec<String> = std::env::args().skip(index + 1).collect();
        std::process::exit(trust_key_cli(&rest));
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--export-session") {
        let Some(id) = std::env::args().nth(index + 1) else {
            eprintln!("用法: dshr --export-session <会话 id> [--export-dir <目录>]");
            std::process::exit(2);
        };
        let out = std::env::args()
            .position(|arg| arg == "--export-dir")
            .and_then(|dir_index| std::env::args().nth(dir_index + 1))
            .map(PathBuf::from);
        std::process::exit(export_session_cli(&id, out));
    }
    if std::env::args().any(|arg| arg == "--retention-plan") {
        std::process::exit(retention_plan_cli());
    }
    if std::env::args().any(|arg| arg == "--selftest-tree") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_tree());
    }
    if std::env::args().any(|arg| arg == "--selftest-export") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_export());
    }
    if std::env::args().any(|arg| arg == "--selftest-dsh-write") {
        std::process::exit(diagnostics::selftest_dsh_write());
    }
    if std::env::args().any(|arg| arg == "--selftest-dsh") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_dsh());
    }
    if std::env::args().any(|arg| arg == "--selftest-official") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_official_sidecar());
    }
    if std::env::args().any(|arg| arg == "--selftest-l2") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_l2());
    }
    if std::env::args().any(|arg| arg == "--selftest-parallel-tools") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_parallel_tools());
    }
    if std::env::args().any(|arg| arg == "--selftest-cordis") {
        std::env::set_var("DSHR_SIDECAR", "cordis");
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest_cordis());
    }
    if std::env::args().any(|arg| arg == "--selftest") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(diagnostics::selftest());
    }
}
