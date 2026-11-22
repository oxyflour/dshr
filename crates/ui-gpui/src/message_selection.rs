use std::{cell::RefCell, collections::BTreeMap, ops::Range, rc::Rc};

use gpui::{
    canvas, div, fill, point, prelude::*, px, size, AnyElement, Bounds, ClipboardItem, Div,
    FocusHandle, Hsla, MouseButton, Pixels, Point, SharedString, Stateful, StyledText, TextLayout,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::text_input::Copy;

type FragmentId = (usize, usize);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Position {
    fragment: FragmentId,
    byte: usize,
}

struct Fragment {
    text: SharedString,
    layout: TextLayout,
    bounds: Bounds<Pixels>,
}

#[derive(Default)]
struct Selection {
    session: String,
    visible: BTreeMap<FragmentId, Fragment>,
    selected: BTreeMap<FragmentId, SharedString>,
    endpoints: Option<(Position, Position)>,
    dragging: bool,
}

impl Selection {
    fn range(&self) -> Option<(Position, Position)> {
        let (anchor, head) = self.endpoints?;
        (anchor != head).then_some((anchor.min(head), anchor.max(head)))
    }

    fn range_in(&self, fragment: FragmentId, text: &str) -> Option<Range<usize>> {
        let (start, end) = self.range()?;
        if !(start.fragment..=end.fragment).contains(&fragment) {
            return None;
        }
        let from = if fragment == start.fragment {
            start.byte
        } else {
            0
        };
        let to = if fragment == end.fragment {
            end.byte
        } else {
            text.len()
        };
        let from = text.floor_char_boundary(from.min(text.len()));
        let to = text.floor_char_boundary(to.min(text.len()));
        (from < to).then_some(from..to)
    }

    fn clear(&mut self) -> bool {
        let changed = self.endpoints.take().is_some();
        self.selected.clear();
        self.dragging = false;
        changed
    }

    fn position_at(&self, point: Point<Pixels>, within_text: bool) -> Option<Position> {
        let (id, fragment) = self
            .visible
            .iter()
            .filter(|(_, fragment)| !within_text || fragment.bounds.contains(&point))
            .min_by_key(|(_, fragment)| {
                let x = (fragment.bounds.left() - point.x)
                    .max(point.x - fragment.bounds.right())
                    .max(px(0.0));
                let y = (fragment.bounds.top() - point.y)
                    .max(point.y - fragment.bounds.bottom())
                    .max(px(0.0));
                y * 2.0 + x
            })?;
        Some(Position {
            fragment: *id,
            byte: index_at(&fragment.layout, point).min(fragment.text.len()),
        })
    }

    fn remember_selected_text(&mut self) {
        if let Some((start, end)) = self.range() {
            for (id, fragment) in self.visible.range(start.fragment..=end.fragment) {
                self.selected.insert(*id, fragment.text.clone());
            }
        }
    }
}

/// Selection for native chat text, including separate Markdown blocks and nearby message rows.
#[derive(Clone)]
pub struct MessageSelection {
    state: Rc<RefCell<Selection>>,
    focus: FocusHandle,
}

impl MessageSelection {
    pub fn new(focus: FocusHandle) -> Self {
        Self {
            state: Rc::default(),
            focus,
        }
    }

    pub fn begin_frame(&self, session: &str) {
        let mut state = self.state.borrow_mut();
        if state.session != session {
            state.clear();
            state.session = session.to_owned();
        }
        state.visible.clear();
    }

    pub fn selected_text(&self) -> Option<String> {
        let state = self.state.borrow();
        state.range()?;
        let text: Vec<_> = state
            .selected
            .iter()
            .filter_map(|(id, text)| state.range_in(*id, text).map(|range| &text[range]))
            .collect();
        (!text.is_empty()).then(|| text.join("\n"))
    }

    pub fn clear(&self) -> bool {
        self.state.borrow_mut().clear()
    }

    pub fn bind(&self, pane: Stateful<Div>) -> Stateful<Div> {
        let start = self.clone();
        let drag = self.clone();
        let release = self.clone();
        let release_outside = self.clone();
        let copy = self.clone();
        pane.track_focus(&self.focus)
            .key_context("TranscriptSelection")
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                let mut state = start.state.borrow_mut();
                let Some(point) = state.position_at(event.position, true) else {
                    if state.clear() {
                        window.refresh();
                    }
                    return;
                };
                let anchor = state
                    .endpoints
                    .filter(|_| event.modifiers.shift)
                    .map_or(point, |(anchor, _)| anchor);
                if !event.modifiers.shift {
                    state.selected.clear();
                }
                state.endpoints = Some((anchor, point));
                state.dragging = true;
                if event.click_count == 2 {
                    let text = &state.visible[&point.fragment].text;
                    if let Some((offset, word)) =
                        text.split_word_bound_indices().find(|(offset, word)| {
                            (*offset..*offset + word.len()).contains(&point.byte)
                        })
                    {
                        state.endpoints = Some((
                            Position {
                                byte: offset,
                                ..point
                            },
                            Position {
                                byte: offset + word.len(),
                                ..point
                            },
                        ));
                        state.dragging = false;
                    }
                }
                state.remember_selected_text();
                window.focus(&start.focus, cx);
                window.refresh();
            })
            .on_mouse_move(move |event, window, _| {
                let mut state = drag.state.borrow_mut();
                if !state.dragging || event.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                if let (Some((anchor, previous)), Some(head)) =
                    (state.endpoints, state.position_at(event.position, false))
                {
                    if head != previous {
                        state.endpoints = Some((anchor, head));
                        state.remember_selected_text();
                        window.refresh();
                    }
                }
            })
            .on_mouse_up(MouseButton::Left, move |_, _, _| {
                release.state.borrow_mut().dragging = false;
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, _| {
                release_outside.state.borrow_mut().dragging = false;
            })
            .on_action(move |_: &Copy, _, cx| {
                if let Some(text) = copy.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            })
    }

    pub fn fragment(
        &self,
        row: usize,
        index: usize,
        text: SharedString,
        styled: StyledText,
        color: Hsla,
    ) -> AnyElement {
        let id = (row, index);
        let selection = self.clone();
        let layout = styled.layout().clone();
        div()
            .relative()
            .cursor(gpui::CursorStyle::IBeam)
            .child(styled)
            .child(
                canvas(
                    |bounds, window, _| bounds.intersect(&window.content_mask().bounds),
                    move |_, bounds, window, _| {
                        if bounds.is_empty() {
                            return;
                        }
                        let mut state = selection.state.borrow_mut();
                        if let Some(range) = state.range_in(id, &text) {
                            let height = window.pixel_snap(window.line_height());
                            for rect in range_bounds(&layout, bounds, height, range) {
                                window.paint_quad(fill(rect, color.opacity(0.24)));
                            }
                        }
                        state.visible.insert(
                            id,
                            Fragment {
                                text,
                                layout,
                                bounds,
                            },
                        );
                    },
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element()
    }
}

pub(crate) fn index_at(layout: &TextLayout, point: Point<Pixels>) -> usize {
    layout
        .index_for_position(point)
        .unwrap_or_else(|index| index)
}

pub(crate) fn range_bounds(
    layout: &TextLayout,
    viewport: Bounds<Pixels>,
    line_height: Pixels,
    range: Range<usize>,
) -> Vec<Bounds<Pixels>> {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return Vec::new();
    };

    let mut rectangles = Vec::new();
    let hidden_rows = ((viewport.top() - start.y).max(px(0.0)) / line_height).floor();
    let mut y = start.y + line_height * hidden_rows;
    while y <= end.y && y < viewport.bottom() {
        if y + line_height > viewport.top() {
            let left = if y == start.y {
                start.x
            } else {
                viewport.left()
            };
            let right = if y + line_height > end.y {
                end.x
            } else {
                let line_end = index_at(layout, point(viewport.right(), y + line_height / 2.0));
                layout
                    .position_for_index(line_end)
                    .map_or(viewport.right(), |end| end.x)
            };
            let bounds = Bounds::new(
                point(left.min(right), y),
                size((right - left).abs().max(px(2.0)), line_height),
            );
            if viewport.intersects(&bounds) {
                rectangles.push(bounds.intersect(&viewport));
            }
        }
        y += line_height;
    }
    rectangles
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        Context, Font, Keystroke, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput,
        TestAppContext, Window,
    };

    struct Transcript {
        selection: MessageSelection,
    }

    impl Render for Transcript {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let theme = crate::Theme::light();
            let text = SharedString::from("用户消息");
            self.selection.begin_frame("test-session");
            self.selection
                .bind(div().id("transcript").size_full().flex().flex_col())
                .child(self.selection.fragment(
                    0,
                    0,
                    text.clone(),
                    StyledText::new(text),
                    gpui::blue(),
                ))
                .child(crate::selectable_markdown_view(
                    "**Reply** text\n\nSecond paragraph",
                    &theme,
                    &Font::default(),
                    theme.palette.color(theme.palette.label_primary),
                    &self.selection,
                    1,
                ))
        }
    }

    #[gpui::test]
    fn drag_and_copy_across_message_and_markdown_blocks(cx: &mut TestAppContext) {
        let selection = cx.update(|cx| {
            crate::init(cx);
            MessageSelection::new(cx.focus_handle())
        });
        let window = cx.add_window({
            let selection = selection.clone();
            move |_, _| Transcript { selection }
        });
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();

        let (start, end) = {
            let state = selection.state.borrow();
            let first = &state.visible[&(0, 0)];
            let last = &state.visible[&(1, 1)];
            (
                point(first.bounds.left() + px(0.5), first.bounds.center().y),
                point(last.bounds.right() - px(0.5), last.bounds.center().y),
            )
        };

        cx.update_window(window.into(), |_, window, cx| {
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: start,
                    click_count: 1,
                    ..Default::default()
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: end,
                    pressed_button: Some(MouseButton::Left),
                    ..Default::default()
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: end,
                    click_count: 1,
                    ..Default::default()
                }),
                cx,
            );
            window.draw(cx).clear(cx);
            let copy = if cfg!(target_os = "macos") {
                "cmd-c"
            } else {
                "ctrl-c"
            };
            window.dispatch_keystroke(Keystroke::parse(copy).unwrap(), cx);
        })
        .unwrap();
        assert_eq!(
            selection.selected_text().as_deref(),
            Some("用户消息\nReply text\nSecond paragraph")
        );
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("用户消息\nReply text\nSecond paragraph")
            );
        });
    }
}
