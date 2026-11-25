use super::{
    chrome_button, desktop_icon, toolbar_icon_button, ChatApp, FocusComposerShortcut,
    NewSessionShortcut, QuitShortcut, TogglePluginsShortcut, ToggleSidebarShortcut,
};
use dshr_ui_gpui::{CopyInput, CutInput, PasteInput, SelectAllInput, Theme};
use gpui::prelude::*;
use gpui::{
    div, point, px, AnyElement, App, Context, Role, Window, WindowControlArea, WindowHandle,
};
use std::time::Duration;

pub(super) const HEIGHT: f32 = 38.0;
const MENU_TRIGGER_WIDTH: f32 = 48.0;
const MENU_ROW_HEIGHT: f32 = 28.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowMenu {
    Application,
    Edit,
}

#[derive(Clone, Copy)]
enum MenuCommand {
    NewSession,
    FocusComposer,
    ToggleSidebar,
    TogglePlugins,
    Quit,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

const APPLICATION: &[(MenuCommand, &str, &str)] = &[
    (MenuCommand::NewSession, "新建会话", "Ctrl+N"),
    (MenuCommand::FocusComposer, "聚焦输入框", "Ctrl+K"),
    (MenuCommand::ToggleSidebar, "切换侧栏", "Ctrl+B"),
    (MenuCommand::TogglePlugins, "切换插件面板", "Ctrl+J"),
    (MenuCommand::Quit, "关闭窗口", "Ctrl+W"),
];

const EDIT: &[(MenuCommand, &str, &str)] = &[
    (MenuCommand::Cut, "剪切", "Ctrl+X"),
    (MenuCommand::Copy, "复制", "Ctrl+C"),
    (MenuCommand::Paste, "粘贴", "Ctrl+V"),
    (MenuCommand::SelectAll, "全选", "Ctrl+A"),
];

impl MenuCommand {
    fn action(self) -> Box<dyn gpui::Action> {
        match self {
            Self::NewSession => Box::new(NewSessionShortcut),
            Self::FocusComposer => Box::new(FocusComposerShortcut),
            Self::ToggleSidebar => Box::new(ToggleSidebarShortcut),
            Self::TogglePlugins => Box::new(TogglePluginsShortcut),
            Self::Quit => Box::new(QuitShortcut),
            Self::Cut => Box::new(CutInput),
            Self::Copy => Box::new(CopyInput),
            Self::Paste => Box::new(PasteInput),
            Self::SelectAll => Box::new(SelectAllInput),
        }
    }
}

impl ChatApp {
    pub(super) fn close_window_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let closed = self.window_menu.take().is_some();
        if closed {
            cx.notify();
        }
        closed
    }

    pub(super) fn window_bar(
        &self,
        t: &Theme,
        collapsed: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let (sidebar_label, sidebar_icon) = if collapsed {
            ("展开侧栏", "\u{e8a0}")
        } else {
            ("收起侧栏", "\u{e89f}")
        };
        let (zoom_label, zoom_icon) = if window.is_maximized() {
            ("还原窗口", "\u{e923}")
        } else {
            ("最大化窗口", "\u{e922}")
        };
        let menu_disabled = self.attachment_preview.is_some();
        div()
            .id("window-bar")
            .h(px(HEIGHT))
            .flex()
            .flex_none()
            .items_center()
            .pl(px(8.0))
            .bg(p.color(p.sidebar_fill))
            .text_color(p.color(p.label_primary))
            .child(
                toolbar_icon_button("window-sidebar", sidebar_label, sidebar_icon, "☰", t)
                    .aria_expanded(!collapsed)
                    .on_click(cx.listener(|chat, _, window, cx| {
                        chat.close_window_menu(cx);
                        chat.toggle_sidebar(window, cx);
                    })),
            )
            .child(self.window_menu_button(WindowMenu::Application, menu_disabled, t, cx))
            .child(self.window_menu_button(WindowMenu::Edit, menu_disabled, t, cx))
            .child(
                div()
                    .id("window-drag-region")
                    .h_full()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .window_control_area(WindowControlArea::Drag)
                    .when(collapsed, |area| {
                        area.text_size(t.type_scale.small)
                            .text_color(p.color(p.label_secondary))
                            .child("DSHR")
                    }),
            )
            .child(caption_button(
                "window-min",
                "最小化窗口",
                "\u{e921}",
                WindowControlArea::Min,
                window.is_minimizable(),
                t,
            ))
            .child(caption_button(
                "window-zoom",
                zoom_label,
                zoom_icon,
                WindowControlArea::Max,
                window.is_resizable(),
                t,
            ))
            .child(caption_button(
                "window-close",
                "关闭窗口",
                "\u{e8bb}",
                WindowControlArea::Close,
                true,
                t,
            ))
            .into_any_element()
    }

    fn window_menu_button(
        &self,
        menu: WindowMenu,
        disabled: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let open = self.window_menu == Some(menu) && !disabled;
        let (id, label) = match menu {
            WindowMenu::Application => ("window-menu-app", "应用"),
            WindowMenu::Edit => ("window-menu-edit", "编辑"),
        };
        div()
            .relative()
            .flex_none()
            .child(
                chrome_button(id, label, t)
                    .w(px(MENU_TRIGGER_WIDTH))
                    .aria_expanded(open)
                    .when(open, |button| button.bg(p.color(p.interactive_hover_solid)))
                    .when(!disabled, |button| {
                        button.on_click(cx.listener(move |chat, _, _, cx| {
                            chat.composer_menu = None;
                            chat.turn_menu_open = false;
                            chat.window_menu = (chat.window_menu != Some(menu)).then_some(menu);
                            cx.notify();
                        }))
                    }),
            )
            .when(open, |slot| slot.child(self.window_menu_panel(menu, t, cx)))
            .into_any_element()
    }

    fn window_menu_panel(&self, menu: WindowMenu, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let (entries, menu_key) = match menu {
            WindowMenu::Application => (APPLICATION, dshr_plugin_host::APPLICATION_MENU),
            WindowMenu::Edit => (EDIT, dshr_plugin_host::EDIT_MENU),
        };
        let menu = div()
            .id("window-menu-items")
            .role(Role::Menu)
            .absolute()
            .top(gpui::relative(1.0))
            .left_0()
            .occlude()
            .w(px(228.0))
            .p(px(5.0))
            .rounded(t.radii.md)
            .border_1()
            .border_color(p.color(p.border_l2))
            .bg(p.color(p.bg_layer_1))
            .shadow_sm()
            .children(
                entries
                    .iter()
                    .enumerate()
                    .map(|(index, (action, label, key))| {
                        let action = *action;
                        chrome_button(("window-menu-item", index), label, t)
                            .role(Role::MenuItem)
                            .aria_keyshortcuts(*key)
                            .h(px(MENU_ROW_HEIGHT))
                            .w_full()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(t.type_scale.small)
                                    .text_color(p.color(p.label_tertiary))
                                    .child(*key),
                            )
                            .on_click(cx.listener(move |chat, _, window, cx| {
                                chat.close_window_menu(cx);
                                window.dispatch_action(action.action(), cx);
                            }))
                    }),
            )
            .when_some(self.adc_menu_item(menu_key), |menu, (label, target)| {
                menu.child(
                    chrome_button("window-plugin-menu-item", "", t)
                        .role(Role::MenuItem)
                        .aria_label(label.to_owned())
                        .child(label.to_owned())
                        .focusable()
                        .h(px(MENU_ROW_HEIGHT))
                        .w_full()
                        .on_click(cx.listener(move |chat, _, _, cx| {
                            chat.close_window_menu(cx);
                            chat.run_adc_command(target, cx);
                        })),
                )
            });
        gpui::deferred(menu).with_priority(13).into_any_element()
    }

    pub(super) fn window_menu_dismiss_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.window_menu?;
        let layer = div()
            .id("dismiss-window-menu")
            .absolute()
            .top(px(HEIGHT))
            .bottom_0()
            .left_0()
            .right_0()
            .occlude()
            .on_click(cx.listener(|chat, _, _, cx| {
                chat.close_window_menu(cx);
            }));
        Some(gpui::deferred(layer).with_priority(12).into_any_element())
    }
}

fn caption_button(
    id: &'static str,
    label: &'static str,
    icon: &'static str,
    area: WindowControlArea,
    enabled: bool,
    t: &Theme,
) -> impl IntoElement {
    let p = t.palette;
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(px(40.0))
        .h_full()
        .occlude()
        .child(desktop_icon(icon, "").text_size(px(10.0)))
        .text_color(p.color(if enabled {
            p.label_primary
        } else {
            p.label_caption
        }))
        .when(enabled, |button| {
            button.window_control_area(area).hover(|style| {
                if matches!(area, WindowControlArea::Close) {
                    style.bg(p.color(p.state_error)).text_color(gpui::white())
                } else {
                    style.bg(p.color(p.interactive_hover))
                }
            })
        })
}

pub(super) fn run_titlebar_flow(handle: WindowHandle<ChatApp>, cx: &mut App) {
    fn click(window: &mut Window, cx: &mut Context<ChatApp>, x: f32, y: f32) {
        let position = point(px(x), px(y));
        window.defer(cx, move |window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                    button: gpui::MouseButton::Left,
                    position,
                    modifiers: gpui::Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                    button: gpui::MouseButton::Left,
                    position,
                    modifiers: gpui::Modifiers::default(),
                    click_count: 1,
                }),
                cx,
            );
        });
    }

    cx.spawn(async move |cx| {
        let delay = Duration::from_millis(250);
        let app_x = 8.0 + 28.0 + MENU_TRIGGER_WIDTH / 2.0;
        let edit_x = app_x + MENU_TRIGGER_WIDTH;
        // The panel starts under the centered 28px trigger, then has 6px of border/padding.
        let first_item_y = (HEIGHT + 28.0) / 2.0 + 6.0 + MENU_ROW_HEIGHT / 2.0;
        cx.background_executor().timer(delay).await;
        let (original_session, layout_ok) = handle
            .update(cx, |chat, window, cx| {
                chat.input.update(cx, |input, cx| {
                    input.set_content("titlebar test", window, cx);
                });
                chat.focus_composer(window, cx);
                click(window, cx, edit_x, HEIGHT / 2.0);
                let layout_ok = chat
                    .input
                    .read(cx)
                    .bounds()
                    .is_some_and(|bounds| bounds.top() >= px(HEIGHT));
                (chat.session_id.clone(), layout_ok)
            })
            .expect("title bar test window");
        cx.background_executor().timer(delay).await;
        let edit_opened = handle
            .update(cx, |chat, window, cx| {
                click(window, cx, edit_x, first_item_y + 3.0 * MENU_ROW_HEIGHT);
                chat.window_menu == Some(WindowMenu::Edit)
            })
            .unwrap_or(false);
        cx.background_executor().timer(delay).await;
        let _ = handle.update(cx, |_, window, cx| {
            window.defer(cx, |window, cx| {
                let mut key = gpui::Keystroke::parse("q").expect("title bar test key");
                key.key_char = Some("q".into());
                window.dispatch_keystroke(key, cx);
            });
        });
        cx.background_executor().timer(delay).await;
        let edit_worked = handle
            .update(cx, |chat, window, cx| {
                click(window, cx, app_x, HEIGHT / 2.0);
                chat.window_menu.is_none() && chat.input.read(cx).content() == "q"
            })
            .unwrap_or(false);
        cx.background_executor().timer(delay).await;
        let app_opened = handle
            .update(cx, |chat, window, cx| {
                click(window, cx, app_x, first_item_y);
                chat.window_menu == Some(WindowMenu::Application)
            })
            .unwrap_or(false);
        cx.background_executor().timer(delay).await;
        let session_created = handle
            .update(cx, |chat, _, _| {
                chat.window_menu.is_none() && chat.session_id != original_session
            })
            .unwrap_or(false);
        println!(
            "[titlebar-flow] layout={layout_ok} edit_opened={edit_opened} \
             select_all={edit_worked} app_opened={app_opened} new_session={session_created}"
        );
        if !(layout_ok && edit_opened && edit_worked && app_opened && session_created) {
            std::process::exit(1);
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}
