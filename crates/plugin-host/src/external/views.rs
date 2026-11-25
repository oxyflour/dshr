use super::{ExternalPlugin, HostError, RENDER_TIMEOUT_MS};
use dshr_plugin_api::{PanelContribution, PluginContext, PluginError};
use dshr_plugin_sdk::{kind, ActionPayload, Envelope};
use serde_json::{json, Value};
use std::time::Duration;

pub(super) fn take_chat_node(result: &mut Value) -> Option<Value> {
    let success = result["is_error"].as_bool() == Some(false);
    let node = result
        .get_mut("meta")?
        .as_object_mut()?
        .remove("adc_node")?;
    success.then_some(node)
}

impl ExternalPlugin {
    pub(super) fn render_registered_view(&mut self, view: &str) -> Result<Value, PluginError> {
        self.render_view(view).map_err(|error| {
            PluginError::Invalid(format!(
                "external plugin `{}` view `{view}`: {error}",
                self.plugin_id
            ))
        })
    }

    pub(super) fn dispatch_view_action(
        &mut self,
        view: &str,
        action: &str,
        payload: &Value,
    ) -> Result<(), PluginError> {
        self.send_action(view, action, payload).map_err(|error| {
            PluginError::Invalid(format!(
                "external plugin `{}` action `{action}`: {error}",
                self.plugin_id
            ))
        })
    }

    pub(super) fn register_views(&self, ctx: &mut PluginContext) {
        for panel in &self.ready.panels {
            ctx.register_panel(
                PanelContribution::new(panel.id.clone(), panel.label.clone(), panel.slot.clone())
                    .with_view(panel.view.clone()),
            );
        }
        ctx.register_slots(self.ready.slots.iter().cloned());
    }

    pub(super) fn render_context(
        &mut self,
        view: &str,
        field: &str,
        context: &Value,
    ) -> Result<Value, PluginError> {
        self.render_request(
            json!({"view": view, "state_seq": 0, (field): context}),
            Duration::from_secs(1),
        )
        .map_err(|error| PluginError::Invalid(error.to_string()))
    }

    pub fn render_view(&mut self, view: &str) -> Result<Value, HostError> {
        self.render_request(
            json!({"view": view, "state_seq": 0}),
            Duration::from_millis(RENDER_TIMEOUT_MS),
        )
    }

    pub(super) fn render_request(
        &mut self,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, HostError> {
        if self.failed || self.disposed {
            return Err(self.host_error("plugin process is not alive".to_owned()));
        }
        let request_id = self.next_request_id();
        let request = Envelope::with_payload(kind::UI_RENDER, payload)
            .map_err(|err| self.host_error(err.to_string()))?
            .with_request_id(request_id)
            .with_generation(self.generation);
        match self.exchange(request, timeout) {
            Ok(envelope) => {
                if let Some(error) = envelope.error {
                    return Err(self.host_error(format!("{}: {}", error.code, error.message)));
                }
                envelope
                    .payload
                    .get("tree")
                    .cloned()
                    .ok_or_else(|| self.host_error("ui.result is missing `tree`".to_owned()))
            }
            Err(error) => {
                self.mark_failed(&error.to_string());
                Err(self.host_error(error.to_string()))
            }
        }
    }

    /// Send an ADC action without waiting for a response. Refresh the view separately.
    pub fn send_action(
        &mut self,
        view: &str,
        action: &str,
        payload: &Value,
    ) -> Result<(), HostError> {
        if self.failed || self.disposed {
            return Err(self.host_error("plugin process is not alive".to_owned()));
        }
        let envelope = Envelope::with_payload(
            kind::UI_ACTION,
            ActionPayload {
                view: view.to_owned(),
                action: action.to_owned(),
                payload: payload.clone(),
            },
        )
        .map_err(|err| self.host_error(err.to_string()))?
        .with_generation(self.generation);
        self.send(&envelope).map_err(|error| {
            self.mark_failed(&error.to_string());
            self.host_error(error.to_string())
        })
    }
}
