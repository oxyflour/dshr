use crate::{slash_commands, AdcActionBinding, ChatApp, SlashCommand};
use dshr_plugin_host::{CommandShortcut, SlotTarget, COMMAND_SLOT, MENU_ITEM_SLOT};
use gpui::{Context, KeyDownEvent, Keystroke, Modifiers, Window};

#[derive(Clone, Copy)]
pub(crate) enum CommandChoice {
    Builtin(SlashCommand),
    Plugin(SlotTarget),
}

impl ChatApp {
    pub(crate) fn can_run_adc_commands(&self) -> bool {
        !self.busy
            && self.permission.can_write()
            && self.attachment_preview.is_none()
            && self.adc_editor.is_none()
            && self.pending_external.is_none()
    }

    pub(crate) fn matching_commands<'a>(
        &'a self,
        text: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a str, CommandChoice)> + 'a {
        let prefix = text.strip_prefix('/');
        let enabled = self.can_run_adc_commands();
        slash_commands(text)
            .map(|entry| {
                (
                    entry.name(),
                    entry.description(),
                    CommandChoice::Builtin(entry),
                )
            })
            .chain(self.adc_actions.iter().filter_map(move |entry| {
                let record = &entry.registration;
                let id = record.registration_id.as_deref()?;
                (enabled
                    && record.slot_id == COMMAND_SLOT
                    && prefix.is_some_and(|prefix| id.starts_with(prefix)))
                .then(|| {
                    (
                        id,
                        record.label.as_deref().unwrap_or_default(),
                        CommandChoice::Plugin(entry.target()),
                    )
                })
            }))
    }

    pub(crate) fn adc_menu_item(&self, menu: &str) -> Option<(&str, SlotTarget)> {
        if !self.can_run_adc_commands() {
            return None;
        }
        self.adc_actions.iter().find_map(|entry| {
            let record = &entry.registration;
            if record.slot_id != MENU_ITEM_SLOT || record.key.as_deref() != Some(menu) {
                return None;
            }
            Some((record.label.as_deref()?, entry.target()))
        })
    }

    pub(crate) fn run_command_choice(
        &mut self,
        choice: CommandChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match choice {
            CommandChoice::Builtin(command) => self.run_slash_command(command, window, cx),
            CommandChoice::Plugin(target) => {
                self.run_adc_command(target, cx);
                self.focus_composer(window, cx);
            }
        }
    }

    pub(crate) fn run_adc_command(&mut self, target: SlotTarget, cx: &mut Context<Self>) {
        if !self.can_run_adc_commands() {
            return;
        }
        if let Ok(mut state) = self.state.try_lock() {
            if state.session_id != self.session_id {
                return;
            }
            match state.apply_adc_registered_action(target) {
                Ok(()) => {
                    self.adc_panels = state.adc_panels.clone();
                    self.adc_actions = state.adc_action_entries();
                }
                Err(error) => eprintln!("[adc] command dispatch failed: {error}"),
            }
            cx.notify();
        }
    }

    pub(crate) fn run_adc_view_command(
        &mut self,
        plugin: &str,
        trial: bool,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        let target = self.adc_actions.iter().find_map(|entry| {
            let record = &entry.registration;
            (entry.trial == trial
                && record.slot_id == COMMAND_SLOT
                && record.plugin_id.as_deref() == Some(plugin)
                && record.registration_id.as_deref() == Some(id))
            .then(|| entry.target())
        });
        if let Some(target) = target {
            self.run_adc_command(target, cx);
        }
    }

    pub(crate) fn adc_command_keydown(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_run_adc_commands()
            || event.is_held
            || event.prefer_character_input
            || self.input.read(cx).is_composing()
            || !event.keystroke.modifiers.modified()
        {
            return;
        }
        if let Some(target) = shortcut_target(&self.adc_actions, &event.keystroke) {
            cx.stop_propagation();
            self.run_adc_command(target, cx);
        }
    }
}

fn shortcut_target(actions: &[AdcActionBinding], keystroke: &Keystroke) -> Option<SlotTarget> {
    let mut matches = actions.iter().filter(|entry| {
        entry.registration.slot_id == COMMAND_SLOT
            && entry
                .registration
                .field("keys")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|keys| {
                    keys.iter()
                        .filter_map(serde_json::Value::as_str)
                        .filter_map(CommandShortcut::parse)
                        .any(|binding| {
                            keystroke.key.eq_ignore_ascii_case(&binding.key)
                                && keystroke.modifiers
                                    == Modifiers {
                                        control: binding.control,
                                        platform: binding.platform,
                                        alt: binding.alt,
                                        shift: binding.shift,
                                        ..Modifiers::default()
                                    }
                        })
                })
    });
    let target = matches.next()?.target();
    matches.next().is_none().then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshr_ui_contributions::ContributionRegistry;
    use serde_json::json;

    #[test]
    fn plugin_shortcuts_only_choose_an_unambiguous_chord() {
        let mut slots = ContributionRegistry::new();
        for (id, generation, key) in [("one", 1, "Ctrl+Shift+G"), ("two", 2, "shift-ctrl-g")] {
            slots.register(&json!({"slot_id": COMMAND_SLOT, "id": id, "label": id, "run": id, "keys": [key]}), generation).unwrap();
        }
        let entries: Vec<_> = slots
            .registrations_for(COMMAND_SLOT)
            .into_iter()
            .map(|registration| AdcActionBinding {
                registration: registration.clone(),
                trial: false,
            })
            .collect();
        let key = Keystroke::parse("ctrl-shift-g").unwrap();
        assert_eq!(
            shortcut_target(&entries[..1], &key),
            Some(entries[0].target())
        );
        assert_eq!(shortcut_target(&entries, &key), None);
        assert_eq!(
            shortcut_target(&entries, &Keystroke::parse("ctrl-g").unwrap()),
            None
        );
    }
}
