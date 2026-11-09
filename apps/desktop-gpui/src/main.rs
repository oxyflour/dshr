//! M1 experiment A — GPUI 承载能力的第一个可运行探针。
//!
//! 目标不是"好看的界面"，而是回答一个可判定的问题：
//! **GPUI 能不能在 10k 条事件下保持虚拟化渲染。**
//!
//! 打印的指标（stdout，每 60 帧一次）：
//!  - `rows`：逻辑条目总数（10_000）
//!  - `batch`：本帧实际构造的条目数（应与可视区 + 预取相当，远小于 rows）
//!  - `frames`：累计渲染帧数
//!  - `frame_ms`：距上一帧的毫秒数
//!  - `render_ms`：上一帧在渲染条目上花掉的时间
//!
//! 后续增量（未在本文件内）：中文 IME（需要 EntityInputHandler 自绘输入框）、
//! WebView2 叠层、流式 Markdown。

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, uniform_list, App, Bounds, Context, ScrollStrategy, SharedString,
    UniformListScrollHandle, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;

const ROW_COUNT: usize = 10_000;
const ROW_HEIGHT: f32 = 22.0;

struct BenchApp {
    rows: Vec<SharedString>,
    scroll: UniformListScrollHandle,
    frames: u64,
    last_batch: usize,
    last_render_at: Instant,
    last_frame_ms: f64,
    last_render_ms: f64,
    max_frame_ms: f64,
}

impl BenchApp {
    fn new() -> Self {
        let rows = (0..ROW_COUNT)
            .map(|ix| {
                SharedString::from(format!(
                    "{ix:05}  ·  step {:02}  ·  tool bash  ·  {}  ·  事件回放占位文本",
                    ix % 37,
                    if ix % 7 == 0 { "ok" } else { "done" }
                ))
            })
            .collect();

        Self {
            rows,
            scroll: UniformListScrollHandle::new(),
            frames: 0,
            last_batch: 0,
            last_render_at: Instant::now(),
            last_frame_ms: 0.0,
            last_render_ms: 0.0,
            max_frame_ms: 0.0,
        }
    }
}

impl Render for BenchApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let frame_ms = now.duration_since(self.last_render_at).as_secs_f64() * 1000.0;
        self.last_render_at = now;
        self.frames += 1;
        self.last_frame_ms = frame_ms;
        self.max_frame_ms = self.max_frame_ms.max(frame_ms);

        if self.frames <= 3 || self.frames % 30 == 0 {
            println!(
                "rows={} batch={} frames={} frame_ms={:.2} render_ms={:.2} max_frame_ms={:.2}",
                self.rows.len(),
                self.last_batch,
                self.frames,
                self.last_frame_ms,
                self.last_render_ms,
                self.max_frame_ms
            );
        }

        let header_text = format!(
            "batch {}/{}  ·  frames {}  ·  last frame {:.1}ms  ·  render {:.2}ms  ·  max {:.1}ms",
            self.last_batch,
            self.rows.len(),
            self.frames,
            self.last_frame_ms,
            self.last_render_ms,
            self.max_frame_ms
        );

        let scroll_bottom = self.scroll.clone();
        let scroll_top = self.scroll.clone();
        let rows = self.rows.clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x141414))
            .text_color(rgb(0xd6d6d6))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .h(px(52.0))
                    .px_4()
                    .border_b_1()
                    .border_color(rgb(0x2a2a2a))
                    .child(div().text_sm().child(header_text))
                    .child(
                        div()
                            .id("jump-bottom")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(0x243a5e))
                            .cursor_pointer()
                            .child("跳到底部")
                            .on_click(move |_, window, _cx| {
                                scroll_bottom.scroll_to_item(ROW_COUNT - 1, ScrollStrategy::Top);
                                window.refresh();
                            }),
                    )
                    .child(
                        div()
                            .id("jump-top")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(0x243a5e))
                            .cursor_pointer()
                            .child("回到顶部")
                            .on_click(move |_, window, _cx| {
                                scroll_top.scroll_to_item(0, ScrollStrategy::Top);
                                window.refresh();
                            }),
                    ),
            )
            .child(
                div().flex_1().overflow_hidden().child(
                    uniform_list(
                        "event-rows",
                        rows.len(),
                        cx.processor(
                            move |this: &mut BenchApp,
                                  range: std::ops::Range<usize>,
                                  _window,
                                  _cx| {
                                this.last_batch = range.len();
                                let started = Instant::now();
                                let mut items = Vec::with_capacity(range.len());
                                for ix in range {
                                    let text = rows.get(ix).cloned().unwrap_or_default();
                                    items.push(
                                        div()
                                            .id(ix)
                                            .h(px(ROW_HEIGHT))
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .text_sm()
                                            .when(ix % 2 == 0, |el| el.bg(rgb(0x191919)))
                                            .child(text)
                                            .into_any_element(),
                                    );
                                }
                                this.last_render_ms = started.elapsed().as_secs_f64() * 1000.0;
                                items
                            },
                        ),
                    )
                    .track_scroll(&self.scroll)
                    .h_full(),
                ),
            )
            .child(
                div()
                    .h(px(28.0))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(rgb(0x2a2a2a))
                    .text_xs()
                    .child("M1-A · 滚轮滚动验证虚拟化：batch 应保持在可视区数量级，而不是 10000"),
            )
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| BenchApp::new()),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
