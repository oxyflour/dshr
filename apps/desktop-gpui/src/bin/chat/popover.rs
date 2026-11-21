use std::cell::RefCell;

use gpui::prelude::*;
use gpui::{anchored, div, point, px, Anchor, AnyElement, AvailableSpace, Edges, Point};

pub(super) fn attach(
    trigger: impl IntoElement,
    popup: Option<AnyElement>,
    target: Anchor,
    origin: Anchor,
) -> AnyElement {
    let Some(popup) = popup else {
        return trigger.into_any_element();
    };
    let popup = RefCell::new(Some(popup));
    div()
        .flex()
        .min_w(px(0.0))
        .child(trigger)
        .on_children_prepainted(move |children, window, cx| {
            let Some((bounds, popup)) = children.first().zip(popup.borrow_mut().take()) else {
                return;
            };
            let gap = match origin {
                Anchor::BottomLeft | Anchor::BottomRight => -8.0,
                _ => 4.0,
            };
            let mut menu = anchored()
                .anchor(origin)
                .attach(*bounds, target)
                .offset(point(px(0.0), px(gap)))
                .snap_to_window_with_margin(Edges::all(px(8.0)))
                .child(popup)
                .into_any_element();
            let room = window.viewport_size().map(AvailableSpace::Definite);
            menu.layout_as_root(room, window, cx);
            window.defer_draw(menu, Point::default(), 2, None);
        })
        .into_any_element()
}
