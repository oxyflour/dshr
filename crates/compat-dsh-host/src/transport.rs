use super::*;

/// Windows：GUI 子系统父进程 spawn console 子程序（node）时默认会新开一个控制台窗口；
/// 设置 `CREATE_NO_WINDOW` 让 sidecar 静默运行（release 双击时不再每个会话弹黑框）。
#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW`（winbase.h）。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

/// 侧车进程句柄：普通 spawn 或 Job Object 包裹（Windows 隔离）。
pub(super) enum ChildHandle {
    Plain(Child),
    Guarded(GuardedChild),
}

impl ChildHandle {
    pub(super) fn kill(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(child) => child.kill(),
            Self::Guarded(child) => child.kill(),
        }
    }

    pub(super) fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match self {
            Self::Plain(child) => child.wait(),
            Self::Guarded(child) => child.wait(),
        }
    }

    pub(super) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self {
            Self::Plain(child) => child.try_wait(),
            Self::Guarded(child) => child.try_wait(),
        }
    }
}

impl SidecarHost {
    /// 旧行为：只读侧车（capabilities = tool.stream / tool.cancel）。
    pub fn spawn(script: impl AsRef<Path>, plugin: impl AsRef<Path>) -> Result<Self, HostError> {
        Self::spawn_with(script, plugin, HostOptions::default())
    }

    /// 带能力协商/写裁决的启动入口（chat.rs 用 `HostOptions::with_fs_write` 打开写工具）。
    pub fn spawn_with(
        script: impl AsRef<Path>,
        plugin: impl AsRef<Path>,
        options: HostOptions,
    ) -> Result<Self, HostError> {
        let script = script.as_ref().to_path_buf();
        let plugin = plugin.as_ref().to_path_buf();

        let started = Instant::now();
        let program = options
            .program
            .clone()
            .unwrap_or_else(|| PathBuf::from("node"));
        let mut command = Command::new(&program);
        command
            .arg(&script)
            .arg(&plugin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(dir) = &options.current_dir {
            command.current_dir(dir);
        }
        let mut env = options.env.clone();
        if let Some(service) = &options.fs_write {
            // sidecar 的 fs 根必须与 Rust 裁决层的工作区一致；显式 env 优先。
            if !env
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case("DSHR_WORKSPACE"))
            {
                env.push((
                    "DSHR_WORKSPACE".to_owned(),
                    service.workspace_root().to_string_lossy().into_owned(),
                ));
            }
        }
        if options.guard {
            // Job Object 包裹：环境变量先清洗（去凭据）再叠加显式 env；
            // cwd 也由 guard 统一设置。
            let cwd = options
                .current_dir
                .clone()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            let mut guarded =
                dshr_sidecar_guard::spawn_guarded(command, cwd, &env, &options.guard_limits)
                    .map_err(|err| HostError::Protocol(format!("guarded spawn: {err}")))?;
            let stdin = guarded
                .child_mut()
                .stdin
                .take()
                .ok_or_else(|| HostError::Protocol("no stdin".into()))?;
            let stdout = guarded
                .child_mut()
                .stdout
                .take()
                .ok_or_else(|| HostError::Protocol("no stdout".into()))?;
            return Self::from_parts(
                ChildHandle::Guarded(guarded),
                stdin,
                stdout,
                script,
                plugin,
                options,
                started,
            );
        }
        command.envs(env.iter().map(|(key, value)| (key, value)));
        hide_console_window(&mut command);
        let mut child = command.spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HostError::Protocol("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HostError::Protocol("no stdout".into()))?;

        Self::from_parts(
            ChildHandle::Plain(child),
            stdin,
            stdout,
            script,
            plugin,
            options,
            started,
        )
    }

    /// spawn 之后共用的握手/注册收集流程（guard 与普通路径一致）。
    pub(super) fn from_parts(
        child: ChildHandle,
        stdin: ChildStdin,
        stdout: std::process::ChildStdout,
        script: PathBuf,
        plugin: PathBuf,
        options: HostOptions,
        started: Instant,
    ) -> Result<Self, HostError> {
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
            retired_sessions: HashSet::new(),
            agent_presets_supported: false,
            agent_tool_context_supported: false,
            preparing_step: None,
            preset_definitions: None,
            preset_status: Value::Null,
            preset_bindings: HashMap::new(),
            ready: Duration::ZERO,
            host_caps: options.effective_caps(),
            fs_write: options.fs_write.clone(),
            host_services: options.host_services.clone(),
            negotiated_protocol: PROTOCOL_VERSION,
            host_services_active: false,
            active_calls: HashMap::new(),
            program_tools: None,
            boot_errors: Vec::new(),
        };

        // 规范 §5：generation 由宿主分配，sidecar 只回声。
        static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = format!(
            "node-{:04}",
            NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        // 有 HostServiceRouter 时才宣告 v2；否则与 M2 完全一致（hello.protocol_max = 1）。
        let mut hello = if host.host_services.is_some() {
            hello_with_caps_at(&host.host_caps, PROTOCOL_MAX)
        } else {
            hello_with_caps(&host.host_caps)
        };
        hello.payload["generation"] = json!(generation);
        host.write(&hello)?;
        let ready = host.wait_for("runtime.ready", None, Duration::from_secs(10))?;
        if let Some(err) = ready.error {
            return Err(HostError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        host.generation = generation;
        host.ready = started.elapsed();
        host.apply_negotiation(&ready)?;

        let registration_window = options
            .registration_window
            .unwrap_or(Duration::from_secs(2));
        host.collect_registrations(registration_window)?;
        Ok(host)
    }

    /// 推进 generation（崩溃重启/重连，spec §13.5）：同步给 router，旧 generation 的
    /// 请求会以 `E_SERVICE_UNAVAILABLE` 结算，去重窗口清空。
    pub fn set_generation(&mut self, generation: impl Into<String>) -> Result<(), HostError> {
        self.generation = generation.into();
        if let Some(router) = &self.host_services {
            let mut router = router
                .lock()
                .map_err(|_| HostError::Protocol("host service router poisoned".to_owned()))?;
            router.set_generation(self.generation.clone());
        }
        Ok(())
    }

    /// 应用 `runtime.ready` 的协商结果（spec §3.2/§13.5）。
    pub(super) fn apply_negotiation(&mut self, ready: &Envelope) -> Result<(), HostError> {
        let reported = ready
            .payload
            .get("protocol")
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(PROTOCOL_VERSION)) as u32;
        self.negotiated_protocol = if self.host_services.is_some() {
            reported.clamp(PROTOCOL_VERSION, PROTOCOL_MAX)
        } else {
            reported.clamp(1, PROTOCOL_VERSION)
        };
        self.agent_presets_supported = ready.payload["sidecar_caps"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|cap| cap == "agent.presets"));
        let sidecar_proxy = ready
            .payload
            .get("sidecar_caps")
            .and_then(Value::as_array)
            .map(|caps| {
                caps.iter()
                    .any(|cap| cap.as_str() == Some(CAP_SERVICE_PROXY))
            })
            .unwrap_or(false);
        self.host_services_active =
            self.host_services.is_some() && self.negotiated_protocol >= 2 && sidecar_proxy;
        self.agent_tool_context_supported = self.host_services_active
            && self
                .host_caps
                .iter()
                .any(|cap| cap == CAP_AGENT_TOOL_CONTEXT)
            && ready.payload["sidecar_caps"]
                .as_array()
                .is_some_and(|caps| caps.iter().any(|cap| cap == CAP_AGENT_TOOL_CONTEXT));
        if self.host_services_active {
            if let Some(router) = &self.host_services {
                let mut router = router
                    .lock()
                    .map_err(|_| HostError::Protocol("host service router poisoned".to_owned()))?;
                router.set_generation(self.generation.clone());
            }
        }
        Ok(())
    }

    pub fn kill(&mut self) -> Result<(), HostError> {
        self.child.kill()?;
        let _ = self.child.wait();
        Ok(())
    }

    pub(super) fn process_background_message(
        &mut self,
        envelope: &Envelope,
    ) -> Result<(), HostError> {
        if self.process_tool_lifecycle(envelope)? || self.maybe_serve_fs_write(envelope)? {
            return Ok(());
        }
        if envelope.kind == "log.emit" {
            self.note_log_emit(envelope);
        } else {
            self.maybe_serve_host_service(envelope)?;
        }
        Ok(())
    }

    pub(super) fn wait_for(
        &mut self,
        kind: &str,
        request: Option<&str>,
        timeout: Duration,
    ) -> Result<Envelope, HostError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HostError::Timeout(kind.to_owned()));
            }
            match self.rx.recv_timeout(remaining) {
                Ok(envelope) if envelope.kind == KIND_FS_WRITE_REQUEST => {
                    self.maybe_serve_fs_write(&envelope)?;
                }
                Ok(envelope) if envelope.kind == "log.emit" => {
                    self.note_log_emit(&envelope);
                }
                Ok(envelope)
                    if envelope.kind == kind
                        && request.is_none_or(|id| {
                            envelope.request_id == id
                                && envelope.runtime_generation == self.generation
                        }) =>
                {
                    return Ok(envelope)
                }
                Ok(envelope) if envelope.kind == "runtime.reject" => {
                    return Err(HostError::Protocol(format!(
                        "handshake rejected: {}",
                        envelope.payload
                    )))
                }
                Ok(envelope) => self.process_background_message(&envelope)?,
                Err(RecvTimeoutError::Timeout) => return Err(HostError::Timeout(kind.to_owned())),
                Err(RecvTimeoutError::Disconnected) => return Err(HostError::Closed),
            }
        }
    }

    /// 记录 sidecar 上报的错误日志（`log.emit` level=error），供 UI 插件错误提示。
    pub(super) fn note_log_emit(&mut self, envelope: &Envelope) {
        let level = envelope
            .payload
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("info");
        let message = envelope
            .payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("");
        if level == "error" && !message.is_empty() {
            if self.boot_errors.len() < 8 {
                self.boot_errors.push(message.to_owned());
            }
            eprintln!("[sidecar] {message}");
        }
    }

    pub(super) fn write(&mut self, envelope: &Envelope) -> Result<(), HostError> {
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
        for request_id in self.active_calls.keys() {
            self.release_tool_resources(request_id);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
