use crate::*;

/// `--selftest-wasm`：WASM 插件的产品链路（review P1-1）。
///
/// 链路：临时 WASM 包（package.json + plugin.toml + plugin.wasm(WAT) + schemas +
/// `dshr-trust.json`）→ `install_package_trusted` → 从 profile 装载（走
/// `PluginHost::load_wasm_plugin`）→ 调用 `echo` → 渲染 `ui.render` panel →
/// `trap` 工具撞 fuel 看门狗 → 宿主存活（再 call echo）→ unload。
///
/// 包建在 data root 下（不依赖用户 `%TEMP%` 是否可写），断言不联网。
pub(crate) fn selftest_clipboard() -> i32 {
    application().run(|cx: &mut App| {
        let expected = "dshr clipboard selftest 中文";
        cx.write_to_clipboard(ClipboardItem::new_string(expected.to_owned()));
        let actual = cx.read_from_clipboard().and_then(|item| item.text());
        if actual.as_deref() != Some(expected) {
            eprintln!("[selftest-clipboard] FAIL: {actual:?}");
            cx.quit();
            std::process::exit(1);
        }
        println!("[selftest-clipboard] PASS text={expected:?}");
        cx.quit();
    });
    0
}

/// `--selftest-window`：真正打开一个 GPUI 窗口，等首帧后自动关闭。
/// 只验证窗口系统/事件循环，不依赖 sidecar 或 GPU 工具。
struct WindowProbe {
    focus: FocusHandle,
    key_hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl WindowProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            key_hits: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}

impl Render for WindowProbe {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let hits = self.key_hits.clone();
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(move |_event, _window, _cx| {
                hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
            .child("window probe")
    }
}

pub(crate) fn selftest_window() -> i32 {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.0), px(480.0)), cx);
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    let probe = cx.new(WindowProbe::new);
                    let focus = probe.read(cx).focus.clone();
                    window.focus(&focus, cx);
                    probe
                },
            )
            .expect("open window");
        let hits = handle.read(cx).expect("probe").key_hits.clone();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            // 不能在这个 update 里直接 dispatch（实体重入）；defer 到本帧更新之后。
            let scheduled = handle.update(cx, |_probe, window, cx| {
                window.defer(cx, |window, cx| {
                    window.dispatch_keystroke(
                        gpui::Keystroke::parse("enter").expect("enter keystroke"),
                        cx,
                    );
                });
            });
            if scheduled.is_err() {
                eprintln!("[selftest-window] FAIL: defer scheduling failed");
                cx.update(|cx| cx.quit());
                std::process::exit(1);
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            if hits.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                eprintln!("[selftest-window] FAIL: deferred keystroke not handled");
                cx.update(|cx| cx.quit());
                std::process::exit(1);
            }
            println!("[selftest-window] PASS window+keyboard");
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    0
}

pub(crate) fn run_navigation_flow(window: gpui::WindowHandle<ChatApp>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let delay = Duration::from_millis(250);
        cx.background_executor().timer(delay).await;
        let initial = window.update(cx, |chat, _, _| {
            chat.visible_limit < chat.rows.len() && chat.scroll.offset().y < px(-100.0)
        }).unwrap_or(false);

        for key in ["/", "down", "down", "enter"] {
            let _ = window.update(cx, |_, window, cx| {
                window.defer(cx, move |window, cx| {
                    let mut keystroke = gpui::Keystroke::parse(key).expect("navigation flow key");
                    if key == "/" {
                        keystroke.key_char = Some("/".into());
                    }
                    window.dispatch_keystroke(keystroke, cx);
                });
            });
            cx.background_executor().timer(delay).await;
        }
        let invoked = window.update(cx, |chat, window, cx| {
            let invoked = chat.turn_menu_open && chat.composer_menu.is_none()
                && chat.input.read(cx).content().is_empty() && !chat.busy;
            if invoked {
                let position = point(
                    window.viewport_size().width - px(150.0),
                    chat.scroll.bounds().top() + px(20.0),
                );
                window.defer(cx, move |window, cx| {
                    window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                        button: gpui::MouseButton::Left,
                        position,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }), cx);
                    window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                        button: gpui::MouseButton::Left,
                        position,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 1,
                    }), cx);
                });
            }
            invoked
        }).unwrap_or(false);
        cx.background_executor().timer(delay).await;
        let anchored = window.update(cx, |chat, _, _| {
            let scroll = &chat.scroll;
            scroll.bounds_for_item(0).is_some_and(|bounds| {
                let top = bounds.top() + scroll.offset().y;
                top >= scroll.bounds().top() - px(1.0)
                    && top <= scroll.bounds().top() + px(24.0)
            }) && chat.visible_limit >= chat.rows.len() && !chat.turn_menu_open
        }).unwrap_or(false);
        println!("[navigation-flow] initial_tail={initial} slash_invoked={invoked} first_turn_visible={anchored}");
        if !(initial && invoked && anchored) {
            std::process::exit(1);
        }
        cx.update(|cx| cx.quit());
    }).detach();
}
