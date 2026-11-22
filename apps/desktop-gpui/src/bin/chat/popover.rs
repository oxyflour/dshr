use std::cell::RefCell;

use gpui::prelude::*;
use gpui::{
    anchored, deferred, div, point, px, Anchor, AnyElement, App, AvailableSpace, Bounds, Div,
    Edges, MouseDownEvent, Pixels, Point, Stateful, Window,
};

pub(super) fn attach_dismissible(
    trigger: impl IntoElement,
    popup: Option<Stateful<Div>>,
    target: Anchor,
    origin: Anchor,
    dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    attach_to(
        trigger,
        popup.map(|popup| {
            move |bounds: Bounds<Pixels>| {
                dismiss_on_outside(popup, move |position| bounds.contains(&position), dismiss)
            }
        }),
        target,
        origin,
    )
}

pub(super) fn dismiss_on_outside(
    popup: Stateful<Div>,
    is_trigger: impl Fn(Point<Pixels>) -> bool + 'static,
    dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    popup
        .on_mouse_down_out(move |event, window, cx| {
            if !is_trigger(event.position) {
                dismiss(event, window, cx);
            }
        })
        .into_any_element()
}

pub(super) fn at_point(position: Point<Pixels>, popup: AnyElement) -> AnyElement {
    deferred(
        anchored()
            .position(position)
            .snap_to_window_with_margin(Edges::all(px(8.0)))
            .child(popup),
    )
    .with_priority(2)
    .into_any_element()
}

pub(super) fn attach_to(
    trigger: impl IntoElement,
    popup: Option<impl FnOnce(Bounds<Pixels>) -> AnyElement + 'static>,
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
                .child(popup(*bounds))
                .into_any_element();
            let room = window.viewport_size().map(AvailableSpace::Definite);
            menu.layout_as_root(room, window, cx);
            window.defer_draw(menu, Point::default(), 2, None);
        })
        .into_any_element()
}
