use super::*;

/// Select 的 `onChange` 只对 `call` 注入 `args.value`；其他动作保持原样（无法携带值）。
pub(super) fn action_with_value(action: &Action, value: &str) -> Action {
    action_with_json(action, serde_json::json!(value))
}

/// 把任意 JSON 值（字符串/数字）注入 `call.args.value`；非 `call` 原样返回。
pub(super) fn action_with_json(action: &Action, value: serde_json::Value) -> Action {
    match action {
        Action::Call { method, args } => {
            let mut args = args.clone();
            if let Some(object) = args.as_object_mut() {
                object.insert("value".to_owned(), value);
            }
            Action::Call {
                method: method.clone(),
                args,
            }
        }
        other => other.clone(),
    }
}

/// Input/Textarea 的编辑请求：走同一个 action handler 的保留方法 `__adc_edit`。
/// 宿主据此打开真实 `TextInput`，提交时再把值注入 `on_change` 并派发。
pub(super) fn edit_action(path: &str, value: &str, multiline: bool, on_change: Action) -> Action {
    let mut args = serde_json::json!({
        "path": path,
        "value": value,
        "multiline": multiline,
    });
    args["on_change"] = action_to_json(&on_change);
    Action::Call {
        method: "__adc_edit".to_owned(),
        args,
    }
}

pub(super) fn action_to_json(action: &Action) -> serde_json::Value {
    match action {
        Action::Call { method, args } => {
            serde_json::json!({ "call": method, "args": args })
        }
        Action::Command { id } => serde_json::json!({ "command": id }),
        Action::OpenExternal { url } => serde_json::json!({ "openExternal": url }),
        Action::Copy { text } => serde_json::json!({ "copy": text }),
        _ => serde_json::Value::Null,
    }
}
