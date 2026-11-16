//! M4 W3：用 GPUI `TestAppContext` 模拟真实鼠标点击，验证 ADC Button 的 onPress
//! 回调真的到达宿主 handler，而不只是构造动作树。

use std::cell::Cell;
use std::rc::Rc;

use dshr_ui_gpui::{
    adc_view_with_handler, parse_tree, Action, AdcActionHandler, Theme,
};
use gpui::{div, prelude::*, px, IntoElement, Modifiers, Render, TestAppContext};

struct Harness {
    tree: serde_json::Value,
    clicked: Rc<Cell<bool>>,
}

impl Render for Harness {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let clicked = self.clicked.clone();
        let handler: AdcActionHandler = Rc::new(move |action: &Action, _, _| {
            if matches!(action, Action::Copy { .. }) {
                clicked.set(true);
            }
        });
        let parsed = parse_tree(&self.tree);
        div()
            .w(px(400.0))
            .h(px(300.0))
            .child(adc_view_with_handler(&Theme::light(), &parsed.root, handler))
    }
}

/// M4-ADC-009：TestAppContext 点击 `adc-$` 后，`copy` 动作进入 handler。
#[gpui::test]
fn m4_adc_009_real_click_dispatches_copy(cx: &mut TestAppContext) {
    let clicked = Rc::new(Cell::new(false));
    let tree = serde_json::json!({
        "type": "Button",
        "props": {
            "label": "Copy",
            "a11yLabel": "Copy text",
            "onPress": { "copy": "hello" }
        }
    });
    let flag = clicked.clone();
    let (_view, cx) = cx.add_window_view(move |_, _| Harness {
        tree,
        clicked: flag,
    });
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds("adc-$")
        .expect("Button root should record debug bounds");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(clicked.get(), "click did not reach ADC copy handler");
}
