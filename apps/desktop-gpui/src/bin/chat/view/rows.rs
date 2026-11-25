use crate::*;

/// 会话流里的一行。字段全部是 `String`：快照跨线程从 Agent 线程送到 UI 线程。
///
/// 这是"数据"而不是"样式"：渲染时再按 DSH 的对话样式组合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Row {
    User {
        text: String,
        attachments: Vec<UserAttachment>,
    },
    Assistant(String),
    Streaming(String),
    ChatNode(dshr_conversation_model::ChatNode),
    Process(Vec<Row>),
    Reasoning {
        text: String,
        streaming: bool,
    },
    Tool {
        call_seq: u64,
        name: String,
        origin: String,
        status: String,
        detail: String,
        is_error: bool,
        pending: bool,
        /// 结构化卡片（Diff/Terminal 的原生视图从这里取正文）。
        card: Option<Box<ToolCard>>,
        delivered_files: Vec<dsh_deliverables::DeliveredFile>,
    },
    Meta(String),
    Stopped,
}

impl Row {
    pub(crate) fn has_tool_call(&self, seq: u64) -> bool {
        match self {
            Self::Tool { call_seq, .. } => *call_seq == seq,
            Self::Process(steps) => steps.iter().any(|step| step.has_tool_call(seq)),
            _ => false,
        }
    }

    pub(crate) fn main_text(&self) -> String {
        match self {
            Self::User { text, .. } => format!("你 · {text}"),
            Self::Assistant(text) | Self::Streaming(text) => format!("助手 · {text}"),
            Self::Process(_) => "思考过程".to_owned(),
            Self::ChatNode(node) => node.fallback.clone(),
            Self::Reasoning { text, .. } => text.clone(),
            Self::Tool {
                name,
                origin,
                status,
                ..
            } => format!("工具 · {name} [{origin}] · {status}"),
            Self::Meta(text) => text.clone(),
            Self::Stopped => "已停止".to_owned(),
        }
    }

    pub(crate) fn sub_text(&self) -> Option<String> {
        match self {
            Self::Tool { detail, .. } if !detail.is_empty() => Some(detail.clone()),
            _ => None,
        }
    }
}

pub(crate) fn is_streaming_row(row: &Row) -> bool {
    matches!(row, Row::Streaming(_))
}

pub(crate) fn turn_preview(row: &Row) -> Option<&str> {
    let Row::User { text, attachments } = row else {
        return None;
    };
    Some(
        text.lines()
            .find(|line| !line.trim().is_empty())
            .or_else(|| attachments.first().map(|file| file.name.as_str()))
            .unwrap_or_default(),
    )
}

pub(crate) fn turn_rail_window(total: usize, active: usize) -> std::ops::Range<usize> {
    let count = total.min(7);
    let start = active.saturating_sub(count / 2).min(total - count);
    start..start + count
}

/// 估计一行的高度（逻辑像素）。用于虚拟化的滚动占位；估算偏差只影响滚动条比例，
/// 不影响可见行之间的真实排版。按 CJK 宽度保守估计每行字符数。
pub(crate) fn trial_panel_max_height(window: &Window) -> gpui::Pixels {
    (window.viewport_size().height * 0.42).clamp(px(90.0), px(360.0))
}

pub(crate) fn estimated_row_height(row: &Row) -> f32 {
    const LINE: f32 = 23.0;
    match row {
        Row::User { text, attachments } => {
            let chars = text.chars().count() as f32;
            let text_height = if text.is_empty() {
                0.0
            } else {
                (chars / 28.0).ceil() * LINE + 43.0
            };
            let image_height = if attachments.len() == 1 {
                144.0_f32
            } else {
                80.0
            };
            let attachment_height: f32 = attachments
                .chunks(2)
                .map(|row| {
                    if row
                        .iter()
                        .any(|attachment| attachment.file.as_ref().is_some_and(stored_image))
                    {
                        image_height + 6.0
                    } else {
                        48.0
                    }
                })
                .sum();
            text_height + attachment_height
        }
        Row::Assistant(text) | Row::Streaming(text) => {
            let chars = text.chars().count() as f32;
            let actions = if is_streaming_row(row) {
                22.0
            } else if text.is_empty() {
                0.0
            } else {
                28.0
            };
            (chars / 36.0).ceil().max(1.0) * LINE + 12.0 + actions
        }
        Row::Tool { detail, .. } => {
            let chars = detail.chars().count() as f32;
            46.0 + (chars / 60.0).ceil() * 18.0
        }
        Row::Process(_) => 26.0,
        Row::Reasoning { text, .. } => text
            .lines()
            .map(|line| (line.chars().count() as f32 / 42.0).ceil().max(1.0) * LINE)
            .sum(),
        Row::Meta(_) | Row::Stopped => 30.0,
        Row::ChatNode(node) => (node.fallback.chars().count() as f32 / 36.0).ceil() * LINE + 12.0,
    }
}

pub(crate) fn streaming_text(row: &Row) -> &str {
    match row {
        Row::Streaming(text) => text,
        _ => "",
    }
}

pub(crate) fn describe(item: &Item) -> Row {
    match item {
        Item::User { text, attachments } => Row::User {
            text: text.clone(),
            attachments: attachments.clone(),
        },
        Item::Assistant { text } => Row::Assistant(text.clone()),
        Item::Reasoning(text) => Row::Reasoning {
            text: text.clone(),
            streaming: false,
        },
        Item::Stopped => Row::Stopped,
        Item::ChatNode(node) => Row::ChatNode(node.clone()),
        Item::Tool(tool) => {
            let (status, pending) = match &tool.state {
                ToolState::Pending => ("运行中".to_owned(), true),
                ToolState::Settled { is_error } => {
                    if *is_error {
                        ("失败".to_owned(), false)
                    } else {
                        ("完成".to_owned(), false)
                    }
                }
                ToolState::Aborted { reason } => (format!("中止 · {reason}"), false),
            };
            Row::Tool {
                call_seq: tool.call_seq,
                name: tool.tool.clone(),
                origin: tool.origin.clone(),
                status,
                detail: tool
                    .card
                    .as_ref()
                    .map(ToolCard::summary)
                    .unwrap_or_default(),
                is_error: matches!(tool.state, ToolState::Settled { is_error: true }),
                pending,
                card: tool.card.clone().map(Box::new),
                delivered_files: Vec::new(),
            }
        }
        Item::Compaction { from_seq, to_seq } => {
            Row::Meta(format!("上下文已压缩 · seq {from_seq}–{to_seq}"))
        }
    }
}

/// 从当前日志生成整份快照；`streaming` 非空时把"正在生成的回复"追加为最后一行。
///
/// 快照而不是增量：UI 只做覆盖，不用维护事件合并逻辑，也不会因为丢一条消息就错位。
pub(crate) fn rows_snapshot(log: &SessionLog, streaming: Option<&str>) -> Vec<Row> {
    rows_with_preview(log, streaming, None)
}

pub(crate) fn rows_with_preview(
    log: &SessionLog,
    streaming: Option<&str>,
    reasoning: Option<&str>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut steps = Vec::new();
    let mut deliveries = dsh_deliverables::replay(log);
    for item in build_from_log(log) {
        let mut row = describe(&item);
        if let Item::Tool(tool) = &item {
            if tool.origin == "sidecar"
                && tool.tool == "present"
                && matches!(tool.state, ToolState::Settled { is_error: false })
            {
                if let Row::Tool {
                    delivered_files, ..
                } = &mut row
                {
                    *delivered_files = deliveries.remove(&tool.call_seq).unwrap_or_default();
                }
            }
        }
        if matches!(row, Row::Tool { .. } | Row::Reasoning { .. }) {
            steps.push(row);
        } else {
            if !steps.is_empty() {
                rows.push(Row::Process(std::mem::take(&mut steps)));
            }
            rows.push(row);
        }
    }
    if let Some(text) = reasoning.filter(|text| !text.is_empty()) {
        steps.push(Row::Reasoning {
            text: text.to_owned(),
            streaming: true,
        });
    }
    if !steps.is_empty() {
        rows.push(Row::Process(steps));
    }
    if let Some(text) = streaming.filter(|text| !text.is_empty()) {
        rows.push(Row::Streaming(text.to_owned()));
    }
    rows
}
