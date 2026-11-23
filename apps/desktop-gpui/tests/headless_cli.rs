use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use dshr_session_store::try_lock_session;
use serde_json::{json, Value};

#[path = "fixtures/official-plugin-layout.rs"]
mod official_plugin_layout;

struct Fixture {
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("dshr-headless-")
            .tempdir()
            .expect("create an isolated headless CLI workspace");
        Self { directory }
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("DSHR_") {
                command.env_remove(name);
            }
        }
        command
            .current_dir(self.root())
            .env("DSHR_HOME", self.home())
            .env("DSHR_WORKSPACE", self.root())
            .env("DSHR_PROVIDER", "mock")
            .env("DSHR_APP_ROOT", self.root().join("no-sidecar"));
        command
    }

    fn run(&self, args: &[&str], stdin: &[u8]) -> Output {
        invoke(
            self.command(Path::new(env!("CARGO_BIN_EXE_dshr")))
                .args(args),
            stdin,
        )
    }

    fn install_sidecar(&self, tool: &str, presentation: &str) {
        let dir = self.root().join("no-sidecar/sidecar/dsh-official");
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        let script = include_str!("fixtures/headless-sidecar.mjs")
            .replace("FIXTURE_TOOL", tool)
            .replace("FIXTURE_PRESENTATION", presentation);
        fs::write(dir.join("sidecar-dsh.mjs"), script).unwrap();
    }

    fn log(&self, id: &str) -> PathBuf {
        self.home().join("sessions").join(format!("{id}.jsonl"))
    }
}

fn invoke(command: &mut Command, input: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if !input.is_empty() {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn records(bytes: &[u8]) -> Vec<Value> {
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn local_model(
    status: &str,
    response: Value,
    hold: Option<(Sender<()>, Receiver<()>)>,
) -> (String, thread::JoinHandle<Value>) {
    let (server, endpoint) = model_listener();
    let status = status.to_owned();
    let worker =
        thread::spawn(move || answer_model_request(&server, &status, response.to_string(), hold));
    (endpoint, worker)
}

fn model_listener() -> (TcpListener, String) {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr().unwrap();
    server.set_nonblocking(true).unwrap();
    (server, format!("http://{address}"))
}

fn answer_model_request(
    server: &TcpListener,
    status: &str,
    response: String,
    hold: Option<(Sender<()>, Receiver<()>)>,
) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut stream = loop {
        match server.accept() {
            Ok((stream, _)) => break stream,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10))
            }
            Err(error) => panic!("model fixture did not receive a request: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(&mut stream);
    let mut length = None;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = Some(value.trim().parse().unwrap());
        }
    }
    let mut request = vec![0; length.unwrap()];
    reader.read_exact(&mut request).unwrap();
    if let Some((ready, hold)) = hold {
        ready.send(()).unwrap();
        hold.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
    serde_json::from_slice(&request).unwrap()
}

fn openai_command(fixture: &Fixture, endpoint: &str) -> Command {
    let mut command = fixture.command(Path::new(env!("CARGO_BIN_EXE_dshr")));
    command
        .env("DSHR_PROVIDER", "openai")
        .env("OPENAI_API_KEY", "fixture-secret")
        .env("OPENAI_BASE_URL", endpoint)
        .env("NO_PROXY", "127.0.0.1");
    command
}

fn rc5_resource_root() -> PathBuf {
    let sidecar =
        PathBuf::from(std::env::var_os("DSHR_OFFICIAL_RC5_DIR").expect("isolated rc.5 sidecar"));
    assert!(sidecar.join("sidecar-dsh.mjs").is_file());
    sidecar
        .parent()
        .and_then(Path::parent)
        .expect("app resource root")
        .to_path_buf()
}

#[test]
#[ignore = "requires tools/official-rc5-gate.ps1"]
fn rc5_desktop_official_writes() {
    let fixture = Fixture::new();
    let output = invoke(
        fixture
            .command(Path::new(env!("CARGO_BIN_EXE_dshr")))
            .env("DSHR_APP_ROOT", rc5_resource_root())
            .arg("--selftest-dsh-write"),
        b"",
    );
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("[selftest-dsh-write] PASS"));
}

#[test]
#[ignore = "requires tools/official-rc5-gate.ps1"]
fn rc5_headless_replays_the_saved_working_directory() {
    let fixture = Fixture::new();
    let settings = fixture.root().join("profile");
    let child = fixture.root().join("child");
    fs::create_dir(&settings).unwrap();
    fs::create_dir(&child).unwrap();
    fs::write(fixture.root().join("file.txt"), "root marker").unwrap();
    fs::write(child.join("file.txt"), "saved directory marker").unwrap();
    fs::write(
        settings.join("plugins.json"),
        json!({ "plugins": [
        { "name": "@deepseek-ai/dsh-tool-working-directory", "enabled": true },
        { "name": "@deepseek-ai/dsh-tool-fs", "enabled": true, "config": { "read": true } },
    ] })
        .to_string(),
    )
    .unwrap();

    let tool = |id: &str, name: &str, args: Value| {
        json!({
            "choices": [{ "message": { "tool_calls": [{
                "id": id, "type": "function", "function": { "name": name, "arguments": args.to_string() },
            }] }, "finish_reason": "tool_calls" }]
        })
    };
    let final_reply =
        json!({ "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }] });
    let (server, endpoint) = model_listener();
    let worker = thread::spawn(move || {
        [
            tool("cwd", "working_directory", json!({ "cd": "child" })),
            final_reply.clone(),
            tool("read", "read", json!({ "file_path": "file.txt" })),
            final_reply,
        ]
        .into_iter()
        .map(|response| answer_model_request(&server, "200 OK", response.to_string(), None))
        .collect::<Vec<_>>()
    });
    let command = || {
        let mut cmd = openai_command(&fixture, &endpoint);
        cmd.env("DSHR_APP_ROOT", rc5_resource_root())
            .env("DSHR_PROFILE_DIR", &settings)
            .env("DSHR_SIDECAR", "dsh");
        cmd
    };
    let output = invoke(
        command().args(["--profile", "headless", "--json", "switch directory"]),
        b"",
    );
    assert_success(&output);
    let events = records(&output.stdout);
    let session = events[0]["sessionId"].as_str().unwrap();
    assert!(records(&fs::read(fixture.log(session)).unwrap())
        .iter()
        .any(|event| {
            event["type"] == "working-directory/change"
                && event["payload"]["cwd"]
                    .as_str()
                    .is_some_and(|cwd| Path::new(cwd).ends_with("child"))
        }));

    let output = invoke(
        command().args([
            "--profile",
            "headless",
            "--json",
            "--session-id",
            session,
            "read file",
        ]),
        b"",
    );
    assert_success(&output);
    let requests = worker.join().unwrap();
    assert!(requests[3]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "read"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("saved directory marker"))
        }));
}

fn rc5_image_profile(fixture: &Fixture) -> PathBuf {
    let profile = fixture.root().join("image-profile");
    fs::create_dir(&profile).unwrap();
    fs::write(profile.join("plugins.json"), json!({ "plugins": [{
        "name": "@deepseek-ai/dsh-tool-fs",
        "enabled": true,
        "inject": ["tools", "fs", "systemPrompt"],
        "config": { "read": true, "readImage": true, "write": false, "edit": false, "applyPatch": false }
    }] }).to_string()).unwrap();
    profile
}

#[test]
#[ignore = "requires tools/official-rc5-gate.ps1"]
fn rc5_headless_preserves_image_results_across_restart() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let profile = rc5_image_profile(&fixture);
    let png = fixture.root().join("sample.png");
    let external_png = outside.root().join("outside.png");
    image::RgbImage::from_pixel(2100, 3, image::Rgb([180, 40, 12]))
        .save(&png)
        .unwrap();
    fs::copy(&png, &external_png).unwrap();
    fs::write(fixture.root().join("bad.png"), b"not a png").unwrap();
    fs::File::create(fixture.root().join("too-large.png"))
        .unwrap()
        .set_len(8 * 1024 * 1024 + 1)
        .unwrap();

    let calls = [
        ("picture", json!("sample.png")), ("outside", json!(external_png)),
        ("bad", json!("bad.png")), ("size", json!("too-large.png")),
    ]
        .into_iter().map(|(id, path)| json!({
            "id": id, "type": "function",
            "function": { "name": "read_image", "arguments": json!({ "file_path": path }).to_string() },
        })).collect::<Vec<_>>();
    let reply = json!({ "choices": [{
        "message": { "tool_calls": calls }, "finish_reason": "tool_calls",
    }] });
    let final_reply =
        json!({ "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }] });
    let (server, endpoint) = model_listener();
    let worker = thread::spawn(move || {
        [reply, final_reply.clone(), final_reply]
            .into_iter()
            .map(|response| answer_model_request(&server, "200 OK", response.to_string(), None))
            .collect::<Vec<_>>()
    });
    let command = || {
        let mut cmd = openai_command(&fixture, &endpoint);
        cmd.env("DSHR_APP_ROOT", rc5_resource_root())
            .env("DSHR_PROFILE_DIR", &profile)
            .env("DSHR_SIDECAR", "dsh");
        cmd
    };
    let output = invoke(
        command().args(["--profile", "headless", "--json", "read images"]),
        b"",
    );
    assert_success(&output);
    let events = records(&output.stdout);
    let session = events[0]["sessionId"].as_str().unwrap();
    let log = records(&fs::read(fixture.log(session)).unwrap());
    let spill = log
        .iter()
        .find(|event| event["type"] == "spill.created" && event["payload"]["kind"] == "tool.image")
        .unwrap();
    let outcome = |call_id: &str| {
        &log.iter()
            .find(|event| event["type"] == "tool.result" && event["payload"]["call_id"] == call_id)
            .unwrap()["payload"]
    };
    assert_eq!(outcome("picture")["is_error"], false, "{log:?}");
    assert_eq!(
        outcome("picture")["value"]["image"]["attachmentId"],
        spill["payload"]["id"]
    );
    assert_eq!(outcome("picture")["value"]["image"]["width"], 2048);
    assert_eq!(outcome("picture")["value"]["image"]["height"], 3);
    assert_eq!(
        outcome("picture")["value"]["image"]["originalDimensions"]["width"],
        2100
    );
    for id in ["outside", "bad", "size"] {
        assert_eq!(outcome(id)["is_error"], true, "{log:?}");
    }

    fs::remove_file(png).unwrap();
    let output = invoke(
        command().args([
            "--profile",
            "headless",
            "--json",
            "--session-id",
            session,
            "resume",
        ]),
        b"",
    );
    assert_success(&output);
    let requests = worker.join().unwrap();
    let model_image = |index: usize| {
        let messages = requests[index]["messages"].as_array().unwrap();
        let image_index = messages
            .iter()
            .position(|message| {
                message["content"]
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(|part| part["type"] == "image_url"))
            })
            .unwrap();
        assert!(messages[..image_index]
            .iter()
            .rev()
            .take(4)
            .all(|message| message["role"] == "tool"));
        messages[image_index]["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|part| part["type"] == "image_url")
            .unwrap()["image_url"]["url"]
            .as_str()
            .unwrap()
    };
    assert!(model_image(1).starts_with("data:image/png;base64,"));
    assert_eq!(model_image(1), model_image(2));
}

#[test]
#[ignore = "requires tools/official-rc5-gate.ps1"]
fn rc5_headless_rejects_read_image_on_a_text_only_route() {
    let fixture = Fixture::new();
    let profile = rc5_image_profile(&fixture);
    let (server, endpoint) = model_listener();
    let worker = thread::spawn(move || {
        let call = json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [{
            "index": 0, "id": "text-route", "type": "function",
            "function": { "name": "read_image", "arguments": "{\"file_path\":\"unread.png\"}" },
        }] }, "finish_reason": "tool_calls" }] });
        let done = json!({ "choices": [{ "index": 0, "delta": { "content": "ok" }, "finish_reason": "stop" }] });
        for response in [call, done] {
            answer_model_request(
                &server,
                "200 OK",
                format!("data: {response}\n\ndata: [DONE]\n\n"),
                None,
            );
        }
    });
    let bin = Path::new(env!("CARGO_BIN_EXE_dshr"));
    let output = invoke(
        fixture
            .command(bin)
            .env("DSHR_APP_ROOT", rc5_resource_root())
            .env("DSHR_PROFILE_DIR", profile)
            .env("DSHR_SIDECAR", "dsh")
            .env("DSHR_PROVIDER", "deepseek")
            .env("DEEPSEEK_API_KEY", "fixture-secret")
            .env("DEEPSEEK_BASE_URL", endpoint)
            .env("DEEPSEEK_MODEL", "deepseek-chat")
            .env("NO_PROXY", "127.0.0.1")
            .args(["--profile", "headless", "--json", "read image"]),
        b"",
    );
    assert_success(&output);
    worker.join().unwrap();
    let events = records(&output.stdout);
    let session = events[0]["sessionId"].as_str().unwrap();
    let log = records(&fs::read(fixture.log(session)).unwrap());
    let result = &log
        .iter()
        .find(|event| event["type"] == "tool.result")
        .unwrap()["payload"];
    assert_eq!(result["is_error"], true, "{result:?}");
    assert!(
        result["content"]
            .to_string()
            .contains("does not declare image input"),
        "{result:?}"
    );
    assert!(log.iter().all(|event| event["type"] != "spill.created"));
}

#[test]
#[ignore = "requires the rc.5 search and shell packages"]
fn stage2_headless_search_uses_session_directory_and_excludes_shell() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let profile = fixture.root().join("search-profile");
    let child = fixture.root().join("child");
    fs::create_dir(&profile).unwrap();
    fs::create_dir(&child).unwrap();
    fs::write(child.join("marker.txt"), "search-marker-child").unwrap();
    fs::write(fixture.root().join("root-only.txt"), "search-marker-root").unwrap();
    fs::write(outside.root().join("outside.txt"), "search-marker-outside").unwrap();
    let shell = if cfg!(windows) { "pwsh" } else { "bash" };
    fs::write(profile.join("plugins.json"), json!({ "plugins": [
        { "name": "@deepseek-ai/dsh-tool-working-directory", "enabled": true },
        { "name": "@deepseek-ai/dsh-tool-fs-search", "enabled": true, "config": { "sampleOverCapGlobResults": false } },
        { "name": format!("@deepseek-ai/dsh-tool-{shell}"), "enabled": true },
    ] }).to_string()).unwrap();
    let tool = |id: &str, name: &str, arguments: Value| {
        json!({
            "id": id, "type": "function",
            "function": { "name": name, "arguments": arguments.to_string() },
        })
    };
    let reply = |calls: Vec<Value>| {
        json!({
            "choices": [{ "message": { "tool_calls": calls }, "finish_reason": "tool_calls" }]
        })
    };
    let responses = [
        reply(vec![tool(
            "cwd",
            "working_directory",
            json!({ "cd": "child" }),
        )]),
        reply(vec![
            tool("glob", "glob", json!({ "pattern": "**/*.txt" })),
            tool("grep", "grep", json!({ "pattern": "search-marker-" })),
            tool(
                "escape",
                "grep",
                json!({ "pattern": "search-marker-", "path": outside.root() }),
            ),
        ]),
        json!({ "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }] }),
    ];
    let (server, endpoint) = model_listener();
    let worker = thread::spawn(move || {
        responses
            .into_iter()
            .map(|response| answer_model_request(&server, "200 OK", response.to_string(), None))
            .collect::<Vec<_>>()
    });
    let output = invoke(
        openai_command(&fixture, &endpoint)
            .env("DSHR_APP_ROOT", rc5_resource_root())
            .env("DSHR_PROFILE_DIR", profile)
            .env("DSHR_SIDECAR", "dsh")
            .args(["--profile", "headless", "--json", "search"]),
        b"",
    );
    assert_success(&output);
    let requests = worker.join().unwrap();
    let offered = requests[0]["tools"].as_array().unwrap();
    assert!(offered
        .iter()
        .any(|tool| tool["function"]["name"] == "glob"));
    assert!(offered
        .iter()
        .any(|tool| tool["function"]["name"] == "grep"));
    assert!(offered
        .iter()
        .all(|tool| !matches!(tool["function"]["name"].as_str(), Some("pwsh" | "bash"))));
    let events = records(&output.stdout);
    let session = events[0]["sessionId"].as_str().unwrap();
    let log = records(&fs::read(fixture.log(session)).unwrap());
    let outcome = |call_id: &str| {
        &log.iter()
            .find(|event| event["type"] == "tool.result" && event["payload"]["call_id"] == call_id)
            .unwrap()["payload"]
    };
    assert_eq!(
        outcome("glob")["value"]["paths"],
        json!(["marker.txt"]),
        "{}",
        outcome("glob")
    );
    assert_eq!(
        outcome("grep")["value"]["matches"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        outcome("grep")["value"]["matches"][0]["line"],
        "search-marker-child"
    );
    assert_eq!(outcome("escape")["is_error"], true);
}

#[test]
#[ignore = "requires the rc.5 ask-user package"]
fn stage3_headless_questions_return_a_noninteractive_error() {
    let fixture = Fixture::new();
    let profile = fixture.root().join("questions-profile");
    fs::create_dir(&profile).unwrap();
    fs::write(
        profile.join("plugins.json"),
        json!({
            "plugins": [{ "name": "@deepseek-ai/dsh-tool-ask-user" }],
        })
        .to_string(),
    )
    .unwrap();
    let (server, endpoint) = model_listener();
    let worker = thread::spawn(move || {
        let arguments =
            json!({ "questions": [{ "id": "language", "question": "Which language?" }] });
        let call = json!({ "choices": [{ "message": { "tool_calls": [{
            "id": "ask", "type": "function",
            "function": { "name": "ask_user_question", "arguments": arguments.to_string() },
        }] }, "finish_reason": "tool_calls" }] });
        let done = json!({ "choices": [{ "message": { "content": "continued" }, "finish_reason": "stop" }] });
        [call, done]
            .into_iter()
            .map(|reply| answer_model_request(&server, "200 OK", reply.to_string(), None))
            .collect::<Vec<_>>()
    });
    let output = invoke(
        openai_command(&fixture, &endpoint)
            .env("DSHR_APP_ROOT", rc5_resource_root())
            .env("DSHR_PROFILE_DIR", profile)
            .env("DSHR_SIDECAR", "dsh")
            .args(["--profile", "headless", "--json", "ask a question"]),
        b"",
    );
    assert_success(&output);
    let requests = worker.join().unwrap();
    assert!(requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| { tool["function"]["name"] == "ask_user_question" }));
    let events = records(&output.stdout);
    let session = events[0]["sessionId"].as_str().unwrap();
    let log = records(&fs::read(fixture.log(session)).unwrap());
    let result = &log
        .iter()
        .find(|event| event["type"] == "tool.result")
        .unwrap()["payload"];
    assert_eq!(result["is_error"], true, "{result:?}");
    assert!(
        result["content"]
            .to_string()
            .contains("require an interactive desktop session"),
        "{result:?}"
    );
}

#[test]
#[ignore = "requires the rc.5 present package"]
fn stage3_headless_deliverables_round_trip_in_the_session() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let profile = fixture.root().join("delivery-profile");
    let child = fixture.root().join("child");
    fs::create_dir(&profile).unwrap();
    fs::create_dir(&child).unwrap();
    fs::write(child.join("deliverable.txt"), "result").unwrap();
    let outside_file = outside.root().join("outside.txt");
    fs::write(&outside_file, "outside").unwrap();
    fs::write(
        profile.join("plugins.json"),
        json!({ "plugins": [
            { "name": "@deepseek-ai/dsh-tool-working-directory" },
            { "name": "@deepseek-ai/dsh-tool-present" },
        ] })
        .to_string(),
    )
    .unwrap();
    let (server, endpoint) = model_listener();
    let worker = thread::spawn(move || {
        let tool = |id: &str, name: &str, args: Value| {
            json!({ "id": id, "type": "function", "function": {
                "name": name, "arguments": args.to_string(),
            } })
        };
        let calls = |calls: Vec<Value>| json!({ "choices": [{ "message": { "tool_calls": calls }, "finish_reason": "tool_calls" }] });
        let done = json!({ "choices": [{ "message": { "content": "delivered" }, "finish_reason": "stop" }] });
        let responses = [
            calls(vec![tool(
                "cwd",
                "working_directory",
                json!({ "cd": "child" }),
            )]),
            calls(vec![
                tool(
                    "deliver",
                    "present",
                    json!({ "files": [{ "path": "deliverable.txt", "description": "Result" }] }),
                ),
                tool(
                    "escape",
                    "present",
                    json!({ "files": [{ "path": outside_file }] }),
                ),
            ]),
            done.clone(),
            done,
        ];
        responses
            .into_iter()
            .map(|reply| answer_model_request(&server, "200 OK", reply.to_string(), None))
            .collect::<Vec<_>>()
    });
    let command = || {
        let mut cmd = openai_command(&fixture, &endpoint);
        cmd.env("DSHR_APP_ROOT", rc5_resource_root())
            .env("DSHR_PROFILE_DIR", &profile)
            .env("DSHR_SIDECAR", "dsh")
            .args(["--profile", "headless", "--json"]);
        cmd
    };
    let output = invoke(command().arg("deliver a file"), b"");
    assert_success(&output);
    let events = records(&output.stdout);
    let session = events[0]["sessionId"].as_str().unwrap();
    let log = records(&fs::read(fixture.log(session)).unwrap());
    let results: Vec<_> = log
        .iter()
        .filter(|event| event["type"] == "tool.result")
        .collect();
    assert!(
        results.iter().any(|event| {
            event["payload"]["call_id"] == "deliver" && event["payload"]["is_error"] == false
        }),
        "{log:?}"
    );
    assert!(
        results.iter().any(|event| {
            event["payload"]["call_id"] == "escape" && event["payload"]["is_error"] == true
        }),
        "{log:?}"
    );
    let deliveries: Vec<_> = log
        .iter()
        .filter(|event| event["type"] == "deliverables/presented")
        .collect();
    assert_eq!(deliveries.len(), 1, "{log:?}");
    let delivery = &deliveries[0]["payload"];
    assert_eq!(delivery["turn"], 1);
    assert_eq!(delivery["callId"], "deliver");
    assert_eq!(delivery["files"][0]["description"], "Result");
    let path = Path::new(delivery["files"][0]["path"].as_str().unwrap());
    assert_eq!(path, Path::new("child").join("deliverable.txt"));

    let output = invoke(command().args(["--session-id", session, "resume"]), b"");
    assert_success(&output);
    let requests = worker.join().unwrap();
    assert!(requests[3]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "deliver"
                && message["content"].to_string().contains("deliverable.txt")
        }));
}

#[test]
#[ignore = "requires the rc.5 skill tool and filesystem provider"]
fn stage3_headless_skill_is_scoped_and_supports_direct_invocation() {
    let fixture = Fixture::new();
    let profile = fixture.root().join("skill-profile");
    fs::create_dir(&profile).unwrap();
    for (project, name, body) in [
        (fixture.root().to_path_buf(), "root-only", "ROOT_SKILL_BODY"),
        (
            fixture.root().join("child"),
            "child-only",
            "CHILD_SKILL_BODY",
        ),
    ] {
        let skill = project.join(".agents").join("skills").join(name);
        fs::create_dir_all(&skill).unwrap();
        fs::create_dir(project.join(".git")).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Use the {name} skill\n---\n{body}\n"),
        )
        .unwrap();
    }
    fs::write(profile.join("plugins.json"), json!({ "plugins": [
        { "name": "@deepseek-ai/dsh-tool-working-directory" },
        { "name": "@deepseek-ai/dsh-skill-filesystem", "config": {
            "includeDefaultRoots": true, "watch": false,
            "dshHome": fixture.root().join("no-user-dsh"), "agentsHome": fixture.root().join("no-user-agents"),
        } },
        { "name": "@deepseek-ai/dsh-tool-skill" },
    ] }).to_string()).unwrap();
    let (server, endpoint) = model_listener();
    let worker = thread::spawn(move || {
        let call = |id, name, arguments: Value| {
            json!({ "choices": [
            { "message": { "tool_calls": [{ "id": id, "type": "function", "function":
                { "name": name, "arguments": arguments.to_string() }
            }] }, "finish_reason": "tool_calls" },
        ] })
        };
        let done =
            json!({ "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }] });
        let replies = [
            call("cwd", "working_directory", json!({ "cd": "child" })),
            call("load", "skill", json!({ "name": "child-only" })),
            done.clone(),
            done,
        ];
        replies
            .into_iter()
            .map(|reply| answer_model_request(&server, "200 OK", reply.to_string(), None))
            .collect::<Vec<_>>()
    });
    let command = || {
        let mut cmd = openai_command(&fixture, &endpoint);
        cmd.env("DSHR_APP_ROOT", rc5_resource_root())
            .env("DSHR_PROFILE_DIR", &profile)
            .env("DSHR_SIDECAR", "dsh")
            .args(["--profile", "headless", "--json"]);
        cmd
    };
    let first = invoke(command().arg("Load the project skill"), b"");
    assert_success(&first);
    let events = records(&first.stdout);
    let session = events[0]["sessionId"].as_str().unwrap();
    let log = records(&fs::read(fixture.log(session)).unwrap());
    assert!(
        log.iter().any(|event| {
            event["type"] == "tool.result"
                && event["payload"]["call_id"] == "load"
                && event["payload"]["is_error"] == false
                && event["payload"]["content"]
                    .to_string()
                    .contains("CHILD_SKILL_BODY")
        }),
        "{log:?}"
    );
    assert_success(&invoke(
        command().args(["--session-id", session, "Please use /child-only now"]),
        b"",
    ));
    let requests = worker.join().unwrap();
    let systems = |index: usize| {
        requests[index]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "system")
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(systems(0).contains("root-only"), "{requests:?}");
    assert!(!systems(0).contains("child-only"), "{requests:?}");
    assert!(systems(1).contains("child-only"), "{requests:?}");
    assert!(!systems(1).contains("root-only"), "{requests:?}");
    assert!(systems(3).contains("CHILD_SKILL_BODY"), "{requests:?}");
}

#[test]
#[ignore = "requires rc.5 Web tool and providers"]
fn stage3_headless_web_uses_official_providers_and_times_out() {
    let fixture = Fixture::new();
    let profile = fixture.root().join("web-profile");
    fs::create_dir(&profile).unwrap();
    let (web, web_url) = model_listener();
    let (server, model_url) = model_listener();
    let page = "https://example.com/source";
    fs::write(
        profile.join("plugins.json"),
        json!({ "plugins": [
        { "name": "@deepseek-ai/dsh-web-deepseek-api", "config": {
            "searchBaseURL": web_url, "fetchBaseURL": web_url,
            "searchApiKeyEnv": "DSHR_FIXTURE_WEB_KEY", "fetchApiKeyEnv": "DSHR_FIXTURE_WEB_KEY",
            "sendOriginalQuestion": false
        } },
        { "name": "@deepseek-ai/dsh-tool-web", "config": { "fetchTimeoutMs": 1000 } },
    ] })
        .to_string(),
    )
    .unwrap();
    let api = thread::spawn(move || {
        let search = answer_model_request(
            &web,
            "200 OK",
            json!({ "results": [{
            "url": page, "title": "Fixture source", "snippet": "SEARCH_SNIPPET", "date": null
        }] })
            .to_string(),
            None,
        );
        let fetch = answer_model_request(
            &web,
            "200 OK",
            json!({
                "content": "WEB_PAGE_BODY",
            })
            .to_string(),
            None,
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let silent = loop {
            match web.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("fixture did not receive the timed fetch: {error}"),
            }
        };
        thread::sleep(Duration::from_millis(1500));
        drop(silent);
        (search, fetch)
    });
    let model = thread::spawn(move || {
        let tool = |id, name, args: Value| {
            json!({ "choices": [{ "message": {
            "tool_calls": [{ "id": id, "type": "function", "function": {
                "name": name, "arguments": args.to_string(),
            } }],
        }, "finish_reason": "tool_calls" }] })
        };
        let replies = [
            tool(
                "search",
                "web_search",
                json!({ "queries": ["fixture query"] }),
            ),
            tool("fetch", "web_fetch", json!({ "url": page })),
            tool(
                "timeout",
                "web_fetch",
                json!({ "url": "https://example.com/slow" }),
            ),
            json!({ "choices": [{ "message": { "content": "done" }, "finish_reason": "stop" }] }),
        ];
        replies
            .into_iter()
            .map(|reply| answer_model_request(&server, "200 OK", reply.to_string(), None))
            .collect::<Vec<_>>()
    });
    let command = |endpoint: &str| {
        let mut cmd = openai_command(&fixture, endpoint);
        cmd.env("DSHR_APP_ROOT", rc5_resource_root())
            .env("DSHR_PROFILE_DIR", &profile)
            .env("DSHR_SIDECAR", "dsh")
            .args(["--profile", "headless", "--json"]);
        cmd
    };
    // The provider can be explicitly unavailable even while the tool package is loaded.
    let (no_key, no_key_model) = local_model(
        "200 OK",
        json!({
            "choices": [{ "message": { "content": "done" }, "finish_reason": "stop" }]
        }),
        None,
    );
    assert_success(&invoke(
        command(&no_key)
            .env_remove("DSHR_FIXTURE_WEB_KEY")
            .arg("no credential"),
        b"",
    ));
    let missing = no_key_model.join().unwrap();
    assert!(
        missing["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|tool| {
                !matches!(
                    tool["function"]["name"].as_str(),
                    Some("web_search" | "web_fetch")
                )
            }),
        "{missing:?}"
    );
    let output = invoke(
        command(&model_url)
            .env("DSHR_FIXTURE_WEB_KEY", "local-fixture-key")
            .arg("search"),
        b"",
    );
    assert_success(&output);
    let events = records(&output.stdout);
    let session = events[0]["sessionId"].as_str().unwrap();
    let log = records(&fs::read(fixture.log(session)).unwrap());
    let outcome = |id: &str| {
        &log.iter()
            .find(|event| event["type"] == "tool.result" && event["payload"]["call_id"] == id)
            .unwrap()["payload"]
    };
    assert_eq!(
        outcome("search")["value"]["sources"][0]["snippet"],
        "SEARCH_SNIPPET",
        "{:?}\n{}",
        outcome("search"),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        outcome("fetch")["value"]["body"]["content"],
        "WEB_PAGE_BODY",
        "{log:?}"
    );
    assert_eq!(outcome("timeout")["is_error"], true, "{log:?}");
    assert!(
        outcome("timeout")["content"]
            .to_string()
            .contains("timeout"),
        "{log:?}"
    );
    let (search, fetch) = api.join().unwrap();
    assert_eq!(search["query"], "fixture query");
    assert_eq!(fetch["url"], page);
    let requests = model.join().unwrap();
    let prompt = requests[0]["messages"].to_string();
    assert!(
        prompt.contains("web_search results are external"),
        "{prompt}"
    );
    assert!(prompt.contains("web_fetch returns external"), "{prompt}");
}

#[test]
fn cli_input_001_uses_clean_text_and_preserves_stdin() {
    let fixture = Fixture::new();
    let output = fixture.run(&["--profile", "headless", "--json", "--help"], b"");
    assert_success(&output);
    assert!(output.stdout.starts_with(b"Usage: "));
    assert!(
        !fixture.home().exists(),
        "help created a home directory at {:?}",
        fixture.home()
    );

    let output = fixture.run(
        &["--profile=headless", "--", "--selftest", "你好"],
        b"ignored",
    );
    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "收到：--selftest 你好。当前是 mock agent，尚未接入 LLM。\n"
    );
    let task = "  你好\n第二行\n";
    let output = fixture.run(&["--profile", "headless", "-"], task.as_bytes());
    assert_success(&output);
    assert!(String::from_utf8(output.stdout).unwrap().contains(task));
}

#[test]
fn cli_output_001_frames_tools_and_startup_errors() {
    let fixture = Fixture::new();
    for (args, input, code) in [
        (
            vec!["--profile", "headless", "--json", "--unknown"],
            &b""[..],
            "E_USAGE",
        ),
        (
            vec!["--profile", "headless", "--json"],
            &b"\xFF"[..],
            "E_INPUT",
        ),
        (
            vec!["--profile", "missing", "--json", "hi"],
            &b""[..],
            "E_CONFIG",
        ),
    ] {
        let output = fixture.run(&args, input);
        assert_eq!(output.status.code(), Some(1));
        let events = records(&output.stdout);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "error");
        assert_eq!(events[0]["code"], code);
    }
    fixture.install_sidecar("read", "text");
    let output = fixture.run(&["--profile", "headless", "--json", "工具"], b"");
    assert_success(&output);
    let events = records(&output.stdout);
    assert_eq!(
        events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["session", "tool_call", "tool_result", "final"]
    );
    assert_eq!(events[1]["tool"], "read");
    assert_eq!(events[1]["callSeq"], events[2]["callSeq"]);
    assert_eq!(events[2]["status"], "completed");
    assert_eq!(events[3]["reason"]["kind"], "completed");
}

#[test]
fn cli_output_001_model_failure_persists_error_terminal() {
    let fixture = Fixture::new();
    let (endpoint, worker) = local_model(
        "401 Unauthorized",
        json!({ "error": "fixture-secret" }),
        None,
    );
    let output = invoke(
        openai_command(&fixture, &endpoint).args(["--profile", "headless", "--json", "hi"]),
        b"",
    );
    worker.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let events = records(&output.stdout);
    assert_eq!(events[1]["type"], "final");
    assert_eq!(events[1]["text"], "");
    assert_eq!(events[1]["reason"]["kind"], "error");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture-secret"));
    let log = records(&fs::read(fixture.log(events[0]["sessionId"].as_str().unwrap())).unwrap());
    assert_eq!(log.last().unwrap()["payload"]["reason"], "error");
}

#[test]
fn cli_output_001_final_save_failure_emits_error_instead_of_final() {
    let fixture = Fixture::new();
    let (resume, hold) = mpsc::channel();
    let (ready, received) = mpsc::channel();
    let (endpoint, worker) = local_model(
        "200 OK",
        json!({ "choices": [{ "message": { "content": "reply" }, "finish_reason": "stop" }] }),
        Some((ready, hold)),
    );
    let mut child = openai_command(&fixture, &endpoint)
        .args(["--profile", "headless", "--json", "hi"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let session = records(line.as_bytes());
    let id = session[0]["sessionId"].as_str().unwrap();
    received.recv_timeout(Duration::from_secs(10)).unwrap();
    let path = fixture.log(id);
    fs::rename(&path, fixture.root().join("before-final.jsonl")).unwrap();
    fs::create_dir(&path).unwrap();
    resume.send(()).unwrap();
    worker.join().unwrap();

    let mut terminal = String::new();
    stdout.read_to_string(&mut terminal).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let events = records(terminal.as_bytes());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "error");
    assert_eq!(events[0]["code"], "E_SESSION_IO");
    assert_eq!(events[0]["sessionId"], id);
}

#[test]
fn cli_cancel_001_closed_pipe_cancels_a_running_turn() {
    let fixture = Fixture::new();
    let (resume, hold) = mpsc::channel();
    let (ready, received) = mpsc::channel();
    let (endpoint, worker) = local_model(
        "200 OK",
        json!({
            "choices": [{
                "message": { "tool_calls": [{
                    "id": "test-call",
                    "type": "function",
                    "function": { "name": "probe_native", "arguments": "{\"text\":\"hello\"}" }
                }] },
                "finish_reason": "tool_calls"
            }]
        }),
        Some((ready, hold)),
    );
    let mut child = openai_command(&fixture, &endpoint)
        .args(["--profile", "headless", "--json", "hi"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut session = String::new();
    stdout.read_line(&mut session).unwrap();
    let session = records(session.as_bytes());
    assert_eq!(session[0]["type"], "session", "{session:?}");
    received.recv_timeout(Duration::from_secs(10)).unwrap();
    drop(stdout);
    resume.send(()).unwrap();
    worker.join().unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let log = records(&fs::read(fixture.log(session[0]["sessionId"].as_str().unwrap())).unwrap());
    assert_eq!(log.last().unwrap()["payload"]["reason"], "cancelled");
}

#[cfg(unix)]
#[test]
fn cli_cancel_001_sigterm_persists_cancelled_terminal() {
    let fixture = Fixture::new();
    let (resume, hold) = mpsc::channel();
    let (ready, received) = mpsc::channel();
    let (endpoint, worker) = local_model(
        "200 OK",
        json!({ "choices": [{ "message": { "content": "reply" }, "finish_reason": "stop" }] }),
        Some((ready, hold)),
    );
    let mut child = openai_command(&fixture, &endpoint)
        .args(["--profile", "headless", "--json", "hi"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let session = records(line.as_bytes());
    let id = session[0]["sessionId"].as_str().unwrap();
    received.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    resume.send(()).unwrap();
    worker.join().unwrap();
    let mut terminal = String::new();
    stdout.read_to_string(&mut terminal).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        records(terminal.as_bytes())[0]["reason"]["kind"],
        "cancelled"
    );
    let log = records(&fs::read(fixture.log(id)).unwrap());
    assert_eq!(log.last().unwrap()["payload"]["reason"], "cancelled");
}

#[test]
fn cli_session_001_resumes_and_refuses_busy_sessions() {
    let fixture = Fixture::new();
    let first = fixture.run(&["--profile", "headless", "--json", "first"], b"");
    assert_success(&first);
    let events = records(&first.stdout);
    let id = events[0]["sessionId"].as_str().unwrap();
    let path = fixture.log(id);
    let before = fs::read_to_string(&path).unwrap();
    let args = [
        "--profile",
        "headless",
        "--json",
        "--session-id",
        id,
        "next",
    ];

    let writer = try_lock_session(&path).unwrap();
    let busy = fixture.run(&args, b"");
    assert_eq!(busy.status.code(), Some(1));
    assert_eq!(records(&busy.stdout)[0]["code"], "E_SESSION_BUSY");
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    drop(writer);

    let output = fixture.run(&args, b"");
    assert_success(&output);
    let events = records(&output.stdout);
    assert_eq!(events[0]["sessionId"], id);
    assert!(events.last().unwrap()["text"]
        .as_str()
        .unwrap()
        .contains("next"));
    let after = fs::read_to_string(&path).unwrap();
    assert!(after.starts_with(&before));
    assert_eq!(
        records(after.as_bytes())
            .iter()
            .filter(|event| event["type"] == "turn.ended")
            .count(),
        2
    );

    let mut command = fixture.command(Path::new(env!("CARGO_BIN_EXE_dshr")));
    let output = invoke(
        command.env("DSHR_WORKSPACE", fixture.home()).args(args),
        b"",
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(records(&output.stdout)[0]["code"], "E_SESSION_WORKSPACE");
    assert_eq!(fs::read_to_string(&path).unwrap(), after);
}

#[test]
fn cli_profile_001_loads_dsh_manifest_and_reports_patch_origin() {
    let fixture = Fixture::new();
    let profile = fixture.home().join("profiles/review");
    fs::create_dir_all(&profile).unwrap();
    fs::write(profile.join("package.json"), json!({
        "private": true,
        "dsh": { "profile": { "bundles": ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-headless"] } },
    }).to_string()).unwrap();
    let home = fixture.home();
    let first = fixture.root().join("first.yml");
    let last = fixture.root().join("last.yml");
    fs::write(
        profile.join("cordis.patch.yml"),
        "- id: tool-fs\n  disabled: true\n  config:\n    readImage: true\n",
    )
    .unwrap();
    fs::write(
        home.join("cordis.patch.yml"),
        "- id: tool-fs\n  disabled: false\n  config:\n    read: false\n",
    )
    .unwrap();
    fs::write(
        &first,
        "- id: tool-fs\n  disabled: true\n  config:\n    write: false\n",
    )
    .unwrap();
    fs::write(
        &last,
        "- id: tool-fs\n  disabled: false\n  config:\n    edit: false\n",
    )
    .unwrap();
    let layers = [first, last];
    for (count, enabled, config) in [
        (0, true, json!({ "read": false })),
        (1, false, json!({ "write": false })),
        (2, true, json!({ "edit": false })),
    ] {
        let entries = dshr_profile::headless::load(&home, "review", &layers[..count])
            .unwrap()
            .unwrap();
        assert_eq!(entries[0].enabled, enabled);
        assert_eq!(entries[0].config, config);
    }
    let args = [
        "--profile",
        "review",
        "--patch",
        "first.yml",
        "--patch",
        "last.yml",
        "--json",
        "hi",
    ];
    assert_success(&fixture.run(&args, b""));

    for (patch, detail) in [
        ("- id: tool-fs\n  disabled: !!js true\n", "tool-fs"),
        ("- id: absent\n  disabled: true\n", "absent"),
        ("- id: tool-fs\n  config:\n    unknown: true\n", "unknown"),
    ] {
        fs::write(&layers[1], patch).unwrap();
        let result = fixture.run(&args, b"");
        assert_eq!(result.status.code(), Some(1));
        let events = records(&result.stdout);
        assert_eq!(events[0]["code"], "E_CONFIG");
        let message = events[0]["message"].as_str().unwrap();
        assert!(message.contains("last.yml") && message.contains(detail));
    }
}

#[test]
fn cli_tools_001_denies_approval_even_when_environment_allows() {
    let fixture = Fixture::new();
    fixture.install_sidecar("write", "diff");
    let output = invoke(
        fixture
            .command(Path::new(env!("CARGO_BIN_EXE_dshr")))
            .env("DSHR_APPROVAL_MODE", "allow")
            .args(["--profile", "headless", "--json", "工具"]),
        b"",
    );
    assert_success(&output);
    let events = records(&output.stdout);
    let id = events[0]["sessionId"].as_str().unwrap();
    assert_eq!(events[1]["tool"], "write");
    assert_eq!(events[2]["status"], "error");
    let log = records(&fs::read(fixture.log(id)).unwrap());
    let approval = log
        .iter()
        .find(|event| event["type"] == "approval.answered")
        .unwrap();
    assert_eq!(approval["payload"]["outcome"], "deny");
    assert_eq!(approval["payload"]["answerer"], "headless");
}

#[test]
fn cli_tools_excludes_diagnostics_from_the_model_and_dispatch() {
    let fixture = Fixture::new();
    for mode in ["probe", "cordis"] {
        let output = invoke(
            fixture
                .command(Path::new(env!("CARGO_BIN_EXE_dshr")))
                .env("DSHR_SIDECAR", mode)
                .args(["--profile", "headless", "--json", "test"]),
            b"",
        );
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(records(&output.stdout)[0]["code"], "E_CONFIG");
    }

    fixture.install_sidecar("read", "text");
    let (endpoint, worker) = local_model(
        "200 OK",
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "forged-diagnostic",
                        "type": "function",
                        "function": { "name": "probe_fixture", "arguments": "{}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }),
        None,
    );
    let output = invoke(
        openai_command(&fixture, &endpoint).args(["--profile", "headless", "--json", "test"]),
        b"",
    );
    let request = worker.join().unwrap();
    let tools = request["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["function"]["name"], "read");
    let events = records(&output.stdout);
    assert_eq!(events[1]["tool"], "probe_fixture");
    assert_eq!(events[2]["status"], "error");
    let id = events[0]["sessionId"].as_str().unwrap();
    let log = records(&fs::read(fixture.log(id)).unwrap());
    let result = log
        .iter()
        .find(|event| event["type"] == "tool.result")
        .unwrap();
    assert!(result["payload"]["content"]
        .to_string()
        .contains("was not offered"));
}

#[cfg(windows)]
#[test]
fn cli_package_001_console_entry_forwards_stdio_and_exit_status() {
    let fixture = Fixture::new();
    let bundle = fixture.root().join("bundle");
    fs::create_dir_all(bundle.join("bin")).unwrap();
    let launcher = bundle.join("bin/dshr.exe");
    fs::copy(env!("CARGO_BIN_EXE_dshr"), bundle.join("dshr.exe")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_dshr-cli"), &launcher).unwrap();
    let output = invoke(
        fixture.command(&launcher).args(["--profile", "headless"]),
        "输入\n".as_bytes(),
    );
    assert_success(&output);
    assert!(String::from_utf8(output.stdout).unwrap().contains("输入\n"));
    let output = invoke(
        fixture
            .command(&launcher)
            .args(["--profile", "headless", "--json", "--bad"]),
        b"",
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(records(&output.stdout)[0]["code"], "E_USAGE");
    assert!(String::from_utf8_lossy(&output.stderr).contains("E_USAGE"));
}
