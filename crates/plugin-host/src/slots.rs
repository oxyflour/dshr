use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use dshr_plugin_api::PluginError;
use dshr_tool_runtime::ToolDefinition;
use dshr_ui_contributions::{
    ContributionRegistry, Generation, RegistrationHandle, RegistrationRecord,
};
use serde_json::Value;

pub const TOOL_CALL_VIEW: &str = "tool.call.toolview";
pub const TOOL_RESULT_EXTRA: &str = "tool.result.extra";
pub const VIEWER_TAB: &str = "viewer.tab";
pub const CHAT_NODE: &str = "conversation.chat.node";
pub const COMMAND_SLOT: &str = "command";
pub const MENU_ITEM_SLOT: &str = "menu.item";
pub const COMPOSER_ADD_MENU: &str = "composer.add";
#[cfg(windows)]
pub const APPLICATION_MENU: &str = "window.application";
#[cfg(windows)]
pub const EDIT_MENU: &str = "window.edit";

pub const DESKTOP_MENU_KEYS: &[&str] = &[
    COMPOSER_ADD_MENU,
    #[cfg(windows)]
    APPLICATION_MENU,
    #[cfg(windows)]
    EDIT_MENU,
];
pub const BUILTIN_COMMAND_IDS: &[&str] = &["new", "creator", "search", "turns", "plugins", "theme"];

const DESKTOP_ACTION_SLOTS: &[(&str, &str)] = &[
    (COMMAND_SLOT, "Composer / command and Add menus"),
    (MENU_ITEM_SLOT, "Named desktop menu (see menu_keys)"),
];

pub const DESKTOP_VIEW_SLOTS: &[(&str, &str)] = &[
    (
        CHAT_NODE,
        "Conversation flow, as a standalone node published by the plugin's tool",
    ),
    (
        VIEWER_TAB,
        "Right sidebar file preview for a selected delivered file",
    ),
    ("conversation.input.left", "Composer toolbar, on the left"),
    (
        "conversation.input.right",
        "Composer toolbar, before the send button",
    ),
    ("settings.general.item", "Settings → General"),
    ("settings.section", "Settings, as a separate page"),
    (
        "workspace.sidebar.tab",
        "Conversation right sidebar, as a tab",
    ),
    ("workspace.page", "Left sidebar navigation and central page"),
    (
        "status.bar.item",
        "Status bar at the bottom of the main content area",
    ),
    (
        TOOL_CALL_VIEW,
        "Body of a successful call to the plugin's declared tool",
    ),
    (
        TOOL_RESULT_EXTRA,
        "Below successful tool results from the same plugin",
    ),
];

pub fn is_tool_view_slot(slot: &str) -> bool {
    matches!(slot, TOOL_CALL_VIEW | TOOL_RESULT_EXTRA)
}

pub fn desktop_slots() -> impl Iterator<Item = (&'static str, &'static str)> {
    DESKTOP_VIEW_SLOTS
        .iter()
        .chain(DESKTOP_ACTION_SLOTS)
        .copied()
}

#[derive(Debug, PartialEq, Eq)]
pub struct CommandShortcut {
    pub key: String,
    pub control: bool,
    pub platform: bool,
    pub alt: bool,
    pub shift: bool,
}

impl CommandShortcut {
    pub fn parse(binding: &str) -> Option<Self> {
        let binding = binding.to_ascii_lowercase();
        let mut parts = binding.rsplit(if binding.contains('+') { '+' } else { '-' });
        let key = match parts.next()? {
            "esc" => "escape",
            key => key,
        };
        let named_key = matches!(
            key,
            "escape"
                | "enter"
                | "space"
                | "tab"
                | "backspace"
                | "delete"
                | "insert"
                | "home"
                | "end"
                | "pageup"
                | "pagedown"
                | "left"
                | "right"
                | "up"
                | "down"
        );
        let function_key = key
            .strip_prefix('f')
            .and_then(|key| key.parse::<u8>().ok())
            .is_some_and(|number| (1..=24).contains(&number) && key == format!("f{number}"));
        if !(named_key
            || function_key
            || key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric())
        {
            return None;
        }
        let mut shortcut = Self {
            key: key.to_owned(),
            control: false,
            platform: false,
            alt: false,
            shift: false,
        };
        for part in parts {
            let flag = match part {
                "ctrl" => &mut shortcut.control,
                "cmd" | "meta" | "super" => &mut shortcut.platform,
                "alt" => &mut shortcut.alt,
                "shift" => &mut shortcut.shift,
                _ => return None,
            };
            if std::mem::replace(flag, true) {
                return None;
            }
        }
        (shortcut.control || shortcut.platform).then_some(shortcut)
    }
}

pub(super) fn validate_tool_declarations(
    tools: &[ToolDefinition],
    declarations: &[Value],
) -> Result<(), PluginError> {
    for (index, declaration) in declarations.iter().enumerate() {
        let failure = match declaration["slot_id"].as_str() {
            Some(TOOL_CALL_VIEW)
                if !tools
                    .iter()
                    .any(|tool| declaration["key"].as_str() == Some(&tool.name)) =>
            {
                "tool.call.toolview.key must name a tool declared by the same plugin"
            }
            Some(TOOL_RESULT_EXTRA) if tools.is_empty() => {
                "tool.result.extra requires a tool declared by the same plugin"
            }
            _ => continue,
        };
        return Err(PluginError::Invalid(format!("slots[{index}]: {failure}")));
    }
    Ok(())
}

pub(super) fn matches_tool(record: &RegistrationRecord, plugin_id: &str, tool: &str) -> bool {
    record.plugin_id.as_deref() == Some(plugin_id)
        && match record.slot_id.as_str() {
            TOOL_CALL_VIEW => record.key.as_deref() == Some(tool),
            TOOL_RESULT_EXTRA => true,
            _ => false,
        }
}

// The UI can retain an action from a prior workspace, trial host or plugin instance.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotTarget {
    handle: RegistrationHandle,
    generation: Generation,
}

impl From<&RegistrationRecord> for SlotTarget {
    fn from(record: &RegistrationRecord) -> Self {
        Self {
            handle: record.handle,
            generation: record.generation,
        }
    }
}

#[derive(Clone)]
pub(super) struct PluginSlots {
    registry: ContributionRegistry,
    owners: BTreeMap<String, Generation>,
}

impl Default for PluginSlots {
    fn default() -> Self {
        let mut registry = ContributionRegistry::new();
        registry
            .set_closed_key_domain(
                MENU_ITEM_SLOT,
                DESKTOP_MENU_KEYS.iter().map(|key| (*key).to_owned()),
            )
            .expect("builtin menu.item defines a closed key domain");
        Self {
            registry,
            owners: BTreeMap::new(),
        }
    }
}

impl PluginSlots {
    pub(super) fn stage(
        &self,
        owner: &str,
        declarations: Vec<Value>,
    ) -> Result<Option<Self>, PluginError> {
        if declarations.is_empty() && !self.owners.contains_key(owner) {
            return Ok(None);
        }

        let mut staged = self.clone();
        staged.retract(owner);
        if declarations.is_empty() {
            return Ok(Some(staged));
        }

        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        for (index, declaration) in declarations.iter().enumerate() {
            if declaration["plugin_id"].as_str() != Some(owner) {
                return Err(PluginError::Invalid(format!(
                    "slots[{index}].plugin_id must match plugin `{owner}`"
                )));
            }
            if declaration["slot_id"] == CHAT_NODE {
                let key = declaration["key"]
                    .as_str()
                    .filter(|key| dshr_plugin_sdk::ChatNodeContext::valid_key(key))
                    .ok_or_else(|| {
                        PluginError::Invalid(format!(
                            "slots[{index}]: invalid conversation node key"
                        ))
                    })?;
                if staged.registrations(CHAT_NODE).iter().any(|record| {
                    record.key.as_deref() == Some(key) && record.plugin_id.as_deref() != Some(owner)
                }) {
                    return Err(PluginError::Invalid(format!(
                        "slots[{index}]: conversation node key `{key}` belongs to another plugin"
                    )));
                }
            }
            staged
                .registry
                .register(declaration, generation)
                .map_err(|error| PluginError::Invalid(format!("slots[{index}]: {error}")))?;
            let slot = declaration["slot_id"].as_str().unwrap_or_default();
            if !desktop_slots().any(|(supported, _)| supported == slot) {
                return Err(PluginError::Invalid(format!(
                    "slots[{index}]: `{slot}` is unavailable in this desktop host"
                )));
            }
            if slot == VIEWER_TAB
                && !declaration["key"]
                    .as_str()
                    .is_some_and(dshr_plugin_sdk::FileViewContext::valid_file_type)
            {
                return Err(PluginError::Invalid(format!(
                    "slots[{index}]: viewer.tab.key must be a lowercase file extension"
                )));
            }
            if slot == COMMAND_SLOT {
                if declaration["id"]
                    .as_str()
                    .is_some_and(|id| BUILTIN_COMMAND_IDS.contains(&id))
                {
                    return Err(PluginError::Invalid(format!(
                        "slots[{index}]: built-in command id is reserved"
                    )));
                }
                if let Some(keys) = declaration["keys"].as_array() {
                    for key in keys.iter().filter_map(Value::as_str) {
                        if CommandShortcut::parse(key).is_none() {
                            return Err(PluginError::Invalid(format!(
                                "slots[{index}]: unsupported command key `{key}`"
                            )));
                        }
                    }
                }
            }
        }
        staged.owners.insert(owner.to_owned(), generation);
        Ok(Some(staged))
    }

    pub(super) fn declarations(&self, owner: &str) -> Vec<Value> {
        desktop_slots()
            .flat_map(|(slot, _)| self.registrations(slot))
            .filter(|record| record.plugin_id.as_deref() == Some(owner))
            .map(|record| Value::Object(record.fields.clone().into_iter().collect()))
            .collect()
    }

    pub(super) fn retract(&mut self, owner: &str) {
        if let Some(generation) = self.owners.remove(owner) {
            self.registry.invalidate_generation(generation);
        }
    }

    pub(super) fn registrations(&self, slot: &str) -> Vec<&RegistrationRecord> {
        self.registry.registrations_for(slot)
    }

    pub(super) fn active(&self, target: SlotTarget) -> Option<&RegistrationRecord> {
        self.registry
            .registration(target.handle)
            .filter(|record| record.generation == target.generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn declaration(owner: &str, id: &str, order: u32) -> Value {
        json!({
            "slot_id": "conversation.input.left", "plugin_id": owner,
            "id": id, "label": "Control", "view": id, "order": order
        })
    }

    #[test]
    fn desktop_commands_validate_reserved_ids_shortcuts_and_menu_keys() {
        let slots = PluginSlots::default();
        let owner = "demo.commands";
        let mut command = json!({
            "slot_id": COMMAND_SLOT, "plugin_id": owner, "id": "new", "label": "Refresh",
            "keys": ["Ctrl+Shift+G"], "run": "refresh"
        });
        assert!(slots.stage(owner, vec![command.clone()]).is_err());
        command["id"] = json!("demo.refresh");
        assert!(slots.stage(owner, vec![command.clone()]).unwrap().is_some());
        command["keys"] = json!(["Shift+G"]);
        assert!(slots.stage(owner, vec![command]).is_err());
        assert_eq!(
            CommandShortcut::parse("META+Shift+Esc"),
            CommandShortcut::parse("cmd-shift-escape")
        );
        assert!(CommandShortcut::parse("Ctrl+Ctrl+A").is_none());
        assert!(CommandShortcut::parse("Ctrl+F01").is_none());

        let item = |plugin_id, key| {
            json!({
                "slot_id": MENU_ITEM_SLOT, "plugin_id": plugin_id, "key": key, "id": "refresh-menu",
                "label": "Refresh", "run": "refresh"
            })
        };
        assert!(slots
            .stage(owner, vec![item(owner, "unknown.menu")])
            .is_err());
        let with_menu = slots
            .stage(owner, vec![item(owner, COMPOSER_ADD_MENU)])
            .unwrap()
            .unwrap();
        let target = SlotTarget::from(with_menu.registrations(MENU_ITEM_SLOT)[0]);
        let replaced = with_menu
            .stage("demo.next", vec![item("demo.next", COMPOSER_ADD_MENU)])
            .unwrap()
            .unwrap();
        assert!(replaced.active(target).is_none());
        assert_eq!(
            replaced.registrations(MENU_ITEM_SLOT)[0]
                .plugin_id
                .as_deref(),
            Some("demo.next")
        );
    }

    #[test]
    fn staged_replacement_preserves_old_slots_on_failure_and_invalidates_old_targets_on_success() {
        let left = "conversation.input.left";
        let slots = PluginSlots::default()
            .stage(
                "demo.left",
                vec![
                    declaration("demo.left", "second", 20),
                    declaration("demo.left", "first", 10),
                ],
            )
            .unwrap()
            .unwrap();
        let original = slots.registrations(left);
        assert_eq!(original[0].registration_id.as_deref(), Some("first"));
        let old = SlotTarget::from(original[0]);
        assert!(slots
            .stage("demo.right", vec![declaration("demo.left", "forged", 5)])
            .is_err());
        assert!(slots
            .stage("demo.right", vec![declaration("demo.right", "first", 5)])
            .is_err());
        let mut malformed = declaration("demo.left", "broken", 5);
        malformed.as_object_mut().unwrap().remove("label");
        assert!(slots
            .stage(
                "demo.left",
                vec![declaration("demo.left", "new", 1), malformed]
            )
            .is_err());
        assert!(slots.active(old).is_some());
        assert_eq!(slots.registrations(left).len(), 2);
        let staged = slots
            .stage("demo.left", vec![declaration("demo.left", "first", 10)])
            .unwrap()
            .unwrap();
        assert_eq!(staged.registrations(left).len(), 1);
        assert!(staged.active(old).is_none());
        assert!(slots
            .active(SlotTarget::from(staged.registrations(left)[0]))
            .is_none());
    }
}
