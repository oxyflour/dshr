use super::*;

#[test]
#[ignore = "requires tools/official-rc5-gate.ps1"]
fn rc5_user_plugin_installs_loads_and_uninstalls_from_profile() {
    let fixture = Fixture::new();
    let package = fixture.root().join("plugin-source");
    fs::create_dir(&package).unwrap();
    fs::write(
        package.join("package.json"),
        r#"{"name":"dshr-profile-test","version":"1.0.0","type":"module","main":"index.js"}"#,
    )
    .unwrap();
    fs::write(
        package.join("index.js"),
        r#"import { Context } from '@deepseek-ai/cordis';
import { createRequire } from 'node:module';
export const inject = ['tools'];
export function apply(ctx) {
  const commonJs = createRequire(import.meta.url)('@deepseek-ai/cordis');
  if (!(ctx instanceof Context) || commonJs.Context !== Context) {
    throw new Error('the host must share its Cordis instance');
  }
  ctx.tools.register({ name: 'profile_layout_check', execute: () => ({ ok: true }) });
}"#,
    )
    .unwrap();
    let resources = rc5_resource_root();
    let app = Path::new(env!("CARGO_BIN_EXE_dshr"));
    let command = || {
        let mut cmd = fixture.command(app);
        cmd.env("DSHR_APP_ROOT", &resources);
        cmd
    };
    assert_success(&invoke(
        command()
            .arg("--install-plugin")
            .arg(&package)
            .arg("--yes-install"),
        b"",
    ));
    let modules = fixture.home().join("profile/node_modules");
    let installed = modules.join("dshr-profile-test");
    assert!(installed.join("index.js").is_file());
    let duplicate = modules.join("@deepseek-ai/cordis");
    fs::create_dir_all(&duplicate).unwrap();
    fs::write(
        duplicate.join("package.json"),
        r#"{"name":"@deepseek-ai/cordis","type":"module","main":"index.js"}"#,
    )
    .unwrap();
    fs::write(duplicate.join("index.js"), "export class Context {};").unwrap();
    let list_tools = || {
        let reply =
            json!({ "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }] });
        let (endpoint, worker) = local_model("200 OK", reply, None);
        let mut cmd = openai_command(&fixture, &endpoint);
        cmd.env("DSHR_APP_ROOT", &resources)
            .env("DSHR_SIDECAR", "dsh");
        assert_success(&invoke(
            cmd.args(["--profile", "headless", "test tools"]),
            b"",
        ));
        worker.join().unwrap()["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|tool| tool["function"]["name"] == "profile_layout_check")
    };
    assert!(list_tools());
    assert_success(&invoke(
        command().args(["--uninstall-plugin", "dshr-profile-test"]),
        b"",
    ));
    assert!(!installed.exists());
    assert!(!list_tools());
}
