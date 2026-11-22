#![cfg(test)]

use super::*;
use std::io::Read;

pub(super) struct OfficialScript(pub(super) VecDeque<ModelReply>);

impl ModelProvider for OfficialScript {
    fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
        Ok(self.0.pop_front().expect("unexpected model request"))
    }
}

pub(super) fn model_tools(calls: &[(&str, &str, serde_json::Value)]) -> ModelReply {
    let mut reply = ModelReply::text_only("");
    reply.tool_calls = calls
        .iter()
        .map(|(id, name, args)| ModelToolCall {
            id: (*id).to_owned(),
            name: (*name).to_owned(),
            arguments: args.clone(),
        })
        .collect();
    reply
}

pub(super) fn stage5_runtime(path: &Path, plugins: &[&str]) -> PathBuf {
    let workspace = path.join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sidecar = PathBuf::from(std::env::var_os("DSHR_OFFICIAL_RC5_DIR").unwrap());
    std::env::set_var("DSHR_APP_ROOT", sidecar.parent().unwrap().parent().unwrap());
    std::env::set_var("DSHR_HOME", path.join("home"));
    std::env::set_var("DSHR_WORKSPACE", &workspace);
    std::env::set_var("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_PROVIDER", "mock");
    std::env::set_var("DSHR_PERMISSION_MODE", "workspace-write");
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    std::env::set_var(
        "DSHR_DSH_PLUGINS",
        json!(plugins
            .iter()
            .map(|name| {
                let name = *name;
                let mut entry = json!({"name": format!("@deepseek-ai/dsh-tool-{name}")});
                if name == "subagent" {
                    entry["config"] = json!({"provider":"local","maxDepth":2});
                }
                entry
            })
            .collect::<Vec<_>>())
        .to_string(),
    );
    workspace
}

pub(super) fn logged_tool(log: &SessionLog, id: &str) -> serde_json::Value {
    log.events()
        .iter()
        .find(|event| event.event_type == "tool.result" && event.payload["call_id"] == id)
        .unwrap_or_else(|| panic!("missing result for {id}; {:?}", log.events()))
        .payload
        .clone()
}

pub(super) fn read_child_model_request(stream: &mut std::net::TcpStream) -> serde_json::Value {
    use std::io::{BufRead, BufReader};
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

pub(super) fn write_child_model_text(
    stream: &mut std::net::TcpStream,
    text: &str,
) -> std::io::Result<()> {
    write_child_model_reply(stream, json!({"role":"assistant","content":text}))
}

pub(super) fn write_child_model_reply(
    stream: &mut std::net::TcpStream,
    message: serde_json::Value,
) -> std::io::Result<()> {
    use std::io::Write;
    let body = json!({"choices":[{"index":0,"message":message}]}).to_string();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

pub(super) fn accept_child_model(server: &std::net::TcpListener) -> std::net::TcpStream {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        match server.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("child model did not receive a request: {error}"),
        }
    }
}
