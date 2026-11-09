//! Sidecar 生命周期与 L1 调用路径的最小实现（`docs/spec/sidecar-rpc.md`）。
//!
//! 目的不是立刻做成生产级 Host，而是把**协议成本**测出来：
//! 进程启动 → 握手 → 注册 → invoke（含 chunk 流）→ 取消 → 崩溃。
//!
//! 传输固定为 NDJSON over stdio：规范允许抽象 transport，探针先用最便宜的一种。

#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use dshr_compat_dsh_protocol::{hello, Envelope, MAX_DEPTH};
use serde_json::{json, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timeout waiting for {0}")]
    Timeout(String),
    #[error("sidecar closed the pipe")]
    Closed,
    #[error("rpc error {code}: {message}")]
    Rpc { code: String, message: String },
}

#[derive(Debug, Clone)]
pub struct Registration {
    pub name: String,
    pub registration_id: String,
    pub plugin_id: String,
}

#[derive(Debug)]
pub struct InvokeOutcome {
    pub chunks: Vec<String>,
    pub terminal: Envelope,
    pub elapsed: Duration,
}

pub struct SidecarHost {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Envelope>,
    generation: String,
    script: PathBuf,
    plugin: PathBuf,
    seq: u64,
    registrations: Vec<Registration>,
    ready: Duration,
}

impl SidecarHost {
    pub fn spawn(script: impl AsRef<Path>, plugin: impl AsRef<Path>) -> Result<Self, HostError> {
        let script = script.as_ref().to_path_buf();
        let plugin = plugin.as_ref().to_path_buf();

        let started = Instant::now();
        let mut child = Command::new("node")
            .arg(&script)
            .arg(&plugin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HostError::Protocol("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HostError::Protocol("no stdout".into()))?;

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(text) => match serde_json::from_str::<Envelope>(&text) {
                        Ok(envelope) => {
                            if tx.send(envelope).is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            eprintln!("[sidecar] bad envelope: {err}: {text}");
                        }
                    },
                    Err(_) => break,
                }
            }
        });

        let mut host = Self {
            child,
            stdin,
            rx,
            generation: "node-pending".to_owned(),
            script,
            plugin,
            seq: 0,
            registrations: Vec::new(),
            ready: Duration::ZERO,
        };

        let hello = hello(&["tool.stream", "tool.cancel"]);
        host.write(&hello)?;
        let ready = host.wait_for("runtime.ready", Duration::from_secs(10))?;
        if let Some(err) = ready.error {
            return Err(HostError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        host.generation = ready.payload["generation"]
            .as_str()
            .unwrap_or("node-0001")
            .to_owned();
        host.ready = started.elapsed();

        host.collect_registrations(Duration::from_secs(2))?;
        Ok(host)
    }

    pub fn ready_latency(&self) -> Duration {
        self.ready
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn registrations(&self) -> &[Registration] {
        &self.registrations
    }

    pub fn script(&self) -> &Path {
        &self.script
    }

    pub fn plugin(&self) -> &Path {
        &self.plugin
    }

    pub fn invoke(
        &mut self,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<InvokeOutcome, HostError> {
        let call_id = self.begin_invoke(tool, arguments)?;
        self.wait_terminal(&call_id, timeout)
    }

    pub fn begin_invoke(&mut self, tool: &str, arguments: Value) -> Result<String, HostError> {
        self.seq += 1;
        let call_id = format!("call-{}", self.seq);
        let request_id = format!("req-invoke-{}", self.seq);
        let envelope = Envelope::host(
            "tool.invoke",
            request_id,
            json!({ "name": tool, "arguments": arguments }),
        )
        .with_generation(self.generation.clone())
        .with_call(call_id.clone())
        .with_session("s-probe");
        self.write(&envelope)?;
        Ok(call_id)
    }

    pub fn cancel(&mut self, call_id: &str) -> Result<(), HostError> {
        self.seq += 1;
        let envelope = Envelope::host(
            "tool.cancel",
            format!("req-cancel-{}", self.seq),
            json!({ "call_id": call_id, "reason": "user" }),
        )
        .with_generation(self.generation.clone())
        .with_call(call_id.to_owned());
        self.write(&envelope)
    }

    /// 读到达终止事件（`tool.result` / `tool.aborted`）为止，收集中间 chunk。
    pub fn wait_terminal(
        &mut self,
        call_id: &str,
        timeout: Duration,
    ) -> Result<InvokeOutcome, HostError> {
        let started = Instant::now();
        let deadline = started + timeout;
        let mut chunks = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HostError::Timeout(format!("tool {call_id}")));
            }
            let envelope = match self.rx.recv_timeout(remaining) {
                Ok(envelope) => envelope,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(HostError::Timeout(format!("tool {call_id}")))
                }
                Err(RecvTimeoutError::Disconnected) => return Err(HostError::Closed),
            };

            if envelope.call_id.as_deref() != Some(call_id) {
                continue;
            }
            match envelope.kind.as_str() {
                "tool.chunk" => {
                    if let Some(text) = envelope.payload.get("text").and_then(Value::as_str) {
                        chunks.push(text.to_owned());
                    }
                }
                "tool.result" | "tool.aborted" => {
                    return Ok(InvokeOutcome {
                        chunks,
                        terminal: envelope,
                        elapsed: started.elapsed(),
                    });
                }
                _ => {}
            }
        }
    }

    pub fn kill(&mut self) -> Result<(), HostError> {
        self.child.kill()?;
        let _ = self.child.wait();
        Ok(())
    }

    fn collect_registrations(&mut self, window: Duration) -> Result<(), HostError> {
        let deadline = Instant::now() + window;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            match self.rx.recv_timeout(remaining) {
                Ok(envelope) if envelope.kind == "tool.register" => {
                    let name = envelope.payload["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    let plugin_id = envelope.plugin_id.clone().unwrap_or_default();
                    self.seq += 1;
                    let registration_id = format!("reg-{}", self.seq);
                    let ack = envelope.reply(
                        "tool.register",
                        json!({ "registration_id": registration_id, "name": name }),
                    );
                    self.write(&ack)?;
                    self.registrations.push(Registration {
                        name,
                        registration_id,
                        plugin_id,
                    });
                }
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => return Ok(()),
                Err(RecvTimeoutError::Disconnected) => return Err(HostError::Closed),
            }
        }
    }

    fn wait_for(&mut self, kind: &str, timeout: Duration) -> Result<Envelope, HostError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HostError::Timeout(kind.to_owned()));
            }
            match self.rx.recv_timeout(remaining) {
                Ok(envelope) if envelope.kind == kind => return Ok(envelope),
                Ok(envelope) if envelope.kind == "runtime.reject" => {
                    return Err(HostError::Protocol(format!(
                        "handshake rejected: {}",
                        envelope.payload
                    )))
                }
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => return Err(HostError::Timeout(kind.to_owned())),
                Err(RecvTimeoutError::Disconnected) => return Err(HostError::Closed),
            }
        }
    }

    fn write(&mut self, envelope: &Envelope) -> Result<(), HostError> {
        assert!(
            envelope.depth <= MAX_DEPTH,
            "depth must respect the spec limit"
        );
        let line = serde_json::to_string(envelope)
            .map_err(|err| HostError::Protocol(format!("serialize: {err}")))?;
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }
}

impl Drop for SidecarHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
