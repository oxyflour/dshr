use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dshr_session_store::try_lock_session;
use serde_json::{json, Value};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("dshr-headless-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("DSHR_") {
                command.env_remove(name);
            }
        }
        command
            .current_dir(&self.root)
            .env("DSHR_HOME", self.home())
            .env("DSHR_WORKSPACE", &self.root)
            .env("DSHR_PROVIDER", "mock")
            .env("DSHR_APP_ROOT", self.root.join("no-sidecar"));
        command
    }

    fn run(&self, args: &[&str], stdin: &[u8]) -> Output {
        invoke(
            &mut self
                .command(Path::new(env!("CARGO_BIN_EXE_dshr")))
                .args(args),
            stdin,
        )
    }

    fn log(&self, id: &str) -> PathBuf {
        self.home().join("sessions").join(format!("{id}.jsonl"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
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
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr().unwrap();
    server.set_nonblocking(true).unwrap();
    let status = status.to_owned();
    let worker = thread::spawn(move || {
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
        let response = response.to_string();
        write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        serde_json::from_slice(&request).unwrap()
    });
    (format!("http://{address}"), worker)
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

#[test]
fn cli_input_001_uses_clean_text_and_preserves_stdin() {
    let fixture = Fixture::new();
    let output = fixture.run(&["--profile", "headless", "--json", "--help"], b"");
    assert_success(&output);
    assert!(output.stdout.starts_with(b"Usage: "));
    assert!(!fixture.home().exists());

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
    let output = fixture.run(&["--profile", "headless", "--json", "原生"], b"");
    assert_success(&output);
    let events = records(&output.stdout);
    assert_eq!(
        events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["session", "tool_call", "tool_result", "final"]
    );
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
    fs::rename(&path, fixture.root.join("before-final.jsonl")).unwrap();
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
    assert_eq!(session[0]["type"], "session");
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
    let first = fixture.root.join("first.yml");
    let last = fixture.root.join("last.yml");
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
    assert_eq!(events[1]["tool"], "probe_write");
    assert_eq!(events[2]["status"], "error");
    let log = records(&fs::read(fixture.log(id)).unwrap());
    let approval = log
        .iter()
        .find(|event| event["type"] == "approval.answered")
        .unwrap();
    assert_eq!(approval["payload"]["outcome"], "deny");
    assert_eq!(approval["payload"]["answerer"], "headless");
}

#[cfg(windows)]
#[test]
fn cli_package_001_console_entry_forwards_stdio_and_exit_status() {
    let fixture = Fixture::new();
    let bundle = fixture.root.join("bundle");
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
