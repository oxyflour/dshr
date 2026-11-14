//! 从 `schema/adc-v1/element.schema.json` 读取元素词汇表。
//!
//! 元素槽位、必填 props、prop 类型/枚举/范围/长度都从 schema 编译期嵌入的 JSON
//! 派生，不在 Rust 里重复维护一份字段表（避免双份真相）。本模块只解释 ADC-v1
//! 用到的 JSON Schema 子集：`$ref`、`enum`、`const`、`type`、`minLength`/
//! `maxLength`/`pattern`、`minimum`/`maximum`、`items`、对象 `properties`/
//! `required`/`additionalProperties`。`$defs/node` 视为不透明（元素节点由
//! `parse_tree` 自己递归校验）。

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::Value;

const ELEMENT_SCHEMA_JSON: &str = include_str!("../../../schema/adc-v1/element.schema.json");

/// prop 校验规则。
#[derive(Debug, Clone)]
pub(crate) enum PropRule {
    Bool,
    String {
        min_length: Option<usize>,
        max_length: Option<usize>,
        pattern: Option<String>,
    },
    Integer {
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    Number {
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    Enum(Vec<Value>),
    Action,
    Object(ObjectRule),
    Array(ArrayRule),
    Opaque,
}

/// 对象规则。
#[derive(Debug, Clone)]
pub(crate) struct ObjectRule {
    pub(crate) required: Vec<String>,
    pub(crate) props: BTreeMap<String, PropRule>,
    pub(crate) additional: bool,
}

/// 数组规则。
#[derive(Debug, Clone)]
pub(crate) struct ArrayRule {
    pub(crate) min_items: Option<usize>,
    pub(crate) max_items: Option<usize>,
    pub(crate) items: Option<Box<PropRule>>,
}

/// 一个元素的词汇表条目。
#[derive(Debug, Clone)]
pub(crate) struct ElementSpec {
    pub(crate) allows_children: bool,
    pub(crate) requires_children: bool,
    pub(crate) required_props: Vec<String>,
    pub(crate) props: BTreeMap<String, PropRule>,
}

/// ADC-v1 元素词汇表（从 element.schema.json 派生）。
#[derive(Debug)]
pub(crate) struct ElementVocabulary {
    elements: BTreeMap<String, ElementSpec>,
}

impl ElementVocabulary {
    /// 编译期嵌入的 element.schema.json。
    pub(crate) fn builtin() -> &'static ElementVocabulary {
        static VOCABULARY: OnceLock<ElementVocabulary> = OnceLock::new();
        VOCABULARY.get_or_init(|| match Self::from_json(ELEMENT_SCHEMA_JSON) {
            Ok(vocabulary) => vocabulary,
            Err(error) => panic!(
                "ADC-v1 element vocabulary schema/adc-v1/element.schema.json is invalid: {error}. \
                 The schema is compiled into dshr-ui-contributions; fix the schema file."
            ),
        })
    }

    /// 按元素类型名查条目。
    pub(crate) fn spec(&self, type_name: &str) -> Option<&ElementSpec> {
        self.elements.get(type_name)
    }

    fn from_json(json: &str) -> Result<Self, String> {
        let root: Value = serde_json::from_str(json).map_err(|error| error.to_string())?;
        let defs = root
            .get("$defs")
            .ok_or_else(|| "schema has no $defs".to_owned())?;
        let elements = defs
            .get("elements")
            .and_then(Value::as_object)
            .ok_or_else(|| "schema has no $defs.elements".to_owned())?;
        let mut vocabulary = BTreeMap::new();
        for (name, definition) in elements {
            let required_node_fields: Vec<String> = definition
                .get("required")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let allows_children = definition
                .get("properties")
                .and_then(|properties| properties.get("children"))
                .is_some();
            let requires_children = required_node_fields.iter().any(|field| field == "children");
            let (required_props, props) = match definition
                .get("properties")
                .and_then(|properties| properties.get("props"))
            {
                None => (Vec::new(), BTreeMap::new()),
                Some(props_schema) => {
                    let mut stack = Vec::new();
                    match resolve_rule(&root, props_schema, &mut stack, 0) {
                        PropRule::Object(object) => (object.required, object.props),
                        _ => (Vec::new(), BTreeMap::new()),
                    }
                }
            };
            vocabulary.insert(
                name.clone(),
                ElementSpec {
                    allows_children,
                    requires_children,
                    required_props,
                    props,
                },
            );
        }
        if vocabulary.is_empty() {
            return Err("schema declares no elements".to_owned());
        }
        Ok(ElementVocabulary {
            elements: vocabulary,
        })
    }
}

fn resolve_rule(root: &Value, schema: &Value, stack: &mut Vec<String>, depth: usize) -> PropRule {
    if depth > 12 {
        return PropRule::Opaque;
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if reference.ends_with("/action") {
            return PropRule::Action;
        }
        if reference.ends_with("/node") {
            // 元素节点树由 parse_tree 自己递归校验；此处不能继续展开（避免无限递归）。
            return PropRule::Opaque;
        }
        if stack.iter().any(|item| item == reference) {
            return PropRule::Opaque;
        }
        let Some(resolved) = root.pointer(reference.trim_start_matches('#')) else {
            return PropRule::Opaque;
        };
        stack.push(reference.to_owned());
        let rule = resolve_rule(root, resolved, stack, depth + 1);
        stack.pop();
        return rule;
    }
    if let Some(const_value) = schema.get("const") {
        return PropRule::Enum(vec![const_value.clone()]);
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return PropRule::Enum(values.clone());
    }
    if let Some(schema_type) = schema.get("type").and_then(Value::as_str) {
        return match schema_type {
            "boolean" => PropRule::Bool,
            "string" => PropRule::String {
                min_length: schema
                    .get("minLength")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize),
                max_length: schema
                    .get("maxLength")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize),
                pattern: schema
                    .get("pattern")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
            "integer" => PropRule::Integer {
                minimum: schema.get("minimum").and_then(Value::as_f64),
                maximum: schema.get("maximum").and_then(Value::as_f64),
            },
            "number" => PropRule::Number {
                minimum: schema.get("minimum").and_then(Value::as_f64),
                maximum: schema.get("maximum").and_then(Value::as_f64),
            },
            "array" => PropRule::Array(ArrayRule {
                min_items: schema
                    .get("minItems")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize),
                max_items: schema
                    .get("maxItems")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize),
                items: schema
                    .get("items")
                    .map(|items| Box::new(resolve_rule(root, items, stack, depth + 1))),
            }),
            "object" => PropRule::Object(build_object_rule(root, schema, stack, depth)),
            _ => PropRule::Opaque,
        };
    }
    if schema.get("properties").is_some() {
        return PropRule::Object(build_object_rule(root, schema, stack, depth));
    }
    PropRule::Opaque
}

fn build_object_rule(
    root: &Value,
    schema: &Value,
    stack: &mut Vec<String>,
    depth: usize,
) -> ObjectRule {
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut props = BTreeMap::new();
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, property_schema) in properties {
            props.insert(
                name.clone(),
                resolve_rule(root, property_schema, stack, depth + 1),
            );
        }
    }
    let additional = !matches!(schema.get("additionalProperties"), Some(Value::Bool(false)));
    ObjectRule {
        required,
        props,
        additional,
    }
}
