use crate::*;
use dshr_plugin_host::SlotTarget;
use dshr_ui_contributions::RegistrationRecord;
use std::collections::HashSet;

const PAGE: &str = "workspace.page";
const RIGHT_TAB: &str = "workspace.sidebar.tab";

#[derive(Default)]
pub(crate) struct AdcNavigation {
    pub(crate) page: Option<SlotTarget>,
    right: Option<SlotTarget>,
    closed_tabs: HashSet<SlotTarget>,
    pub(crate) file: Option<super::adc_file_preview::AdcFilePreview>,
}

impl AdcNavigation {
    pub(super) fn select_panels(&mut self) {
        self.right = None;
        if let Some(file) = &mut self.file {
            file.selected = false;
        }
    }
}

impl ChatApp {
    fn workspace_slots(
        &self,
        name: &'static str,
    ) -> impl Iterator<Item = (&AdcPanelView, &RegistrationRecord, &serde_json::Value)> {
        self.adc_panels.iter().filter_map(move |view| {
            let registration = view.slot.as_ref().filter(|slot| slot.slot_id == name)?;
            Some((view, registration, view.tree.as_ref()?))
        })
    }

    pub(crate) fn adc_workspace_links(
        &self,
        compact: bool,
        t: &Theme,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let mut entries = self.workspace_slots(PAGE).peekable();
        entries.peek()?;
        let p = t.palette;
        Some(
            div()
                .id("adc-workspace-links")
                .flex()
                .flex_col()
                .min_h(px(0.0))
                .max_h(px(156.0))
                .overflow_y_scroll()
                .gap(px(4.0))
                .children(entries.map(|(view, registration, _)| {
                    let target = SlotTarget::from(registration);
                    let selected = self.adc_navigation.page == Some(target);
                    chrome_button(slot_element_id(registration), "", t)
                        .aria_label(view.title.clone())
                        .aria_selected(selected)
                        .focusable()
                        .when(selected, |button| button.bg(p.color(p.business_tertiary)))
                        .child(desktop_icon("\u{e71d}", "◇"))
                        .when(!compact, |button| {
                            button.w_full().justify_start().gap(px(6.0)).child(
                                div()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(view.title.clone()),
                            )
                        })
                        .tooltip(toolbar_tooltip(view.title.clone(), *t))
                        .on_click(cx.listener(move |chat, _, window, cx| {
                            chat.plugins_open = false;
                            chat.right_sidebar_open = false;
                            chat.workspace_picker_open = false;
                            chat.composer_menu = None;
                            chat.session_menu = None;
                            chat.header_menu = None;
                            chat.turn_menu_open = false;
                            chat.adc_navigation.page = Some(target);
                            window.focus(&chat.adc_page_focus, cx);
                            cx.notify();
                        }))
                }))
                .into_any_element(),
        )
    }

    pub(crate) fn adc_workspace_page(&self, t: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let target = self.adc_navigation.page?;
        let (view, registration, tree) = self
            .workspace_slots(PAGE)
            .find(|(_, record, _)| SlotTarget::from(*record) == target)?;
        let p = t.palette;
        Some(
            div()
                .id("adc-workspace-page")
                .track_focus(&self.adc_page_focus)
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(12.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .child(
                            chrome_button("adc-return-to-conversation", "返回会话", t)
                                .focusable()
                                .on_click(cx.listener(|chat, _, window, cx| {
                                    chat.focus_composer(window, cx)
                                })),
                        )
                        .child(
                            div()
                                .min_w(px(0.0))
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_color(p.color(p.label_primary))
                                .child(view.title.clone()),
                        ),
                )
                .child(
                    div()
                        .id(slot_element_id(registration))
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_y_scroll()
                        .px(px(12.0))
                        .child(self.adc_body(view, tree, t, cx).0),
                )
                .into_any_element(),
        )
    }

    pub(crate) fn adc_right_tabs(&self, t: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let mut tabs = self.workspace_slots(RIGHT_TAB).peekable();
        let file_selected = self
            .adc_navigation
            .file
            .as_ref()
            .is_some_and(|file| file.selected);
        let file_tab = self.adc_file_tab(t, cx);
        if tabs.peek().is_none() && file_tab.is_none() {
            return None;
        }
        let active = if file_selected {
            None
        } else {
            self.active_adc_right_tab()
                .map(|(_, slot, _)| SlotTarget::from(slot))
        };
        let panels_selected = !file_selected && active.is_none();
        let p = t.palette;
        let has_closed = self.workspace_slots(RIGHT_TAB).any(|(_, slot, _)| {
            self.adc_navigation
                .closed_tabs
                .contains(&SlotTarget::from(slot))
        });
        Some(
            div()
                .flex()
                .flex_none()
                .gap(px(4.0))
                .px(px(8.0))
                .pb(px(6.0))
                .child(
                    div()
                        .id("adc-right-tab-list")
                        .role(Role::TabList)
                        .flex()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_x_scroll()
                        .gap(px(4.0))
                        .child(
                            chrome_button("adc-right-panels-tab", "面板", t)
                                .role(Role::Tab)
                                .aria_selected(panels_selected)
                                .focusable()
                                .when(panels_selected, |tab| tab.bg(p.color(p.business_tertiary)))
                                .on_click(cx.listener(|chat, _, _, cx| {
                                    chat.adc_navigation.select_panels();
                                    cx.notify();
                                })),
                        )
                        .children(tabs.filter_map(|(view, slot, _)| {
                            let target = SlotTarget::from(slot);
                            if self.adc_navigation.closed_tabs.contains(&target) {
                                return None;
                            }
                            let selected = active == Some(target);
                            Some(
                                chrome_button(slot_element_id(slot), "", t)
                                    .role(Role::Tab)
                                    .aria_label(view.title.clone())
                                    .aria_selected(selected)
                                    .focusable()
                                    .max_w(px(140.0))
                                    .when(selected, |tab| tab.bg(p.color(p.business_tertiary)))
                                    .child(
                                        div()
                                            .min_w(px(0.0))
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(view.title.clone()),
                                    )
                                    .on_click(cx.listener(move |chat, _, _, cx| {
                                        chat.adc_navigation.right = Some(target);
                                        if let Some(file) = &mut chat.adc_navigation.file {
                                            file.selected = false;
                                        }
                                        cx.notify();
                                    })),
                            )
                        }))
                        .children(file_tab),
                )
                .when(file_selected, |row| {
                    row.child(
                        toolbar_icon_button(
                            "adc-close-file-tab",
                            "关闭文件预览",
                            "\u{e8bb}",
                            "×",
                            t,
                        )
                        .on_click(cx.listener(|chat, _, window, cx| {
                            chat.close_adc_file_preview(window, cx);
                        })),
                    )
                })
                .when_some(active, |row, target| {
                    row.child(
                        toolbar_icon_button(
                            "adc-close-tab",
                            "关闭当前插件页签",
                            "\u{e8bb}",
                            "×",
                            t,
                        )
                        .on_click(cx.listener(
                            move |chat, _, window, cx| {
                                chat.adc_navigation.closed_tabs.insert(target);
                                chat.adc_navigation.right = None;
                                chat.focus_composer(window, cx);
                                cx.notify();
                            },
                        )),
                    )
                })
                .when(has_closed, |row| {
                    row.child(
                        toolbar_icon_button(
                            "adc-reopen-tabs",
                            "重新打开已关闭的插件页签",
                            "\u{e710}",
                            "+",
                            t,
                        )
                        .on_click(cx.listener(|chat, _, _, cx| {
                            chat.adc_navigation.closed_tabs.clear();
                            cx.notify();
                        })),
                    )
                })
                .into_any_element(),
        )
    }

    fn active_adc_right_tab(
        &self,
    ) -> Option<(&AdcPanelView, &RegistrationRecord, &serde_json::Value)> {
        let target = self.adc_navigation.right?;
        self.workspace_slots(RIGHT_TAB)
            .find(|(_, slot, _)| SlotTarget::from(*slot) == target)
    }

    pub(crate) fn adc_right_tab_body(&self, t: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let (view, slot, tree) = self.active_adc_right_tab()?;
        Some(
            div()
                .id(slot_element_id(slot))
                .child(self.adc_body(view, tree, t, cx).0)
                .into_any_element(),
        )
    }

    pub(crate) fn adc_status_bar(&self, t: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let items = self.adc_slot_elements("status.bar.item", t, cx);
        let p = t.palette;
        (!items.is_empty()).then(|| {
            div()
                .id("adc-status-bar")
                .aria_label("状态栏")
                .flex()
                .flex_none()
                .min_w(px(0.0))
                .max_h(px(36.0))
                .overflow_x_scroll()
                .items_center()
                .gap(px(8.0))
                .border_t_1()
                .border_color(p.color(p.border_l2))
                .px(px(8.0))
                .py(px(2.0))
                .children(items)
                .into_any_element()
        })
    }
}

fn slot_element_id(slot: &RegistrationRecord) -> String {
    format!(
        "adc-{}-{}-{}",
        slot.slot_id,
        slot.generation,
        slot.handle.value()
    )
}
