//! M1 端到端回放：Session Log（JSONL）→ conversation-model → GPUI 列表。
//!
//! 自证部分：启动前先断言模型统计，并把同一份 Log 回放两次做逐项比较；
//! 任何不一致都会在开窗前 panic，而不是靠人眼发现。
//!
//! 运行：`cargo run -p dshr-desktop-gpui --bin conversation`

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, uniform_list, App, Bounds, Context, SharedString, UniformListScrollHandle,
    Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;

use dshr_conversation_model::{stats, CardKind, Item, ToolCard, DEMO_SESSION};
use dshr_session_store::ToolState;

const ROW_HEIGHT: f32 = 58.0;

type Row = (SharedString, Option<SharedString>);

struct ConversationApp {
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
    frames: u64,
    last_render_at: Instant,
}

fn describe(item: &Item) -> Row {
    match item {
        Item::User { text, attachments } => (
            SharedString::from(format!("USER    · {text}")),
            (!attachments.is_empty()).then(|| {
                let names: Vec<_> = attachments.iter().map(|file| file.name.as_str()).collect();
                SharedString::from(names.join(" · "))
            }),
        ),
        Item::Assistant { text } => (SharedString::from(format!("AGENT   · {text}")), None),
        Item::Tool(tool) => {
            let state = match &tool.state {
                ToolState::Pending => "pending".to_owned(),
                ToolState::Settled { is_error } => {
                    if *is_error {
                        "error".to_owned()
                    } else {
                        "ok".to_owned()
                    }
                }
                ToolState::Aborted { reason } => format!("aborted({reason})"),
            };
            (
                SharedString::from(format!(
                    "TOOL    · {} [{}] · {state}",
                    tool.tool, tool.origin
                )),
                tool.card
                    .as_ref()
                    .map(|card| SharedString::from(card.summary())),
            )
        }
        Item::Compaction { from_seq, to_seq } => (
            SharedString::from(format!(
                "CONTEXT · 已压缩 seq {from_seq}–{to_seq}（原文仍在 Log 中）"
            )),
            None,
        ),
    }
}

fn demo_items() -> Vec<Item> {
    dshr_conversation_model::build(DEMO_SESSION, "s-demo").expect("demo session must be valid")
}

fn demo_cards(items: &[Item]) -> Vec<ToolCard> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Tool(tool) => tool.card.clone(),
            _ => None,
        })
        .collect()
}

/// 回放日志、跑自证断言、返回可渲染的行。
///
/// 断言失败时在开窗之前 panic —— 不允许出现"窗口开了但内容不对"的状态。
fn load_rows() -> Vec<Row> {
    let items = demo_items();
    let stats = stats(&items);

    assert_eq!(stats.total, 6, "demo 会话应有 6 个展示项");
    assert_eq!(stats.user, 1);
    assert_eq!(stats.assistant, 2);
    assert_eq!(stats.tools, 2);
    assert_eq!(stats.settled, 1);
    assert_eq!(stats.aborted, 1);
    assert_eq!(stats.compaction, 1);

    // 卡片断言：结果不能只存在于 JSONL 里，必须折叠成可渲染内容。
    let cards = demo_cards(&items);
    assert_eq!(cards.len(), 2, "两个工具卡片都应有结果视图");
    assert_eq!(cards[0].kind, CardKind::Terminal);
    assert_eq!(cards[0].title, "exit 0");
    assert_eq!(cards[0].preview, vec!["main.rs".to_owned()]);
    assert_eq!(cards[1].kind, CardKind::Error);
    assert_eq!(cards[1].title, "已中止");

    let again = demo_items();
    assert_eq!(items, again, "同一份 JSONL 必须回放出相同模型");
    assert_eq!(cards, demo_cards(&again), "卡片也必须确定性折叠");

    println!(
        "[model] total={} user={} assistant={} tools={} settled={} aborted={} compaction={}",
        stats.total,
        stats.user,
        stats.assistant,
        stats.tools,
        stats.settled,
        stats.aborted,
        stats.compaction
    );
    for card in &cards {
        println!("[card] {}", card.summary());
    }

    items.iter().map(describe).collect()
}

impl ConversationApp {
    fn new(rows: Vec<Row>) -> Self {
        Self {
            rows,
            scroll: UniformListScrollHandle::new(),
            frames: 0,
            last_render_at: Instant::now(),
        }
    }
}

impl Render for ConversationApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let frame_ms = now.duration_since(self.last_render_at).as_secs_f64() * 1000.0;
        self.last_render_at = now;
        self.frames += 1;
        if self.frames <= 3 {
            println!(
                "[ui] frame={} items={} frame_ms={:.1}",
                self.frames,
                self.rows.len(),
                frame_ms
            );
        }

        let rows = self.rows.clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x141414))
            .text_color(rgb(0xd6d6d6))
            .child(
                div()
                    .h(px(48.0))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(rgb(0x2a2a2a))
                    .text_sm()
                    .child(format!(
                        "M1 回放 · s-demo · {} 项（消息 / 工具卡片 / 压缩占位）",
                        self.rows.len()
                    )),
            )
            .child(
                div().flex_1().overflow_hidden().child(
                    uniform_list(
                        "conversation",
                        rows.len(),
                        cx.processor(
                            move |_this: &mut ConversationApp,
                                  range: std::ops::Range<usize>,
                                  _w,
                                  _c| {
                                let mut items = Vec::with_capacity(range.len());
                                for ix in range {
                                    let (main, sub) = rows
                                        .get(ix)
                                        .cloned()
                                        .unwrap_or((SharedString::default(), None));
                                    let tone = if main.starts_with("TOOL") {
                                        rgb(0x243a5e)
                                    } else if main.starts_with("CONTEXT") {
                                        rgb(0x3a2f24)
                                    } else {
                                        rgb(0x1b1b1b)
                                    };
                                    let mut cell = div()
                                        .id(ix)
                                        .h(px(ROW_HEIGHT))
                                        .px_4()
                                        .flex()
                                        .flex_col()
                                        .justify_center()
                                        .border_b_1()
                                        .border_color(rgb(0x222222))
                                        .when(ix % 2 == 1, |el| el.bg(tone))
                                        .child(div().text_sm().child(main));
                                    if let Some(sub) = sub {
                                        cell = cell.child(
                                            div().text_xs().text_color(rgb(0x9aa7bd)).child(sub),
                                        );
                                    }
                                    items.push(cell.into_any_element());
                                }
                                items
                            },
                        ),
                    )
                    .track_scroll(&self.scroll)
                    .h_full(),
                ),
            )
    }
}

fn main() {
    // 先完成回放与自证，再开窗。
    let rows = load_rows();

    // `--check`：只跑回放断言、不开窗（CI / 无显示器环境）。
    if std::env::args().any(|arg| arg == "--check") {
        println!("[check] rows={} PASS", rows.len());
        return;
    }

    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.0), px(640.0)), cx);
        let rows = rows.clone();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_, cx| cx.new(move |_| ConversationApp::new(rows.clone())),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
