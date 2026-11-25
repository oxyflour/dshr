use super::{AdcPanelView, ChatState};
use dshr_agent_core::ToolOutcome;
use dshr_plugin_host::{ChatNodeContext, PluginHost, SlotTarget};
use dshr_session_store::{Event, SessionLog};
use dshr_ui_contributions::{parse_tree, Action};
use serde_json::{json, Value};
use std::collections::VecDeque;

const MAX_TOOL_VIEWS: usize = 16;

#[derive(Clone)]
pub(super) struct ToolViewBinding {
    pub target: SlotTarget,
    pub trial: bool,
    node_key: Option<String>,
}

pub(super) fn call_view_bindings(
    host: &PluginHost,
    tool: &str,
    trial: bool,
) -> Vec<ToolViewBinding> {
    host.chat_node_targets(tool)
        .into_iter()
        .map(|(target, key)| ToolViewBinding {
            target,
            trial,
            node_key: Some(key.to_owned()),
        })
        .chain(
            host.tool_slot_targets(tool)
                .into_iter()
                .map(|target| ToolViewBinding {
                    target,
                    trial,
                    node_key: None,
                }),
        )
        .collect()
}

pub(super) fn native_tool_outcome(host: &PluginHost, tool: &str, mut value: Value) -> ToolOutcome {
    let published = host.take_tool_chat_node(tool, &mut value);
    let mut outcome = ToolOutcome::success(json!([{ "type": "json", "value": value }]), value);
    outcome.meta = published.map(|node| json!({"adc_node": node}));
    outcome
}

#[derive(Default)]
pub(super) struct ToolViewRequests(VecDeque<(u64, u64, ToolViewBinding)>);

impl ToolViewRequests {
    pub(super) fn settled(
        &mut self,
        call_seq: u64,
        result_seq: u64,
        views: Vec<ToolViewBinding>,
        meta: Option<&Value>,
    ) {
        let node = meta.and_then(|meta| meta["adc_node"]["key"].as_str());
        for view in views
            .into_iter()
            .filter(|view| view.node_key.as_deref().is_none_or(|key| Some(key) == node))
            .take(MAX_TOOL_VIEWS)
        {
            if self.0.len() == MAX_TOOL_VIEWS {
                self.0.pop_front();
            }
            self.0.push_back((call_seq, result_seq, view));
        }
    }
}

impl ChatState {
    pub(super) fn mount_tool_views(&mut self, requests: ToolViewRequests) {
        for (call_seq, result_seq, binding) in requests.0 {
            let Some((call, result)) = completed_call(&self.log, call_seq, result_seq) else {
                continue;
            };
            let plugins = if binding.trial {
                let Some(scope) = self.creator_trials.get_mut(&self.session_id) else {
                    continue;
                };
                &mut scope.plugins
            } else {
                &mut self.plugins
            };
            let Some(slot) = plugins.active_slot(binding.target).cloned() else {
                continue;
            };
            let (Some(plugin_id), Some(view)) = (slot.plugin_id.clone(), slot.view.clone()) else {
                continue;
            };
            let Some(tool) = call.payload["tool"].as_str() else {
                continue;
            };
            let rendered = if let Some(key) = binding.node_key {
                let Some(anchor) = std::num::NonZeroU64::new(call_seq) else {
                    continue;
                };
                let context = ChatNodeContext {
                    key,
                    call_seq: anchor,
                    data: result.payload["meta"]["adc_node"]["data"].clone(),
                };
                plugins.render_chat_node_slot(binding.target, tool, &context)
            } else {
                let context = json!({"call_seq": call_seq, "tool": tool, "arguments": call.payload["arguments"], "value": result.payload["value"]});
                plugins.render_tool_slot(binding.target, &context)
            };
            let Ok(tree) = rendered else {
                continue;
            };
            if !valid_readonly_tree(&tree) {
                continue;
            }
            self.adc_panels.push(AdcPanelView {
                plugin_id,
                title: slot.label.clone().unwrap_or_default(),
                trial: binding.trial,
                trial_call_seq: None,
                tool_call_seq: Some(call_seq),
                view,
                tree: Some(tree),
                error: None,
                slot: Some(slot),
            });
        }
        let cached = self
            .adc_panels
            .iter()
            .filter(|view| view.tool_call_seq.is_some())
            .count();
        let mut excess = cached.saturating_sub(MAX_TOOL_VIEWS);
        self.adc_panels.retain(|view| {
            if excess > 0 && view.tool_call_seq.is_some() {
                excess -= 1;
                return false;
            }
            true
        });
    }

    pub(super) fn take_active_tool_views(&mut self) -> Vec<AdcPanelView> {
        std::mem::take(&mut self.adc_panels)
            .into_iter()
            .filter(|view| {
                if view.tool_call_seq.is_none() {
                    return false;
                }
                let plugins = if view.trial {
                    self.creator_trials
                        .get(&self.session_id)
                        .map(|scope| &scope.plugins)
                } else {
                    Some(&self.plugins)
                };
                plugins
                    .zip(view.slot.as_ref())
                    .is_some_and(|(host, slot)| host.active_slot(SlotTarget::from(slot)).is_some())
            })
            .collect()
    }
}

fn completed_call(log: &SessionLog, call_seq: u64, result_seq: u64) -> Option<(&Event, &Event)> {
    let event = |seq| -> Option<&Event> {
        let index = usize::try_from(seq).ok()?.checked_sub(1)?;
        log.events().get(index).filter(|event| event.seq == seq)
    };
    let call = event(call_seq)?;
    let result = event(result_seq)?;
    if call.event_type != "tool.call"
        || call.payload["origin"] != "native"
        || result.event_type != "tool.result"
        || result.payload["call_seq"].as_u64() != Some(call_seq)
        || result.payload["is_error"].as_bool() != Some(false)
    {
        return None;
    }
    Some((call, result))
}

pub(super) fn valid_readonly_tree(tree: &Value) -> bool {
    let parsed = parse_tree(tree);
    let mut valid = parsed.diagnostics.is_empty();
    parsed.root.walk(&mut |node| {
        for prop in ["onPress", "onChange", "onSubmit"] {
            if let Some(action) = node.action(prop) {
                valid &= matches!(action, Action::Copy { .. } | Action::OpenExternal { .. });
            }
        }
    });
    valid
}
