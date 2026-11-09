//! M1 experiment A — GPUI 承载能力探针。
//!
//! 当前目标：**拆解启动尖峰**（首帧后出现 1000–2000 ms 的空档）。
//!
//! 通过 `PROBE_MODE` 环境变量切换变体，用来分离三个嫌疑：
//!
//! | mode | 行数 | 行内文本 | 用来排除 |
//! |---|---|---|---|
//! | `empty` | 0 | — | 只剩窗口 + GPU 初始化 + 头部文本 |
//! | `no_text` | 10000 | 无 | 列表布局开销（不含文字 shaping） |
//! | `ascii` | 10000 | ASCII | 中文/CJK 字体回退扫描 |
//! | `full` | 10000 | 中文 | 基线（默认） |
//!
//! 输出：每帧 `t_ms`（进程启动以来的毫秒数）+ 帧间隔 + 行构造耗时。
//! 判定方式：比较四档的 `t_ms` 序列——尖峰出现在哪一档，就说明开销来自哪一层。

use std::sync::OnceLock;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, uniform_list, App, Bounds, Context, ScrollStrategy, SharedString,
    UniformListScrollHandle, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;

const ROW_COUNT: usize = 10_000;
const ROW_HEIGHT: f32 = 22.0;

static START: OnceLock<Instant> = OnceLock::new();

fn elapsed_ms() -> f64 {
    START
        .get()
        .map(|start| start.elapsed().as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Empty,
    NoText,
    Ascii,
    Full,
}

impl Mode {
    fn from_env() -> Self {
        match std::env::var("PROBE_MODE").as_deref() {
            Ok("empty") => Self::Empty,
            Ok("no_text") => Self::NoText,
            Ok("ascii") => Self::Ascii,
            _ => Self::Full,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::NoText => "no_text",
            Self::Ascii => "ascii",
            Self::Full => "full",
        }
    }

    fn rows(self) -> usize {
        match self {
            Self::Empty => 0,
            _ => ROW_COUNT,
        }
    }

    fn show_text(self) -> bool {
        !matches!(self, Self::NoText | Self::Empty)
    }

    fn cjk(self) -> bool {
        self == Self::Full
    }
}

struct BenchApp {
    mode: Mode,
    rows: Vec<SharedString>,
    scroll: UniformListScrollHandle,
    frames: u64,
    last_batch: usize,
    last_render_at: Instant,
    last_frame_ms: f64,
    last_render_ms: f64,
    max_frame_ms: f64,
    anim: bool,
    dts: Vec<f64>,
}

impl BenchApp {
    fn new(mode: Mode) -> Self {
        let anim = std::env::var("PROBE_ANIM").as_deref() == Ok("1");
        let built_at = Instant::now();
        let rows = (0..mode.rows())
            .map(|ix| {
                SharedString::from(if mode.cjk() {
                    format!(
                        "{ix:05}  ·  step {:02}  ·  tool bash  ·  事件回放占位文本",
                        ix % 37
                    )
                } else {
                    format!(
                        "{ix:05}  ·  step {:02}  ·  tool bash  ·  replay placeholder",
                        ix % 37
                    )
                })
            })
            .collect();
        println!(
            "[boot] mode={} rows={} rows_built_ms={:.2} t={:.2}",
            mode.name(),
            mode.rows(),
            built_at.elapsed().as_secs_f64() * 1000.0,
            elapsed_ms()
        );

        Self {
            mode,
            rows,
            scroll: UniformListScrollHandle::new(),
            frames: 0,
            last_batch: 0,
            last_render_at: Instant::now(),
            last_frame_ms: 0.0,
            last_render_ms: 0.0,
            max_frame_ms: 0.0,
            anim,
            dts: Vec::new(),
        }
    }
}

impl Render for BenchApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let frame_ms = now.duration_since(self.last_render_at).as_secs_f64() * 1000.0;
        self.last_render_at = now;
        self.frames += 1;
        self.last_frame_ms = frame_ms;
        self.max_frame_ms = self.max_frame_ms.max(frame_ms);
        if self.frames > 2 {
            self.dts.push(frame_ms);
        }

        if self.anim {
            window.request_animation_frame();
            if self.frames >= 122 {
                let mut sorted = self.dts.clone();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let pct = |p: f64| {
                    if sorted.is_empty() {
                        0.0
                    } else {
                        sorted[((sorted.len() - 1) as f64 * p) as usize]
                    }
                };
                println!(
                    "[anim] mode={} frames={} p50={:.2} p95={:.2} max={:.2} render_ms={:.2}",
                    self.mode.name(),
                    self.frames,
                    pct(0.50),
                    pct(0.95),
                    pct(1.0),
                    self.last_render_ms
                );
                std::process::exit(0);
            }
        }

        if self.frames <= 5 || self.frames % 10 == 0 {
            println!(
                "frame={:3} t_ms={:8.2} dt_ms={:8.2} batch={} render_ms={:.2}",
                self.frames,
                elapsed_ms(),
                self.last_frame_ms,
                self.last_batch,
                self.last_render_ms
            );
        }

        let header_text = format!(
            "mode={} batch={}/{} frames={} last={:.1}ms render={:.2}ms max={:.1}ms",
            self.mode.name(),
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
        let show_text = self.mode.show_text();

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
                            .child("bottom")
                            .on_click(move |_, window, _cx| {
                                scroll_bottom
                                    .scroll_to_item(ROW_COUNT.max(1) - 1, ScrollStrategy::Top);
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
                            .child("top")
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
                                    let mut row = div()
                                        .id(ix)
                                        .h(px(ROW_HEIGHT))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .text_sm()
                                        .when(ix % 2 == 0, |el| el.bg(rgb(0x191919)));
                                    if show_text {
                                        let text = rows.get(ix).cloned().unwrap_or_default();
                                        row = row.child(text);
                                    }
                                    items.push(row.into_any_element());
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
                    .child("M1-A · startup spike probe"),
            )
    }
}

fn main() {
    START.set(Instant::now()).ok();
    let mode = Mode::from_env();

    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        let opened_at = Instant::now();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| BenchApp::new(mode)),
        )
        .expect("failed to open window");
        println!(
            "[boot] window_opened_ms={:.2} t={:.2}",
            opened_at.elapsed().as_secs_f64() * 1000.0,
            elapsed_ms()
        );
        cx.activate(true);
    });
}
