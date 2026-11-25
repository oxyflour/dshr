use std::sync::OnceLock;

use dshr_plugin_host::{
    desktop_slots, CHAT_NODE, COMMAND_SLOT, MENU_ITEM_SLOT, TOOL_CALL_VIEW, TOOL_RESULT_EXTRA,
    VIEWER_TAB,
};
use dshr_ui_contributions::SlotCatalog;
use serde_json::{json, Map, Value};

#[path = "inspection_sdk.rs"]
mod sdk;

const PANEL: &str = "workspace.panel";
const PANEL_LOCATION: &str =
    "Current conversation's trial result card and desktop plugin panel list";

pub(super) fn arguments_schema() -> Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schema/creator-mode/inspect-arguments.schema.json"
    )))
    .expect("creator inspection parameter schema")
}

fn element_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schema/adc-v1/element.schema.json"
        )))
        .expect("ADC element schema")
    })
}

fn element_definitions() -> &'static Map<String, Value> {
    element_schema()["$defs"]["elements"]
        .as_object()
        .expect("ADC element definitions")
}

pub(super) fn directory() -> Value {
    json!({
        "slot": desktop_slots().map(|(name, _)| name).chain([PANEL]).collect::<Vec<_>>(),
        "element": element_definitions().keys().collect::<Vec<_>>(),
        "sdk": sdk::names().collect::<Vec<_>>()
    })
}

pub(super) fn query(args: &Map<String, Value>) -> Result<Value, String> {
    let section = args.get("section").and_then(Value::as_str);
    let name = args.get("name").and_then(Value::as_str);
    let (Some(section), Some(name)) = (section, name.filter(|name| !name.trim().is_empty())) else {
        return Err("Pass {} for discovery, or both section and a nonempty name.".to_owned());
    };
    if args.len() != 2 {
        return Err("Exact inspection accepts only section and name.".to_owned());
    }
    let detail = match section {
        "slot" => slot_contract(name),
        "element" => element_contract(name),
        "sdk" => sdk::contract(name),
        _ => return Err("Unknown section. Choose slot, element, or sdk from queries.".to_owned()),
    };
    let contract = detail.ok_or_else(|| {
        format!("Unknown {section} name. Call creator_inspect with {{}} and use an exact name from queries.{section}.")
    })?;
    Ok(json!({"section": section, "name": name, "contract": contract}))
}

pub(super) fn supported_slots() -> Vec<Value> {
    let catalog = SlotCatalog::builtin();
    desktop_slots()
        .filter_map(|(name, location)| {
            let slot = catalog.get(name)?;
            Some(json!({
                "slot_id": name, "location": location, "scope": slot.scope.as_str(),
                "example": slot_example(name)
            }))
        })
        .collect()
}

fn slot_example(slot: &str) -> Value {
    let fields = match slot {
        CHAT_NODE => json!({"key": "hello.note", "view": "hello.chat-note"}),
        VIEWER_TAB => json!({"key": "txt", "view": "hello.file-preview"}),
        COMMAND_SLOT => {
            json!({"id": "hello.refresh", "label": "Refresh greeting", "keys": ["Ctrl+Shift+G"], "run": "refresh"})
        }
        MENU_ITEM_SLOT => {
            json!({"key": dshr_plugin_host::COMPOSER_ADD_MENU, "id": "hello.refresh-menu", "label": "Refresh greeting", "run": "refresh"})
        }
        TOOL_CALL_VIEW => json!({"key": "hello_status", "view": "hello.tool-card"}),
        TOOL_RESULT_EXTRA => {
            json!({"id": "hello.extra", "label": "Hello result", "view": "hello.tool-extra"})
        }
        _ => json!({"id": "hello.quick", "label": "Hello", "view": "hello.view"}),
    };
    let mut example = fields;
    example["slot_id"] = json!(slot);
    example["plugin_id"] = json!("example.hello-panel");
    example
}

pub(super) fn renders_at_trial(slot: &str) -> bool {
    !matches!(slot, VIEWER_TAB | CHAT_NODE | COMMAND_SLOT | MENU_ITEM_SLOT)
        && !dshr_plugin_host::is_tool_view_slot(slot)
}

fn slot_contract(name: &str) -> Option<Value> {
    let catalog = SlotCatalog::builtin();
    let slot = catalog.get(name)?;
    let (registration, location, conflict) = if name == PANEL {
        (
            json!({
                "sdk": "PanelDecl",
                "transport": "ReadyDecls::new(tools, vec![panel])",
                "required": ["slot", "id", "label", "view"],
                "optional": ["location", "order"],
                "example": {"slot": PANEL, "id": "hello.panel", "label": "Hello", "view": "hello.view"},
                "location_values": ["left", "right", "bottom", "center"],
                "field_notes": "Required strings must be nonblank. order must be finite. The legacy location field does not select the mount point."
            }),
            PANEL_LOCATION,
            None,
        )
    } else {
        let (_, location) = desktop_slots().find(|(key, _)| *key == name)?;
        let required: Vec<_> = ["slot_id", "plugin_id"]
            .into_iter()
            .chain(slot.registration.required.iter().map(String::as_str))
            .collect();
        let mut fields: Map<String, Value> = required
            .iter()
            .copied()
            .chain(slot.registration.optional.iter().map(String::as_str))
            .filter_map(|field| Some((field.to_owned(), field_schema(catalog.field_type(field)?))))
            .collect();
        fields.insert("slot_id".to_owned(), json!({"const": name}));
        let mut registration = json!({
            "sdk": "ReadyDecls",
            "transport": "ReadyDecls::new(tools, panels).with_slots(vec![json!(example)])",
            "required": required,
            "optional": slot.registration.optional,
            "fields": fields,
            "example": slot_example(name),
            "field_notes": "Use the manifest plugin ID. The host rejects unknown fields."
        });
        if let Some(key_domain) = &slot.key_domain {
            registration["key_domain"] =
                json!({"mode": key_domain.mode.as_str(), "description": key_domain.doc});
        }
        let conflict = catalog
            .kind_spec(slot.kind)
            .and_then(|kind| kind.conflict.as_deref());
        (registration, location, conflict)
    };
    let mut contract = json!({
        "location": location, "scope": slot.scope.as_str(), "kind": slot.kind.as_str(),
        "replacement_risk": slot.replacement_risk.as_str(), "conflict": conflict,
        "registration": registration,
        "validation": if renders_at_trial(name) { "rendered" } else { "registration" },
        "lifecycle": "Installed contributions follow the slot scope. A trial is visible only in its creator session."
    });
    describe_slot(name, &mut contract);
    Some(contract)
}

fn field_schema(field: &dshr_ui_contributions::FieldTypeSpec) -> Value {
    use dshr_ui_contributions::FieldTypeSpec::*;
    let mut schema = match field {
        String {
            min_length,
            max_length,
            pattern,
        } => json!({
            "type": "string", "minLength": min_length, "maxLength": max_length, "pattern": pattern
        }),
        Number { minimum, maximum } => {
            json!({"type": "number", "minimum": minimum, "maximum": maximum})
        }
        Enum { values } => json!({"type": "string", "enum": values}),
        StringArray {
            max_items,
            item_pattern,
        } => json!({
            "type": "array", "maxItems": max_items, "items": {"type": "string", "pattern": item_pattern}
        }),
    };
    schema
        .as_object_mut()
        .unwrap()
        .retain(|_, value| !value.is_null());
    if let Some(items) = schema.get_mut("items").and_then(Value::as_object_mut) {
        items.retain(|_, value| !value.is_null());
    }
    schema
}

fn describe_slot(name: &str, contract: &mut Value) {
    let (method, context, verify) = match name {
        VIEWER_TAB => (
            sdk::RENDER_FILE, Some("FileViewContext"),
            "In the same session, deliver a file with the registered extension and select Preview. Check the right sidebar tab."
        ),
        CHAT_NODE => (
            sdk::RENDER_NODE, Some("ChatNodeContext"),
            "Call the plugin's own tool successfully with meta.adc_node. Check the independent conversation row and its fallback after reopening."
        ),
        TOOL_CALL_VIEW | TOOL_RESULT_EXTRA => (
            sdk::RENDER_TOOL, Some("ToolViewContext"),
            "Call the plugin's own tool successfully. Check the expanded tool card for the registered body or result extra."
        ),
        COMMAND_SLOT | MENU_ITEM_SLOT => (
            sdk::HANDLE_ACTION, None,
            "With the session idle, select the registered command or named menu item and verify the handler's observable effect."
        ),
        PANEL => (
            sdk::RENDER, None,
            "Try the plugin with expect_panel:true. Check panels and the mounted trial result card, then exercise the view's actions."
        ),
        _ => (
            sdk::RENDER, None,
            "Try the plugin and check slots[].validation. Open the listed location in the same session and exercise its view."
        ),
    };
    contract["verify"] = json!(verify);
    match name {
        COMMAND_SLOT | MENU_ITEM_SLOT => {
            contract["callback"] = json!({
                "sdk": "ActionHandler", "signature": method,
                "first_argument": "the declaration's run string",
                "action": "command", "payload": {"id": "the declaration's id"},
                "availability": "Current visible registration in an idle, writable session."
            });
            if name == MENU_ITEM_SLOT {
                contract["registration"]["fields"]["key"]["enum"] =
                    json!(dshr_plugin_host::DESKTOP_MENU_KEYS);
            } else {
                contract["registration"]["reserved_ids"] =
                    json!(dshr_plugin_host::BUILTIN_COMMAND_IDS);
                contract["registration"]["shortcut_rules"] = json!("Use one chord with Ctrl or Cmd (Meta and Super are aliases). Add Alt or Shift if needed. Built-in shortcuts win. Conflicting plugin shortcuts remain selectable from the menu.");
            }
        }
        _ => {
            contract["callback"] = json!({
                "sdk": "ViewHandler", "signature": method, "context": context,
                "returns": "The ADC-v1 element tree as serde_json::Value"
            });
            if let Some(context) = context {
                contract["callback"]["context_contract"] =
                    sdk::contract(context).expect("known SDK context");
                contract["actions"] = json!(["copy", "openExternal"]);
            } else {
                contract["actions"] = json!(["call", "command", "copy", "openExternal"]);
                contract["action_handler"] = json!({"section": "sdk", "name": "ActionHandler"});
            }
        }
    }
    match name {
        VIEWER_TAB | CHAT_NODE => {
            let (context, field) = if name == VIEWER_TAB {
                ("file_context", "file_type")
            } else {
                ("node_context", "key")
            };
            contract["registration"]["fields"]["key"] =
                sdk::render_context(context)["properties"][field].clone();
        }
        TOOL_CALL_VIEW => {
            contract["registration"]["key_rule"] =
                json!("Use the name of a tool declared by this plugin in the same ReadyDecls.");
        }
        _ => {}
    }
    if name == CHAT_NODE {
        contract["conflict"] = json!(
            "Each key belongs to one plugin per host. The owner can replace its own registration."
        );
        contract["publish"] = json!({
            "sdk": "ToolResult::with_meta", "meta": {"adc_node": {"key": "hello.note", "fallback": "Report ready", "data": {"count": 3}}},
            "limits": "fallback must be nonblank and at most 2048 UTF-8 bytes. The encoded adc_node object is limited to 16 KiB."
        });
    }
}

fn element_contract(name: &str) -> Option<Value> {
    let element = element_definitions().get(name)?;
    let mut references = Map::new();
    collect_references(element, &mut references);
    let event = match name {
        "Button" => {
            json!({"prop": "onPress", "trigger": "Button activation", "call_args": "Uses the declared args."})
        }
        "Input" | "Textarea" => change_event(
            "The user submits the editor",
            "string",
            &["call", "command"],
        ),
        "Select" => change_event("The user selects an option", "string", &["call"]),
        "Checkbox" | "Switch" => json!({
            "prop": "onChange", "trigger": "The user activates the control",
            "call_args": "Uses the declared args. Set args.value to the desired boolean when rendering, or toggle the plugin state in the handler. Return the new checked value in the refreshed view."
        }),
        "Slider" => change_event(
            "The user changes the slider with drag, step buttons, or arrow keys",
            "number",
            &["call"],
        ),
        "Tabs" => change_event(
            "The user selects a tab. The value is the tab key.",
            "string",
            &["call", "command", "copy", "openExternal"],
        ),
        "List" => {
            json!({"prop": "onSelect", "dispatch": false, "note": "The native desktop list currently renders items without dispatching onSelect."})
        }
        "Table" => {
            json!({"prop": "onRowClick", "dispatch": false, "note": "The native desktop table currently renders rows without dispatching onRowClick."})
        }
        "Tree" => {
            json!({"prop": "onToggle", "dispatch": false, "note": "The native desktop tree currently renders the declared expanded state without dispatching onToggle."})
        }
        _ => Value::Null,
    };
    Some(json!({
        "schema": element,
        "references": references,
        "recursive_node": "#/$defs/node is any ADC-v1 element. Query the child element by its type.",
        "event": event,
        "availability": "Slot and session rules determine which actions dispatch. Preview is read-only."
    }))
}

fn change_event(trigger: &str, value_type: &str, actions: &[&str]) -> Value {
    json!({"prop": "onChange", "trigger": trigger, "actions": actions, "call_args": {"value": {"type": value_type}}, "note": "For call, the host inserts the new value into args.value and preserves other declared arguments."})
}

fn collect_references(value: &Value, references: &mut Map<String, Value>) {
    match value {
        Value::Object(fields) => {
            if let Some(reference) = fields.get("$ref").and_then(Value::as_str) {
                if reference != "#/$defs/node" && !references.contains_key(reference) {
                    let pointer = reference
                        .strip_prefix('#')
                        .expect("local ADC schema reference");
                    let definition = element_schema()
                        .pointer(pointer)
                        .expect("defined ADC schema reference");
                    references.insert(reference.to_owned(), definition.clone());
                    collect_references(definition, references);
                }
            }
            for field in fields.values() {
                collect_references(field, references);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_references(item, references);
            }
        }
        _ => {}
    }
}
