use crate::*;

/// 10k 事件性能基线：冷加载 + 回放 + 快照 + 历史重建（M2 gate 2 的数据层）。
/// 生成 10k 事件会话（2500 轮 user/assistant/tool.call/tool.result），供性能自检与帧探针复用。
fn build_perf_log(session_id: &str) -> SessionLog {
    let mut log = SessionLog::new(session_id);
    for index in 0..2_500 {
        let user = format!("第 {index} 轮问题：帮我检查一下这个方案的风险点与回滚方式。");
        let assistant = format!("第 {index} 轮回答：已完成检查，主要风险在并发写入与持久化顺序。");
        log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": format!("u{index}"), "text": user }),
        ))
        .expect("append user");
        log.append(EventSpec::durable(
            "message.assistant.settled",
            "t0",
            json!({ "message_id": format!("a{index}"), "text": assistant }),
        ))
        .expect("append assistant");
        log.append(EventSpec::durable(
            "tool.call",
            "t0",
            json!({
                "call_id": format!("c{index}"),
                "tool": "read",
                "arguments": { "path": "README.md" }
            }),
        ))
        .expect("append tool.call");
        log.append(EventSpec::durable(
            "tool.result",
            "t0",
            json!({
                "call_id": format!("c{index}"),
                "is_error": false,
                "content": [{ "type": "text", "text": "ok" }]
            }),
        ))
        .expect("append tool.result");
    }
    log
}

/// 把 10k 事件会话写进会话目录，返回会话 id（供窗口帧探针/截图复用）。
pub(crate) fn seed_perf_session(dir: &std::path::Path) -> i32 {
    let store = SessionIndex::new(dir);
    if let Err(err) = store.ensure_dir() {
        eprintln!("[seed] 无法创建会话目录: {err}");
        return 1;
    }
    let id = store.new_id();
    if let Err(err) = store.create(&id) {
        eprintln!("[seed] 无法创建会话: {err}");
        return 1;
    }
    let log = build_perf_log(&id);
    let jsonl = log.to_jsonl();
    let path = store.path_for(&id);
    if let Err(err) = std::fs::write(&path, jsonl.as_bytes()) {
        eprintln!("[seed] 写入会话失败: {err}");
        return 1;
    }
    println!(
        "[seed] id={id} events={} bytes={} path={}",
        log.len(),
        jsonl.len(),
        path.display()
    );
    0
}

pub(crate) fn selftest_perf() -> i32 {
    let log = build_perf_log("s-perf");
    assert_eq!(log.len(), 10_000);

    let jsonl = log.to_jsonl();
    let bytes = jsonl.len();
    let started = std::time::Instant::now();
    let reloaded = SessionLog::from_jsonl("s-perf", &jsonl).expect("reload");
    let load_ms = started.elapsed().as_millis();

    let started = std::time::Instant::now();
    let items = build_from_log(&reloaded);
    let build_ms = started.elapsed().as_millis();

    let started = std::time::Instant::now();
    let rows = rows_snapshot(&reloaded, None);
    let snapshot_ms = started.elapsed().as_millis();

    let started = std::time::Instant::now();
    let history = history_for_model(&reloaded);
    let history_ms = started.elapsed().as_millis();

    let cold_ms = load_ms + build_ms;
    println!(
        "[perf] events=10000 jsonl_bytes={bytes} load_ms={load_ms} build_ms={build_ms} \
         snapshot_ms={snapshot_ms} history_ms={history_ms} cold_ms={cold_ms} \
         items={} rows={} history_messages={}",
        items.len(),
        rows.len(),
        history.len()
    );
    let ok = reloaded.len() == 10_000
        && items.len() == 7_500
        && rows.len() == items.len()
        && history.len() <= 60
        && cold_ms < 1_500;
    if ok {
        println!("[selftest-perf] PASS");
        0
    } else {
        println!("[selftest-perf] FAIL");
        1
    }
}
