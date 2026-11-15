//! 故障注入 fixture（**仅测试用**，不被产品路径加载）。
//!
//! 模拟"恶意/有 bug 的子进程"以验证宿主读写边界（P1-3）：
//!
//! - `fault-huge-line`：完成 `process.ready` 后，收到第一条请求就写出一条
//!   **超过 1 MiB 且不含换行**的帧，然后保持存活（等宿主结算 `E_TOO_LARGE` 并杀 Job）。
//! - `fault-no-stdin`：完成 ready 后**不再读 stdin**；宿主写入大请求会填满管道，
//!   触发写 watchdog（`E_TIMEOUT`）并杀 Job。
//!
//! 行为按**可执行文件名**（`current_exe().file_stem()`）选择，因为宿主
//! `ExternalPlugin::spawn` 只按 manifest entry 启动进程、不传自定义参数。
//! 测试把同一个可执行文件复制成 `fault-huge-line.exe` / `fault-no-stdin.exe` 使用。

use std::io::{BufRead, Write};
use std::time::Duration;

use dshr_plugin_sdk::{
    kind, Envelope, PanelDecl, ReadyPayload, ToolDecl, MAX_FRAME_BYTES, PROTOCOL_VERSION,
};

fn emit(out: &mut impl Write, envelope: &Envelope) {
    if let Ok(line) = envelope.encode_line() {
        let _ = out.write_all(line.as_bytes());
        let _ = out.flush();
    }
}

fn main() {
    let mode = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_default();

    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin);
    let mut out = std::io::stdout();

    // process.hello：读一行，回 process.ready（握手必须成功，故障发生在业务请求上）。
    let mut hello_line = String::new();
    if reader.read_line(&mut hello_line).is_err() {
        return;
    }
    let Ok(hello) = serde_json::from_str::<Envelope>(hello_line.trim()) else {
        return;
    };
    let ready = ReadyPayload {
        protocol: PROTOCOL_VERSION.to_owned(),
        plugin_id: hello
            .payload
            .get("plugin_id")
            .and_then(|value| value.as_str())
            .unwrap_or("fault.fixture")
            .to_owned(),
        generation: hello
            .payload
            .get("generation")
            .and_then(|value| value.as_u64()),
        tools: vec![ToolDecl::new("fault_probe", "fault injection probe")],
        panels: Vec::<PanelDecl>::new(),
    };
    if let Ok(envelope) = Envelope::with_payload(kind::PROCESS_READY, ready) {
        emit(&mut out, &envelope);
    }

    match mode.as_str() {
        "fault-huge-line" => {
            // 等第一条业务请求，然后吐出「无换行的超长帧」。
            let mut request = String::new();
            let _ = reader.read_line(&mut request);
            let mut payload = String::from(
                "{\"version\":\"cpp-process/v1\",\"kind\":\"tool.result\",\"payload\":{\"content\":[{\"type\":\"text\",\"text\":\"",
            );
            payload.push_str(&"x".repeat(MAX_FRAME_BYTES + 64));
            payload.push_str("\"}]}}");
            let _ = out.write_all(payload.as_bytes());
            let _ = out.flush();
            std::thread::sleep(Duration::from_secs(30));
        }
        "fault-no-stdin" => {
            // 永不读 stdin：宿主写大请求会阻塞在管道，等 watchdog 结算。
            std::thread::sleep(Duration::from_secs(30));
        }
        "fault-stderr-huge" => {
            // 16 MiB 单行（无换行，最后才补一个 '\n'）：验证宿主 stderr 读取在分配前截断。
            let stderr = std::io::stderr();
            let mut err = stderr.lock();
            let chunk = [b'E'; 64 * 1024];
            let mut written = 0usize;
            while written < 16 * 1024 * 1024 {
                let _ = err.write_all(&chunk);
                written += chunk.len();
            }
            let _ = err.write_all(b"\n");
            let _ = err.flush();
            // 写完巨行后不读 stdin：宿主调用路径应按写 watchdog 返回 E_TIMEOUT 并杀 Job。
            std::thread::sleep(Duration::from_secs(30));
        }
        "fault-stderr-flood" => {
            // 5000 条短行：验证 tail 的行数/字节双上限只保留最后若干行。
            let stderr = std::io::stderr();
            let mut err = stderr.lock();
            for index in 0..5000 {
                let _ = writeln!(err, "flood-{index}");
            }
            let _ = err.flush();
            std::thread::sleep(Duration::from_secs(30));
        }
        _ => {
            eprintln!("plugin-fault-fixture: unknown mode `{mode}` (expected fault-huge-line / fault-no-stdin / fault-stderr-huge / fault-stderr-flood)");
        }
    }
}
