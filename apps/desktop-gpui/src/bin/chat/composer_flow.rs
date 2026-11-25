use std::time::Duration;

use gpui::{
    point, px, App, EntityInputHandler, Focusable, Keystroke, MouseButton, MouseDownEvent,
    MouseUpEvent, Pixels, PlatformInput, Point, Window, WindowHandle,
};

use super::{ChatApp, ComposerMenu, Row};

fn click(window: &mut Window, cx: &mut App, position: Point<Pixels>) {
    window.defer(cx, move |window, cx| {
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
    });
}

pub(super) fn run(handle: WindowHandle<ChatApp>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let delay = Duration::from_millis(250);
        cx.background_executor().timer(delay).await;
        let (centered, initial_height) = handle
            .update(cx, |chat, window, cx| {
                let bounds = chat.input.read(cx).bounds().expect("draft is laid out");
                let centered = chat.rows.is_empty()
                    && bounds.top() > window.viewport_size().height * 0.25
                    && bounds.top() < window.viewport_size().height * 0.75;
                click(window, cx, point(bounds.left() + px(9.0), bounds.top() + px(79.0)));
                (centered, bounds.size.height)
            })
            .expect("composer flow window");
        cx.background_executor().timer(delay).await;
        let add_open = handle
            .update(cx, |chat, window, cx| {
                let open = chat.composer_menu == Some(ComposerMenu::Add);
                let bounds = chat.input.read(cx).bounds().expect("draft is laid out");
                click(window, cx, point(bounds.left() + px(16.0), bounds.top() + px(4.0)));
                open
            })
            .unwrap_or(false);
        cx.background_executor().timer(delay).await;
        let add_dismissed = handle
            .update(cx, |chat, window, cx| {
                let dismissed = chat.composer_menu.is_none()
                    && chat.input.read(cx).focus_handle(cx).is_focused(window);
                chat.input.update(cx, |input, cx| {
                    input.set_content("这是需要自动换行的草稿。".repeat(30), window, cx);
                });
                dismissed
            })
            .unwrap_or(false);
        cx.background_executor().timer(delay).await;
        let wraps = handle
            .update(cx, |chat, window, cx| {
                let wraps = chat.input.read(cx).bounds().is_some_and(|bounds| {
                    bounds.size.height > initial_height && bounds.size.height <= px(336.0)
                });
                chat.input
                    .update(cx, |input, cx| input.set_content("第一行", window, cx));
                window.defer(cx, |window, cx| {
                    window.dispatch_keystroke(Keystroke::parse("shift-enter").unwrap(), cx);
                    let mut input = Keystroke::parse("x").unwrap();
                    input.key_char = Some("x".into());
                    window.dispatch_keystroke(input, cx);
                });
                wraps
            })
            .unwrap_or(false);
        cx.background_executor().timer(delay).await;
        let (newline, selection) = handle
            .update(cx, |chat, window, cx| {
                let newline = chat.input.read(cx).content() == "第一行\nx" && chat.rows.is_empty();
                let selection = chat.input.update(cx, |input, cx| {
                    input.bounds().is_some_and(|bounds| {
                        let rows = input.selection_bounds_for_range(0..5, bounds, window, cx);
                        rows.len() == 2 && rows[0].top() < rows[1].top()
                    })
                });
                if newline {
                    window.defer(cx, |window, cx| {
                        window.dispatch_keystroke(Keystroke::parse("enter").unwrap(), cx);
                    });
                }
                (newline, selection)
            })
            .unwrap_or_default();
        cx.background_executor()
            .timer(Duration::from_millis(700))
            .await;
        let docked = handle
            .update(cx, |chat, window, cx| {
                let sent = chat
                    .rows
                    .iter()
                    .any(|row| matches!(row, Row::User { text, .. } if text == "第一行\nx"));
                let input = chat.input.read(cx);
                sent && input.content().is_empty()
                    && input.bounds().is_some_and(|bounds| {
                        bounds.top() >= chat.scroll.bounds().bottom()
                            && window.viewport_size().height - bounds.top() <= px(124.0)
                    })
            })
            .unwrap_or(false);
        println!(
            "[composer-flow] centered={centered} add_menu={add_open} dismissed={add_dismissed} wraps={wraps} \
             newline={newline} selection={selection} docked={docked}"
        );
        if !(centered && add_open && add_dismissed && wraps && newline && selection && docked) {
            std::process::exit(1);
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

pub(super) fn run_adc_command_flow(handle: WindowHandle<ChatApp>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let delay = Duration::from_millis(250);
        cx.background_executor().timer(delay).await;
        if !probe_adc_commands(handle, cx, delay).await {
            std::process::exit(1);
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

async fn probe_adc_commands(
    handle: WindowHandle<ChatApp>,
    cx: &mut gpui::AsyncApp,
    delay: Duration,
) -> bool {
    let fixture_count = |chat: &ChatApp| -> Option<u64> {
        let panel = chat
            .adc_panels
            .iter()
            .find(|panel| panel.plugin_id == "selftest.adc" && panel.view == "fixture.quick")?;
        panel.tree.as_ref()?["props"]["label"]
            .as_str()?
            .strip_prefix("actions=")?
            .parse()
            .ok()
    };
    let start = handle
        .update(cx, |chat, window, cx| {
            let start = fixture_count(chat)?;
            window.defer(cx, |window, cx| {
                window.dispatch_keystroke(Keystroke::parse("ctrl-shift-g").unwrap(), cx);
            });
            Some(start)
        })
        .unwrap_or_default();
    let Some(start) = start else {
        eprintln!("[adc-command-flow] fixture is missing");
        return false;
    };
    cx.background_executor().timer(delay).await;
    let shortcut = handle
        .update(cx, |chat, window, cx| {
            let shortcut = fixture_count(chat) == Some(start + 1);
            chat.input.update(cx, |input, cx| {
                input.set_content("/fixture.refresh".to_owned(), window, cx)
            });
            shortcut
        })
        .unwrap_or(false);
    cx.background_executor().timer(delay).await;
    let shown = handle
        .update(cx, |chat, window, cx| {
            let shown = chat.composer_menu == Some(ComposerMenu::Commands);
            window.defer(cx, |window, cx| {
                window.dispatch_keystroke(Keystroke::parse("enter").unwrap(), cx);
            });
            shown
        })
        .unwrap_or(false);
    cx.background_executor().timer(delay).await;
    let slash = handle
        .update(cx, |chat, _, cx| {
            fixture_count(chat) == Some(start + 2) && chat.input.read(cx).content().is_empty()
        })
        .unwrap_or(false);
    println!("[adc-command-flow] shortcut={shortcut} slash_menu={shown} submit={slash}");
    shortcut && shown && slash
}
