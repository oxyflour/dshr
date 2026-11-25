use super::ChatState;
use dshr_plugin_api::PluginError;
use dshr_plugin_host::{HostError, SlotTarget, COMMAND_SLOT, MENU_ITEM_SLOT};
use dshr_ui_contributions::RegistrationRecord;
use std::collections::BTreeMap;

#[derive(Clone)]
pub(crate) struct AdcActionBinding {
    pub(crate) registration: RegistrationRecord,
    pub(crate) trial: bool,
}

impl AdcActionBinding {
    pub(crate) fn target(&self) -> SlotTarget {
        SlotTarget::from(&self.registration)
    }
}

impl ChatState {
    pub(crate) fn adc_action_entries(&self) -> Vec<AdcActionBinding> {
        let mut actions = BTreeMap::new();
        let trial = self
            .creator_trials
            .get(&self.session_id)
            .map(|scope| (&scope.plugins, true));
        for (host, trial) in trial.into_iter().chain([(&self.plugins, false)]) {
            for slot in [COMMAND_SLOT, MENU_ITEM_SLOT] {
                for record in host.slot_registrations(slot) {
                    let id = if slot == COMMAND_SLOT {
                        &record.registration_id
                    } else {
                        &record.key
                    };
                    if let Some(id) = id.as_deref() {
                        actions
                            .entry((slot, id))
                            .or_insert_with(|| AdcActionBinding {
                                registration: record.clone(),
                                trial,
                            });
                    }
                }
            }
        }
        actions.into_values().collect()
    }

    pub(crate) fn apply_adc_registered_action(
        &mut self,
        target: SlotTarget,
    ) -> Result<(), HostError> {
        if self.headless || !self.permission.can_write() {
            return Err(PluginError::Invalid(
                "ADC commands require a writable desktop session".to_owned(),
            )
            .into());
        }
        let inactive = || HostError::NotFound("inactive ADC command or menu item".to_owned());
        let entry = self
            .adc_action_entries()
            .into_iter()
            .find(|entry| entry.target() == target)
            .ok_or_else(inactive)?;
        let host = if entry.trial {
            &mut self
                .creator_trials
                .get_mut(&self.session_id)
                .ok_or_else(inactive)?
                .plugins
        } else {
            &mut self.plugins
        };
        host.dispatch_registered_action(target)?;
        self.refresh_adc_panels();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{creator::TrialScope, ChatRuntime, DesktopModel, DesktopSettings};
    use dshr_plugin_api::{Plugin, PluginContext};
    use dshr_plugin_host::COMPOSER_ADD_MENU;
    use dshr_session_store::SessionIndex;
    use dshr_tool_runtime::{ApprovalBroker, PermissionMode};
    use serde_json::{json, Value};
    use std::sync::{atomic::AtomicBool, Arc, Mutex};

    #[test]
    fn trial_commands_shadow_installed_commands_and_expire_on_session_switch() {
        struct CommandPlugin(Arc<Mutex<Vec<String>>>);
        impl Plugin for CommandPlugin {
            fn id(&self) -> &str {
                "demo.actions"
            }
            fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
                ctx.register_slots([
                    json!({"slot_id": COMMAND_SLOT, "plugin_id": self.id(), "id": "demo.refresh",
                        "label": "Refresh", "run": "refresh"}),
                    json!({"slot_id": MENU_ITEM_SLOT, "plugin_id": self.id(), "id": "demo.menu",
                        "key": COMPOSER_ADD_MENU, "label": "Refresh menu", "run": "refresh-menu"}),
                ]);
                Ok(())
            }
            fn call(&mut self, _: &str, _: &Value) -> Result<Value, PluginError> {
                unreachable!()
            }
            fn handle_action(
                &mut self,
                run: &str,
                action: &str,
                payload: &Value,
            ) -> Result<(), PluginError> {
                assert_eq!(action, "command");
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("{run}:{}", payload["id"].as_str().unwrap()));
                Ok(())
            }
        }

        let root = tempfile::tempdir().unwrap();
        let store = SessionIndex::new(root.path().join("sessions"));
        for id in ["first", "second"] {
            store
                .create_root(id, &root.path().to_string_lossy(), "1")
                .unwrap();
        }
        let mut state = ChatState::open_in(
            store.path_for("first"),
            "first",
            &Arc::new(AtomicBool::new(false)),
            ApprovalBroker::noninteractive(),
            DesktopSettings {
                model: Arc::new(DesktopModel::Mock),
                permission: Some(PermissionMode::WorkspaceWrite),
                reasoning: None,
            },
            ChatRuntime::Desktop {
                allow_spawn: false,
                sidecar: false,
                diagnostics: false,
            },
        );
        let installed_calls = Arc::new(Mutex::new(Vec::new()));
        state
            .plugins
            .load(Box::new(CommandPlugin(installed_calls.clone())))
            .unwrap();
        let installed = state.adc_action_entries()[0].target();
        let trial_calls = Arc::new(Mutex::new(Vec::new()));
        let mut scope = TrialScope::new(state.permission.clone(), root.path());
        scope
            .plugins
            .load(Box::new(CommandPlugin(trial_calls.clone())))
            .unwrap();
        state.creator_trials.insert("first".to_owned(), scope);
        let trial = state.adc_action_entries();
        assert_eq!(trial.len(), 2);
        assert!(trial.iter().all(|entry| entry.trial));
        for entry in &trial {
            state.apply_adc_registered_action(entry.target()).unwrap();
        }
        assert_eq!(
            *trial_calls.lock().unwrap(),
            ["refresh:demo.refresh", "refresh-menu:demo.menu"]
        );
        assert!(state.apply_adc_registered_action(installed).is_err());
        state.switch_session(store.path_for("second"), "second");
        assert!(state
            .apply_adc_registered_action(trial[0].target())
            .is_err());
        state.permission.set(PermissionMode::ReadOnly);
        assert!(state.apply_adc_registered_action(installed).is_err());
        assert!(installed_calls.lock().unwrap().is_empty());
        state.permission.set(PermissionMode::WorkspaceWrite);
        state.apply_adc_registered_action(installed).unwrap();
        assert_eq!(*installed_calls.lock().unwrap(), ["refresh:demo.refresh"]);
        state.plugins.unload("demo.actions").unwrap();
        assert!(state.adc_action_entries().is_empty());
        assert!(state.apply_adc_registered_action(installed).is_err());
    }
}
