//! M4 W3：用 GPUI `TestAppContext` 模拟真实鼠标点击，验证 ADC 交互元素（Button /
//! Checkbox / Switch / Select chip）的 onPress/onChange 真的到达宿主 handler。

use std::cell::Cell;
use std::rc::Rc;

use dshr_ui_gpui::{adc_view_with_handler, parse_tree, Action, AdcActionHandler, Theme};
use gpui::{div, prelude::*, px, IntoElement, Modifiers, Render, TestAppContext};

struct Harness {
    tree: serde_json::Value,
    on_action: Rc<dyn Fn(&Action)>,
}

impl Render for Harness {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let on_action = self.on_action.clone();
        let handler: AdcActionHandler = Rc::new(move |action: &Action, _, _| on_action(action));
        let parsed = parse_tree(&self.tree);
        div()
            .w(px(400.0))
            .h(px(300.0))
            .child(adc_view_with_handler(&Theme::light(), &parsed.root, handler))
    }
}

fn click(
    cx: &mut TestAppContext,
    tree: serde_json::Value,
    selector: &'static str,
) -> Option<Action> {
    let seen = Rc::new(Cell::new(None));
    let sink = seen.clone();
    let on_action: Rc<dyn Fn(&Action)> = Rc::new(move |action| {
        sink.set(Some(action.clone()));
    });
    let (_view, cx) = cx.add_window_view(move |_, _| Harness { tree, on_action });
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing debug bounds for {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    seen.take()
}

/// M4-ADC-009：点击 Button `adc-$`，`copy` 动作进入 handler。
#[gpui::test]
fn m4_adc_009_real_click_dispatches_copy(cx: &mut TestAppContext) {
    let action = click(
        cx,
        serde_json::json!({
            "type": "Button",
            "props": {
                "label": "Copy",
                "a11yLabel": "Copy text",
                "onPress": { "copy": "hello" }
            }
        }),
        "adc-$",
    )
    .expect("click should dispatch an action");
    assert!(
        matches!(action, Action::Copy { ref text } if text == "hello"),
        "unexpected action: {action:?}"
    );
}

/// M4-ADC-010：点击 Checkbox 行，onChange call 到达 handler。
#[gpui::test]
fn m4_adc_010_real_click_checkbox_on_change(cx: &mut TestAppContext) {
    let action = click(
        cx,
        serde_json::json!({
            "type": "Checkbox",
            "props": {
                "label": "Enable",
                "checked": false,
                "a11yLabel": "Enable",
                "onChange": { "call": "toggle", "args": {} }
            }
        }),
        "adc-$",
    )
    .expect("checkbox click should dispatch an action");
    assert!(
        matches!(action, Action::Call { ref method, .. } if method == "toggle"),
        "unexpected action: {action:?}"
    );
}

/// M4-ADC-011：点击 Switch 行，onChange call 到达 handler。
#[gpui::test]
fn m4_adc_011_real_click_switch_on_change(cx: &mut TestAppContext) {
    let action = click(
        cx,
        serde_json::json!({
            "type": "Switch",
            "props": {
                "label": "Live",
                "checked": true,
                "a11yLabel": "Live",
                "onChange": { "call": "set_live", "args": {} }
            }
        }),
        "adc-$",
    )
    .expect("switch click should dispatch an action");
    assert!(
        matches!(action, Action::Call { ref method, .. } if method == "set_live"),
        "unexpected action: {action:?}"
    );
}

/// M4-ADC-013：点击 Slider 的 + 步进，value 注入 onChange args。
#[gpui::test]
fn m4_adc_013_real_click_slider_step_injects_value(cx: &mut TestAppContext) {
    let action = click(
        cx,
        serde_json::json!({
            "type": "Slider",
            "props": {
                "value": 5.0,
                "min": 0.0,
                "max": 10.0,
                "step": 1.0,
                "a11yLabel": "Level",
                "onChange": { "call": "set_level", "args": {} }
            }
        }),
        "adc-$-inc",
    )
    .expect("slider + click should dispatch an action");
    match action {
        Action::Call { ref method, ref args } => {
            assert_eq!(method, "set_level");
            assert_eq!(args["value"], 6.0, "slider step not injected: {args}");
        }
        other => panic!("unexpected action: {other:?}"),
    }
}

/// M4-ADC-014：点击 Input 发 `__adc_edit`，带 path/value/multiline/on_change。
#[gpui::test]
fn m4_adc_014_real_click_input_emits_edit(cx: &mut TestAppContext) {
    let action = click(
        cx,
        serde_json::json!({
            "type": "Input",
            "props": {
                "value": "hello",
                "a11yLabel": "Name",
                "onChange": { "call": "save", "args": {} }
            }
        }),
        "adc-$",
    )
    .expect("input click should dispatch an edit action");
    match action {
        Action::Call { ref method, ref args } => {
            assert_eq!(method, "__adc_edit");
            assert_eq!(args["path"], "$");
            assert_eq!(args["value"], "hello");
            assert_eq!(args["multiline"], false);
            assert_eq!(args["on_change"]["call"], "save");
        }
        other => panic!("unexpected action: {other:?}"),
    }
}

/// M4-ADC-015：点击 Textarea 发 `__adc_edit` 且 multiline=true。
#[gpui::test]
fn m4_adc_015_real_click_textarea_emits_multiline_edit(cx: &mut TestAppContext) {
    let action = click(
        cx,
        serde_json::json!({
            "type": "Textarea",
            "props": {
                "value": "line1\nline2",
                "rows": 3,
                "a11yLabel": "Notes",
                "onChange": { "call": "save", "args": {} }
            }
        }),
        "adc-$",
    )
    .expect("textarea click should dispatch an edit action");
    match action {
        Action::Call { ref method, ref args } => {
            assert_eq!(method, "__adc_edit");
            assert_eq!(args["multiline"], true);
            assert_eq!(args["value"], "line1\nline2");
        }
        other => panic!("unexpected action: {other:?}"),
    }
}

/// M4-ADC-016：点击 Button 的 openExternal，URL 原样进入 handler（确认条仍由宿主负责）。
#[gpui::test]
fn m4_adc_016_real_click_open_external(cx: &mut TestAppContext) {
    let action = click(
        cx,
        serde_json::json!({
            "type": "Button",
            "props": {
                "label": "Open",
                "a11yLabel": "Open docs",
                "onPress": { "openExternal": "https://example.com/docs" }
            }
        }),
        "adc-$",
    )
    .expect("openExternal click should dispatch an action");
    assert!(
        matches!(action, Action::OpenExternal { ref url } if url == "https://example.com/docs"),
        "unexpected action: {action:?}"
    );
}

/// M4-ADC-017：Button 真实点击获得焦点后，Space 再触发一次 onPress。
#[gpui::test]
fn m4_adc_017_keyboard_activates_focused_button(cx: &mut TestAppContext) {
    let count = Rc::new(Cell::new(0usize));
    let sink = count.clone();
    let on_action: Rc<dyn Fn(&Action)> = Rc::new(move |_| sink.set(sink.get() + 1));
    let tree = serde_json::json!({
        "type": "Button",
        "props": {
            "label": "Run",
            "a11yLabel": "Run",
            "onPress": { "copy": "hello" }
        }
    });
    let (_view, cx) = cx.add_window_view(move |_, _| Harness { tree, on_action });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("adc-$").expect("button bounds");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(count.get(), 1, "click should fire once");
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert_eq!(count.get(), 2, "Space should activate focused button");
}

/// M4-ADC-018：Checkbox 点击聚焦后 Space 再触发 onChange。
#[gpui::test]
fn m4_adc_018_keyboard_activates_checkbox(cx: &mut TestAppContext) {
    let count = Rc::new(Cell::new(0usize));
    let sink = count.clone();
    let on_action: Rc<dyn Fn(&Action)> = Rc::new(move |_| sink.set(sink.get() + 1));
    let tree = serde_json::json!({
        "type": "Checkbox",
        "props": {
            "label": "Enable",
            "checked": false,
            "a11yLabel": "Enable",
            "onChange": { "call": "toggle", "args": {} }
        }
    });
    let (_view, cx) = cx.add_window_view(move |_, _| Harness { tree, on_action });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("adc-$").expect("checkbox bounds");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(count.get(), 1);
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert_eq!(count.get(), 2, "Space should activate checkbox");
}

/// M4-ADC-019：Switch 点击聚焦后 Enter 再触发 onChange。
#[gpui::test]
fn m4_adc_019_keyboard_activates_switch(cx: &mut TestAppContext) {
    let count = Rc::new(Cell::new(0usize));
    let sink = count.clone();
    let on_action: Rc<dyn Fn(&Action)> = Rc::new(move |_| sink.set(sink.get() + 1));
    let tree = serde_json::json!({
        "type": "Switch",
        "props": {
            "label": "Live",
            "checked": true,
            "a11yLabel": "Live",
            "onChange": { "call": "set_live", "args": {} }
        }
    });
    let (_view, cx) = cx.add_window_view(move |_, _| Harness { tree, on_action });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("adc-$").expect("switch bounds");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(count.get(), 1);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(count.get(), 2, "Enter should activate switch");
}

/// M4-ADC-020：Select chip 点击聚焦后 Space 再次派发注入 value。
#[gpui::test]
fn m4_adc_020_keyboard_activates_select_chip(cx: &mut TestAppContext) {
    let count = Rc::new(Cell::new(0usize));
    let sink = count.clone();
    let on_action: Rc<dyn Fn(&Action)> = Rc::new(move |_| sink.set(sink.get() + 1));
    let tree = serde_json::json!({
        "type": "Select",
        "props": {
            "value": "a",
            "a11yLabel": "Mode",
            "options": [
                { "label": "A", "value": "a" },
                { "label": "B", "value": "b" }
            ],
            "onChange": { "call": "pick", "args": {} }
        }
    });
    let (_view, cx) = cx.add_window_view(move |_, _| Harness { tree, on_action });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("adc-$-option-1").expect("select chip bounds");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(count.get(), 1);
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert_eq!(count.get(), 2, "Space should activate selected chip");
}

/// M4-ADC-021：Slider 点击聚焦后 Right 键步进，value 注入 args。
#[gpui::test]
fn m4_adc_021_keyboard_steps_slider(cx: &mut TestAppContext) {
    let last = Rc::new(Cell::new(None));
    let sink = last.clone();
    let on_action: Rc<dyn Fn(&Action)> = Rc::new(move |action| sink.set(Some(action.clone())));
    let tree = serde_json::json!({
        "type": "Slider",
        "props": {
            "value": 5.0,
            "min": 0.0,
            "max": 10.0,
            "step": 1.0,
            "a11yLabel": "Level",
            "onChange": { "call": "set_level", "args": {} }
        }
    });
    let (_view, cx) = cx.add_window_view(move |_, _| Harness { tree, on_action });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("adc-$").expect("slider bounds");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    cx.simulate_keystrokes("right");
    cx.run_until_parked();
    match last.take() {
        Some(Action::Call { method, args }) => {
            assert_eq!(method, "set_level");
            assert_eq!(args["value"], 6.0, "right step not injected: {args}");
        }
        other => panic!("expected slider step action, got {other:?}"),
    }
}

/// M4-ADC-022：拖拽 Slider track 到 75% 位置，value 按 step 注入。
#[gpui::test]
fn m4_adc_022_drag_slider_track_sets_value(cx: &mut TestAppContext) {
    let last = Rc::new(Cell::new(None));
    let sink = last.clone();
    let on_action: Rc<dyn Fn(&Action)> = Rc::new(move |action| sink.set(Some(action.clone())));
    let tree = serde_json::json!({
        "type": "Slider",
        "props": {
            "value": 0.0,
            "min": 0.0,
            "max": 10.0,
            "step": 1.0,
            "a11yLabel": "Level",
            "onChange": { "call": "set_level", "args": {} }
        }
    });
    let (_view, cx) = cx.add_window_view(move |_, _| Harness { tree, on_action });
    cx.run_until_parked();
    let track = cx.debug_bounds("adc-$-track").expect("track bounds");
    let start = gpui::point(track.origin.x + px(2.0), track.center().y);
    let target = gpui::point(
        track.origin.x + track.size.width * 0.75,
        track.center().y,
    );
    cx.simulate_mouse_move(start, None, Modifiers::default());
    cx.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        gpui::point(track.origin.x + track.size.width * 0.5, track.center().y),
        Some(gpui::MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_move(target, Some(gpui::MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(target, gpui::MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    match last.take() {
        Some(Action::Call { method, args }) => {
            assert_eq!(method, "set_level");
            let value = args["value"].as_f64().expect("numeric value");
            assert!(
                (value - 7.0).abs() <= 1.0 || (value - 8.0).abs() <= 1.0,
                "drag value should be near 75%: {value}"
            );
        }
        other => panic!("expected drag action, got {other:?}"),
    }
}

/// M4-ADC-023：点击 Tabs 非 active tab，key 注入 onChange args。
#[gpui::test]
fn m4_adc_023_real_click_tabs_changes_key(cx: &mut TestAppContext) {
    let action = click(
        cx,
        serde_json::json!({
            "type": "Tabs",
            "props": {
                "activeKey": "a",
                "a11yLabel": "Sections",
                "tabs": [
                    { "key": "a", "label": "A" },
                    { "key": "b", "label": "B" }
                ],
                "onChange": { "call": "switch_tab", "args": {} }
            }
        }),
        "adc-$-tab-1",
    )
    .expect("tab click should dispatch an action");
    match action {
        Action::Call { ref method, ref args } => {
            assert_eq!(method, "switch_tab");
            assert_eq!(args["value"], "b", "tab key not injected: {args}");
        }
        other => panic!("unexpected action: {other:?}"),
    }
}

/// M4-ADC-024：Input 点击聚焦后 Enter 再次发 `__adc_edit`。
#[gpui::test]
fn m4_adc_024_keyboard_opens_input_editor(cx: &mut TestAppContext) {
    let last = Rc::new(Cell::new(None));
    let sink = last.clone();
    let on_action: Rc<dyn Fn(&Action)> = Rc::new(move |action| sink.set(Some(action.clone())));
    let tree = serde_json::json!({
        "type": "Input",
        "props": {
            "value": "hello",
            "a11yLabel": "Name",
            "onChange": { "call": "save", "args": {} }
        }
    });
    let (_view, cx) = cx.add_window_view(move |_, _| Harness { tree, on_action });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("adc-$").expect("input bounds");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    match last.take() {
        Some(Action::Call { method, .. }) => assert_eq!(method, "__adc_edit"),
        other => panic!("expected edit action, got {other:?}"),
    }
}

/// M4-ADC-025：Textarea 点击聚焦后 Space 再次发 `__adc_edit`（multiline=true）。
#[gpui::test]
fn m4_adc_025_keyboard_opens_textarea_editor(cx: &mut TestAppContext) {
    let last = Rc::new(Cell::new(None));
    let sink = last.clone();
    let on_action: Rc<dyn Fn(&Action)> = Rc::new(move |action| sink.set(Some(action.clone())));
    let tree = serde_json::json!({
        "type": "Textarea",
        "props": {
            "value": "line1",
            "rows": 3,
            "a11yLabel": "Notes",
            "onChange": { "call": "save", "args": {} }
        }
    });
    let (_view, cx) = cx.add_window_view(move |_, _| Harness { tree, on_action });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("adc-$").expect("textarea bounds");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    match last.take() {
        Some(Action::Call { method, args }) => {
            assert_eq!(method, "__adc_edit");
            assert_eq!(args["multiline"], true);
        }
        other => panic!("expected edit action, got {other:?}"),
    }
}

/// M4-ADC-012：点击 Select 的第 2 个 chip，value 注入 onChange args。
#[gpui::test]
fn m4_adc_012_real_click_select_chip_injects_value(cx: &mut TestAppContext) {
    let action = click(
        cx,
        serde_json::json!({
            "type": "Select",
            "props": {
                "value": "a",
                "a11yLabel": "Mode",
                "options": [
                    { "label": "A", "value": "a" },
                    { "label": "B", "value": "b" }
                ],
                "onChange": { "call": "pick", "args": {} }
            }
        }),
        "adc-$-option-1",
    )
    .expect("select chip click should dispatch an action");
    match action {
        Action::Call { ref method, ref args } => {
            assert_eq!(method, "pick");
            assert_eq!(args["value"], "b", "value not injected: {args}");
        }
        other => panic!("unexpected action: {other:?}"),
    }
}
