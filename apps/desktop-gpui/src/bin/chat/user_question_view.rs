use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity, FontWeight};

use super::dsh_user_questions::Prompt;
use super::{chrome_button, ChatApp, TextInput, Theme};

pub(super) struct QuestionDraft {
    prompt: Prompt,
    index: usize,
    selected: Vec<Vec<String>>,
    inputs: Vec<Entity<TextInput>>,
    error: Option<&'static str>,
}

impl ChatApp {
    pub(super) fn sync_user_questions(&mut self, cx: &mut Context<Self>) {
        let Some(broker) = &self.questions else {
            return;
        };
        let displayed = self
            .question_draft
            .as_ref()
            .map(|draft| draft.prompt.ticket);
        if let Some(prompt) = broker.first(&self.session_id, displayed) {
            let inputs = prompt
                .questions
                .iter()
                .map(|question| {
                    let placeholder = if question.options.is_empty() {
                        "输入答复"
                    } else {
                        "补充答复（可选）"
                    };
                    cx.new(|cx| TextInput::new(placeholder, cx))
                })
                .collect();
            self.question_draft = Some(QuestionDraft {
                selected: vec![Vec::new(); prompt.questions.len()],
                prompt,
                index: 0,
                inputs,
                error: None,
            });
            cx.notify();
        } else if displayed.is_some_and(|ticket| !broker.showing(&self.session_id, ticket)) {
            self.question_draft = None;
            cx.notify();
        }
    }

    fn answer_user_questions(&mut self, ticket: u64, cancel: bool, cx: &mut Context<Self>) {
        let (Some(broker), Some(draft)) = (&self.questions, self.question_draft.as_mut()) else {
            return;
        };
        if ticket != draft.prompt.ticket {
            return;
        }
        let answers = (!cancel).then(|| {
            draft
                .selected
                .iter()
                .cloned()
                .zip(
                    draft
                        .inputs
                        .iter()
                        .map(|input| input.read(cx).content().to_owned()),
                )
                .collect()
        });
        match broker.resolve(&self.session_id, ticket, answers) {
            Ok(()) => self.sync_user_questions(cx),
            Err(error) => {
                draft.error = Some(error);
                cx.notify();
            }
        }
    }

    pub(super) fn user_question_bar(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let draft = self.question_draft.as_ref()?;
        let index = draft.index;
        let question = &draft.prompt.questions[index];
        let ticket = draft.prompt.ticket;
        let palette = &theme.palette;
        let count = draft.prompt.questions.len();
        let multi_select = question.multi_select;
        let options =
            question
                .options
                .iter()
                .enumerate()
                .map(|(option_index, option)| {
                    let label = option.label.clone();
                    let selected = draft.selected[index].contains(&label);
                    div()
                        .id(("question-option", option_index))
                        .role(gpui::Role::Button)
                        .aria_label(label.clone())
                        .cursor_pointer()
                        .w_full()
                        .rounded(theme.radii.sm)
                        .border_1()
                        .border_color(palette.color(if selected {
                            palette.label_primary
                        } else {
                            palette.border_l3
                        }))
                        .px(px(8.0))
                        .py(px(4.0))
                        .child(format!("{} {}", if selected { "✓" } else { "○" }, label))
                        .when_some(option.description.clone(), |el, description| {
                            el.child(div().text_size(theme.type_scale.small).child(description))
                        })
                        .on_click(cx.listener(move |chat, _, _, cx| {
                            let Some(draft) = chat.question_draft.as_mut().filter(|draft| {
                                draft.prompt.ticket == ticket && draft.index == index
                            }) else {
                                return;
                            };
                            let selections = &mut draft.selected[index];
                            if let Some(position) =
                                selections.iter().position(|selection| selection == &label)
                            {
                                selections.remove(position);
                            } else {
                                if !multi_select {
                                    selections.clear();
                                }
                                selections.push(label.clone());
                            }
                            draft.error = None;
                            cx.notify();
                        }))
                });
        let last = index + 1 == count;
        Some(
            div()
                .flex()
                .justify_center()
                .px(px(16.0))
                .pb(px(6.0))
                .child(
                    div()
                        .w_full()
                        .max_w(theme.layout.composer_max_width)
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .px(px(14.0))
                        .py(px(10.0))
                        .rounded(theme.radii.md)
                        .border_1()
                        .border_color(palette.color(palette.border_l3))
                        .bg(palette.color(palette.bg_layer_1))
                        .text_color(palette.color(palette.label_primary))
                        .text_size(theme.type_scale.secondary)
                        .child(div().font_weight(FontWeight::MEDIUM).child(
                            match &question.header {
                                Some(header) if count == 1 => header.clone(),
                                Some(header) => format!("{header} · {} / {count}", index + 1),
                                None if count == 1 => "需要你回答".to_owned(),
                                None => format!("问题 {} / {count}", index + 1),
                            },
                        ))
                        .child(
                            div()
                                .id("question-content")
                                .max_h(px(170.0))
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .child(question.question.clone())
                                .children(options),
                        )
                        .child(
                            div()
                                .rounded(theme.radii.sm)
                                .border_1()
                                .border_color(palette.color(palette.border_l3))
                                .px(px(8.0))
                                .py(px(4.0))
                                .child(draft.inputs[index].clone()),
                        )
                        .when_some(draft.error, |el, error| el.child(error))
                        .child(
                            div()
                                .flex()
                                .gap(px(6.0))
                                .child(
                                    chrome_button("question-cancel", "取消提问", theme).on_click(
                                        cx.listener(move |chat, _, _, cx| {
                                            chat.answer_user_questions(ticket, true, cx)
                                        }),
                                    ),
                                )
                                .child(div().flex_1())
                                .when(index > 0, |row| {
                                    row.child(
                                        chrome_button("question-back", "上一题", theme).on_click(
                                            cx.listener(move |chat, _, _, cx| {
                                                if let Some(draft) =
                                                    chat.question_draft.as_mut().filter(|draft| {
                                                        draft.prompt.ticket == ticket
                                                            && draft.index == index
                                                    })
                                                {
                                                    draft.index -= 1;
                                                    draft.error = None;
                                                    cx.notify();
                                                }
                                            }),
                                        ),
                                    )
                                })
                                .child(
                                    chrome_button(
                                        "question-next",
                                        if last { "提交答复" } else { "下一题" },
                                        theme,
                                    )
                                    .on_click(cx.listener(
                                        move |chat, _, _, cx| {
                                            if last {
                                                chat.answer_user_questions(ticket, false, cx);
                                            } else if let Some(draft) =
                                                chat.question_draft.as_mut().filter(|draft| {
                                                    draft.prompt.ticket == ticket
                                                        && draft.index == index
                                                })
                                            {
                                                draft.index += 1;
                                                draft.error = None;
                                                cx.notify();
                                            }
                                        },
                                    )),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}
