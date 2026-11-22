use crate::*;

/// 滚动帧探针：用 GPUI profiler 的 `FrameTiming` 记录真实帧耗时与呈现间隔。
///
/// 由 `DSHR_FRAME_PROBE=<帧数>` 开启：每帧推进滚动位置、请求下一帧，达到目标帧数后
/// 把统计写到 `DSHR_FRAME_LOG`（默认 `.tmp/frame-probe.txt`）并按 `DSHR_FRAME_QUIT`
/// （默认 1）退出。这是 Gate 2 的测量手段：时间戳来自渲染管线，不是伪造的 sleep。
pub(crate) struct FrameProbe {
    target: usize,
    dir: f32,
    scroll_pos: f32,
    step: f32,
    collector: FrameTimingCollector,
    draw_work_ns: Vec<u64>,
    layout_ns: Vec<u64>,
    prepaint_ns: Vec<u64>,
    paint_ns: Vec<u64>,
    present_interval_ns: Vec<u64>,
    refresh_ns: Option<u64>,
    viewport: Option<(f32, f32)>,
    scale: f32,
    scroll_max: f32,
    rows: usize,
    started: std::time::Instant,
    pub(super) log_path: PathBuf,
    pub(super) quit: bool,
    ticks: usize,
    debug: bool,
}

impl FrameProbe {
    pub(crate) fn from_env() -> Option<Self> {
        let target = std::env::var("DSHR_FRAME_PROBE")
            .ok()?
            .parse::<usize>()
            .ok()?;
        if target == 0 {
            return None;
        }
        let step = std::env::var("DSHR_FRAME_STEP")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(0.01);
        let log_path = std::env::var("DSHR_FRAME_LOG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(".tmp/frame-probe.txt"));
        let quit = std::env::var("DSHR_FRAME_QUIT")
            .map(|value| value != "0")
            .unwrap_or(true);
        Some(Self {
            target,
            dir: 1.0,
            scroll_pos: 0.0,
            step,
            collector: FrameTimingCollector::new(),
            draw_work_ns: Vec::new(),
            layout_ns: Vec::new(),
            prepaint_ns: Vec::new(),
            paint_ns: Vec::new(),
            present_interval_ns: Vec::new(),
            refresh_ns: None,
            viewport: None,
            scale: 1.0,
            scroll_max: 0.0,
            rows: 0,
            started: std::time::Instant::now(),
            log_path,
            quit,
            ticks: 0,
            debug: std::env::var("DSHR_FRAME_DEBUG").is_ok(),
        })
    }

    /// 收集上一帧的 profiler 事件并推进滚动；返回 true 表示达到目标帧数。
    pub(crate) fn tick(
        &mut self,
        scroll: &ScrollHandle,
        viewport: gpui::Size<gpui::Pixels>,
        scale: f32,
        rows: usize,
    ) -> bool {
        for event in self.collector.collect_unseen() {
            match event {
                FrameEvent::Draw(timing) => {
                    self.draw_work_ns.push(
                        timing
                            .draw_end
                            .saturating_duration_since(timing.draw_start)
                            .as_nanos() as u64,
                    );
                    self.layout_ns
                        .push(timing.phases.request_layout.as_nanos() as u64);
                    self.prepaint_ns
                        .push(timing.phases.prepaint.as_nanos() as u64);
                    self.paint_ns.push(timing.phases.paint.as_nanos() as u64);
                    if self.refresh_ns.is_none() {
                        self.refresh_ns = timing.refresh_interval.map(|d| d.as_nanos() as u64);
                    }
                }
                FrameEvent::Present(present) => {
                    if let Some(interval) = present.animation_interval {
                        self.present_interval_ns.push(interval.as_nanos() as u64);
                    }
                }
            }
        }
        // 滚动范围按当前窗口（逻辑像素）计算；看不到内容时仍以 1px 兜底，避免除零。
        let max = scroll.max_offset().y.as_f32().max(1.0);
        self.scroll_pos = (self.scroll_pos + self.dir * self.step).clamp(0.0, 1.0);
        if self.scroll_pos >= 1.0 {
            self.dir = -1.0;
        }
        if self.scroll_pos <= 0.0 {
            self.dir = 1.0;
        }
        scroll.set_offset(point(px(0.0), px(-self.scroll_pos * max)));
        self.viewport = Some((viewport.width.as_f32(), viewport.height.as_f32()));
        self.scale = scale;
        self.scroll_max = max;
        self.rows = rows;
        self.ticks += 1;
        if self.debug && self.ticks.is_multiple_of(60) {
            eprintln!(
                "[probe] ticks={} draws={} trace={}",
                self.ticks,
                self.draw_work_ns.len(),
                gpui::profiler::trace_enabled()
            );
        }
        // 安全阀：120s 内一帧都没完成说明内容量让首帧无法收敛（例如一次性展开上万行），
        // 记录空报告并退出，避免探针把窗口挂死。
        if self.draw_work_ns.is_empty() && self.started.elapsed() > Duration::from_secs(120) {
            return true;
        }
        self.draw_work_ns.len() >= self.target
    }

    pub(crate) fn report(&self) -> String {
        let draws = self.draw_work_ns.len();
        let elapsed = self.started.elapsed();
        let fps = if elapsed.as_secs_f64() > 0.0 {
            draws as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };
        let mut out = String::new();
        out.push_str(&format!(
            "[frame-probe] draws={} elapsed_ms={} avg_fps={:.1} rows={} viewport={:?} scale={} scroll_max={:.0}\n",
            draws,
            elapsed.as_millis(),
            fps,
            self.rows,
            self.viewport,
            self.scale,
            self.scroll_max
        ));
        for (name, values) in [
            ("draw_work", &self.draw_work_ns),
            ("request_layout", &self.layout_ns),
            ("prepaint", &self.prepaint_ns),
            ("paint", &self.paint_ns),
            ("present_interval", &self.present_interval_ns),
        ] {
            if values.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "[frame-probe] {} p50_us={:.2} p95_us={:.2} max_us={:.2} samples={}\n",
                name,
                percentile_ns(values, 0.50) as f64 / 1_000.0,
                percentile_ns(values, 0.95) as f64 / 1_000.0,
                values.iter().copied().max().unwrap_or(0) as f64 / 1_000.0,
                values.len()
            ));
        }
        let budget = self.refresh_ns.unwrap_or(16_666_667);
        let missed = self
            .draw_work_ns
            .iter()
            .filter(|&&work| work > budget)
            .count();
        out.push_str(&format!(
            "[frame-probe] refresh_ns={:?} budget_us={:.2} missed_refresh={} missed_ratio={:.4}\n",
            self.refresh_ns,
            budget as f64 / 1_000.0,
            missed,
            if draws == 0 {
                0.0
            } else {
                missed as f64 / draws as f64
            }
        ));
        out
    }
}

fn percentile_ns(values: &[u64], pct: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 - 1.0) * pct).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}
