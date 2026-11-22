mod agent;
mod components;
mod context;
mod conversation;
mod performance;
mod plugins;
mod profile;
mod recovery;
mod sessions;
mod sidecar;
mod window;

pub(super) use agent::{selftest_defaults, selftest_retry};
pub(super) use components::{
    selftest_adc_panel, selftest_cpp_cli_install, selftest_cpp_component, selftest_cpp_install,
    selftest_external_plugin, selftest_wasm,
};
pub(super) use context::{selftest_context, selftest_long_context, selftest_spill};
pub(super) use conversation::{
    live_chat, live_turn, selftest, selftest_cancel, selftest_live_20, selftest_stream,
};
pub(super) use performance::{seed_perf_session, selftest_perf};
pub(super) use plugins::{selftest_install, selftest_plugin_lifecycle, selftest_trust_install};
pub(super) use profile::{selftest_loader, selftest_profile, selftest_sandbox};
pub(super) use recovery::{
    selftest_corrupt_session, selftest_crash_child, selftest_crash_recovery, selftest_recovery,
    selftest_sidecar_recovery, selftest_window_recovery,
};
pub(super) use sessions::{selftest_export, selftest_tree};
pub(super) use sidecar::{
    selftest_approval, selftest_cordis, selftest_dsh, selftest_dsh_write, selftest_l2,
    selftest_official_sidecar, selftest_parallel_tools,
};
pub(super) use window::{run_navigation_flow, selftest_clipboard, selftest_window};
