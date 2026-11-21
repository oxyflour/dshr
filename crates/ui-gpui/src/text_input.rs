//! GPUI 文本输入组件（中文 IME 可用的最小实现）。
//!
//! 来源：zed `crates/gpui/examples/input.rs` @ d2a4fb6（Apache-2.0）。改动：
//! 1. 增加 `Submit` action 与 `on_submit` 回调（聊天输入框回车发送）；
//! 2. 去掉示例窗口/字体/退出键，组件只负责"一个输入框"，尺寸与配色由外层决定；
//! 3. 占位符使用当前主题的文字色，降低不透明度。
//!
//! **为什么必须自己实现**：GPUI 这个版本没有内置 TextInput；中文 IME 的组合输入要求
//! 元素实现 `EntityInputHandler` 并在 `paint` 里注册 `ElementInputHandler`。
//! 只监听 `key_down` 的 `key_char` 拿不到组合态，候选窗也不会跟随光标。

use std::ops::Range;

use gpui::{
    actions, div, fill, point, prelude::*, px, rgba, size, App, Bounds, ClipboardItem, Context,
    CursorStyle, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, FocusHandle,
    Focusable, GlobalElementId, Hsla, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Role, ScrollHandle, SharedString, StyledText,
    TextLayout, TextRun, UTF16Selection, UnderlineStyle, Window,
};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Submit,
        NewLine,
        Up,
        Down,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
    ]
);

/// 回车提交回调：拿到当前文本，返回是否受理（false 时组件保留内容）。
pub type SubmitHandler = Box<dyn Fn(&str, &mut Window, &mut App) -> bool + 'static>;
pub type PasteHandler = Box<dyn Fn(&mut ClipboardItem, &mut Window, &mut App) -> bool + 'static>;

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    draft: Option<DraftInput>,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<InputLayout>,
    is_selecting: bool,
    /// 回车提交回调；返回 true 表示已受理，组件随后清空自己。
    on_submit: Option<SubmitHandler>,
    on_paste: Option<PasteHandler>,
}

struct DraftInput {
    scroll: ScrollHandle,
    max_height: Pixels,
    placeholder_color: Hsla,
    caret_color: Hsla,
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: SharedString::default(),
            placeholder: placeholder.into(),
            draft: None,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            is_selecting: false,
            on_submit: None,
            on_paste: None,
        }
    }

    /// Enable wrapping and a bounded scrolling viewport for the chat draft.
    pub fn multiline(mut self) -> Self {
        self.draft = Some(DraftInput {
            scroll: ScrollHandle::new(),
            max_height: px(336.0),
            placeholder_color: rgba(0x8185_8cff).into(),
            caret_color: gpui::blue(),
        });
        self
    }

    pub fn style_draft(
        &mut self,
        placeholder: impl Into<SharedString>,
        max_height: Pixels,
        placeholder_color: Hsla,
        caret_color: Hsla,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        if let Some(draft) = &mut self.draft {
            if self.placeholder != placeholder
                || draft.max_height != max_height
                || draft.placeholder_color != placeholder_color
                || draft.caret_color != caret_color
            {
                self.placeholder = placeholder;
                draft.max_height = max_height;
                draft.placeholder_color = placeholder_color;
                draft.caret_color = caret_color;
                cx.notify();
            }
        }
    }

    /// The last painted editing viewport, in window coordinates.
    pub fn bounds(&self) -> Option<Bounds<Pixels>> {
        self.last_layout.as_ref().map(|layout| layout.viewport)
    }

    pub fn set_on_submit(&mut self, on_submit: SubmitHandler) {
        self.on_submit = Some(on_submit);
    }

    /// The host can consume non-text clipboard content before ordinary text paste.
    pub fn set_on_paste(&mut self, on_paste: PasteHandler) {
        self.on_paste = Some(on_paste);
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn is_composing(&self) -> bool {
        self.marked_range.is_some()
    }

    /// 整体替换内容并把光标放到末尾（外部注入或清空用）。
    pub fn set_content(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.content = text.into();
        let len = self.content.len();
        self.selected_range = len..len;
        self.selection_reversed = false;
        self.marked_range = None;
        window.invalidate_character_coordinates();
        cx.notify();
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_content("", window, cx);
    }

    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        if self.on_submit.is_none() {
            return;
        }
        let text = self.content.to_string();
        // 回调不能反过来 borrow 这个实体，所以"提交后清空"由组件自己完成；
        // 回调返回 false（例如正在生成中）则保留内容。
        let accepted = if let Some(on_submit) = &self.on_submit {
            on_submit(&text, window, cx)
        } else {
            false
        };
        if accepted {
            self.set_content("", window, cx);
        }
    }

    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), window, cx);
        } else {
            self.move_to(self.selected_range.start, window, cx)
        }
    }

    fn new_line(&mut self, _: &NewLine, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_composing() {
            self.replace_text_in_range(None, "\n", window, cx);
        }
    }

    fn move_vertically(&mut self, direction: f32, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(layout) = self.current_layout() {
            if let Some(mut point) = layout.text.position_for_index(self.cursor_offset()) {
                point.y += layout.line_height * (direction + 0.5);
                self.move_to(layout.index_at(point), window, cx);
            }
        }
    }

    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), window, cx);
        } else {
            self.move_to(self.selected_range.end, window, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), window, cx);
    }

    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), window, cx);
    }

    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
        self.select_to(self.content.len(), window, cx)
    }

    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
    }

    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                window.play_system_bell();
                return;
            }
            self.select_to(prev, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                window.play_system_bell();
                return;
            }
            self.select_to(next, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;
        window.request_virtual_keyboard();

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), window, cx)
        }
        // 点击进入输入框时同样刷新 IME 坐标，避免第一次组合输入使用默认候选窗位置。
        window.invalidate_character_coordinates();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut item) = cx.read_from_clipboard() else {
            return;
        };
        if self
            .on_paste
            .as_ref()
            .is_some_and(|paste| paste(&mut item, window, cx))
        {
            return;
        }
        if let Some(text) = item.text() {
            let text = if self.draft.is_some() {
                text.replace("\r\n", "\n").replace('\r', "\n")
            } else {
                text.replace("\n", " ")
            };
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        self.current_layout()
            .map_or(0, |layout| layout.index_at(position))
    }

    fn current_layout(&self) -> Option<&InputLayout> {
        self.last_layout
            .as_ref()
            .filter(|layout| layout.content == self.content)
    }

    fn select_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;

        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }

        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_range = self.range_from_utf16(&range_utf16);
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.content.encode_utf16().count())
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        let insertion_start_utf16 = self.offset_to_utf16(range.start);
        let inserted_length_utf16 = new_text.encode_utf16().count();
        self.selected_range = new_selected_range_utf16
            .map(|selected| {
                self.range_from_utf16(
                    &(insertion_start_utf16 + selected.start.min(inserted_length_utf16)
                        ..insertion_start_utf16 + selected.end.min(inserted_length_utf16)),
                )
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;

        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.current_layout()?;
        layout
            .range_bounds(self.range_from_utf16(&range_utf16))
            .into_iter()
            .next()
    }

    fn selection_bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        self.current_layout().map_or_else(Vec::new, |layout| {
            layout.range_bounds(self.range_from_utf16(&range_utf16))
        })
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.current_layout()?;
        layout.viewport.localize(&point)?;
        Some(self.offset_to_utf16(layout.index_at(point)))
    }
}

struct InputLayout {
    text: TextLayout,
    content: SharedString,
    viewport: Bounds<Pixels>,
    line_height: Pixels,
    cursor: usize,
}

impl InputLayout {
    fn index_at(&self, point: Point<Pixels>) -> usize {
        crate::message_selection::index_at(&self.text, point).min(self.content.len())
    }

    fn range_bounds(&self, range: Range<usize>) -> Vec<Bounds<Pixels>> {
        crate::message_selection::range_bounds(&self.text, self.viewport, self.line_height, range)
    }
}

struct TextElement {
    input: Entity<TextInput>,
    text: Option<StyledText>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let style = window.text_style();
        let (display_text, color) = if input.content.is_empty() {
            let color = input
                .draft
                .as_ref()
                .map_or(style.color.opacity(0.65), |draft| draft.placeholder_color);
            (input.placeholder.clone(), color)
        } else {
            (input.content.clone(), style.color)
        };
        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked) = &input.marked_range {
            vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.len(),
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked.end,
                    ..run
                },
            ]
        } else {
            vec![run]
        };
        let text = self
            .text
            .insert(StyledText::new(display_text).with_runs(runs));
        text.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text
            .as_mut()
            .unwrap()
            .prepaint(id, inspector_id, bounds, state, window, cx);
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let text = self.text.as_mut().unwrap();
        let input = self.input.read(cx);
        let focus_handle = input.focus_handle.clone();
        let viewport = input
            .draft
            .as_ref()
            .map_or(bounds, |draft| draft.scroll.bounds());
        let layout = InputLayout {
            text: text.layout().clone(),
            content: input.content.clone(),
            viewport: Bounds::new(
                point(bounds.left(), viewport.top()),
                size(bounds.size.width, viewport.size.height),
            )
            .intersect(&window.content_mask().bounds),
            line_height: window.pixel_snap(window.line_height()),
            cursor: input.cursor_offset(),
        };
        let selected = input.selected_range.clone();
        let caret_color = input
            .draft
            .as_ref()
            .map_or(gpui::blue(), |draft| draft.caret_color);
        let changed = input.last_layout.as_ref().is_none_or(|last| {
            last.cursor != layout.cursor
                || last.content != layout.content
                || last.viewport.size != layout.viewport.size
        });
        let scroll = input
            .draft
            .as_ref()
            .filter(|_| changed)
            .map(|draft| draft.scroll.clone());
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if !selected.is_empty() {
            for selection in layout.range_bounds(selected.clone()) {
                window.paint_quad(fill(selection, rgba(0x3311ff30)));
            }
        }
        text.paint(id, inspector_id, bounds, state, prepaint, window, cx);

        if focus_handle.is_focused(window) {
            if let Some(caret) = layout.text.position_for_index(layout.cursor) {
                if selected.is_empty() {
                    window.paint_quad(fill(
                        Bounds::new(caret, size(px(2.0), layout.line_height)),
                        caret_color,
                    ));
                }
                if let Some(scroll) = scroll {
                    let correction = (layout.viewport.bottom() - caret.y - layout.line_height)
                        .min(px(0.0))
                        + (layout.viewport.top() - caret.y).max(px(0.0));
                    if correction != px(0.0) {
                        window.defer(cx, move |window, _| {
                            let offset = scroll.offset();
                            let y = (offset.y + correction).clamp(-scroll.max_offset().y, px(0.0));
                            if y != offset.y {
                                scroll.set_offset(point(offset.x, y));
                                window.refresh();
                            }
                        });
                    }
                }
            }
        }

        self.input
            .update(cx, |input, _| input.last_layout = Some(layout));
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .w_full()
            .id("text-input")
            .role(Role::TextInput)
            .aria_label(if self.draft.is_some() {
                "消息草稿".into()
            } else {
                self.placeholder.clone()
            })
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .line_height(px(20.))
            .text_size(px(14.))
            .when_some(self.draft.as_ref(), |input, draft| {
                input
                    .key_context("TextInput DraftInput")
                    .line_height(px(24.0))
                    .max_h(draft.max_height)
                    .overflow_y_scroll()
                    .track_scroll(&draft.scroll)
                    .on_action(cx.listener(Self::new_line))
                    .on_action(cx.listener(|input, _: &Up, window, cx| {
                        input.move_vertically(-1.0, window, cx);
                    }))
                    .on_action(cx.listener(|input, _: &Down, window, cx| {
                        input.move_vertically(1.0, window, cx);
                    }))
            })
            .when(self.draft.is_none(), |input| input.whitespace_nowrap())
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .when(self.draft.is_none(), |input| input.px_1())
                    .child(TextElement {
                        input: cx.entity(),
                        text: None,
                    }),
            )
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// 注册文本输入的按键绑定（应用启动时调用一次）。
pub fn init(cx: &mut App) {
    let (select_all, paste, copy, cut) = if cfg!(target_os = "macos") {
        ("cmd-a", "cmd-v", "cmd-c", "cmd-x")
    } else {
        ("ctrl-a", "ctrl-v", "ctrl-c", "ctrl-x")
    };
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new(select_all, SelectAll, Some("TextInput")),
        KeyBinding::new(paste, Paste, Some("TextInput")),
        KeyBinding::new(copy, Copy, Some("TextInput")),
        KeyBinding::new(copy, Copy, Some("TranscriptSelection")),
        KeyBinding::new(cut, Cut, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("enter", Submit, Some("TextInput")),
        KeyBinding::new("shift-enter", NewLine, Some("DraftInput")),
        KeyBinding::new("up", Up, Some("DraftInput")),
        KeyBinding::new("down", Down, Some("DraftInput")),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some("TextInput")),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn paste_shortcut_replaces_selected_draft_with_multiline_text(cx: &mut TestAppContext) {
        cx.update(init);
        let (draft, cx) = cx.add_window_view(|window, cx| {
            let mut input = TextInput::new("消息草稿", cx).multiline();
            input.set_content("旧草稿", window, cx);
            window.focus(&input.focus_handle(cx), cx);
            input
        });
        cx.update(|_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(
                "第一行\r\nSecond line".to_owned(),
            ));
        });
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a cmd-v"
        } else {
            "ctrl-a ctrl-v"
        });
        cx.read(|cx| assert_eq!(draft.read(cx).content(), "第一行\nSecond line"));
    }
}
