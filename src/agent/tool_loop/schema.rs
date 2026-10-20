//! A deliberately closed JSON Schema subset. Unsupported keywords fail at registration;
//! normalized arguments must still satisfy the schema advertised to the model.
use serde_json::{Map, Value};
use std::collections::HashSet;

#[derive(Clone)]
pub(super) struct Schema(Value);
impl Schema {
    pub fn compile(value: &Value) -> Option<Self> {Self::compile_mode(value,false)}
    pub fn compile_mcp(value: &Value) -> Option<Self> {Self::compile_mode(value,true)}
    fn compile_mode(value: &Value, external:bool) -> Option<Self> {
        fn check(v: &Value, depth: usize, nodes: &mut usize, external:bool) -> bool {
            *nodes += 1;
            if depth > 8 || *nodes > 256 {
                return false;
            }
            let Some(o) = v.as_object() else { return false };
            let fields: &[&str] = match v["type"].as_str() {
                Some("object") => &["properties", "required", "additionalProperties"],
                Some("array") => &["items", "minItems", "maxItems"],
                Some("string") => &["minLength", "maxLength"],
                Some("integer" | "number") => &["minimum", "maximum"],
                Some("boolean" | "null") => &[],
                _ => return false,
            };
            if o.keys().any(|k| {
                !matches!(k.as_str(), "type" | "description" | "enum") && !(external&&matches!(k.as_str(),"title"|"default"|"$schema"))
                    && !fields.contains(&k.as_str())
            }) || o
                .get("description")
                .is_some_and(|d| d.as_str().is_none_or(|s| s.len() > 16000))
            {
                return false;
            }
            for (low, high) in [("minLength", "maxLength"), ("minItems", "maxItems")] {
                if [low, high]
                    .iter()
                    .any(|k| o.get(*k).is_some_and(|n| n.as_u64().is_none()))
                    || o.get(low).and_then(Value::as_u64).unwrap_or(0)
                        > o.get(high).and_then(Value::as_u64).unwrap_or(u64::MAX)
                {
                    return false;
                }
            }
            if ["minimum", "maximum"]
                .iter()
                .any(|k| o.get(*k).is_some_and(|n| n.as_f64().is_none()))
                || o.get("minimum")
                    .and_then(Value::as_f64)
                    .unwrap_or(f64::NEG_INFINITY)
                    > o.get("maximum")
                        .and_then(Value::as_f64)
                        .unwrap_or(f64::INFINITY)
            {
                return false;
            }
            if v["type"] == "integer"
                && (["minimum", "maximum"]
                    .iter()
                    .any(|k| o.get(*k).is_some_and(|n| n.as_i64().is_none()))
                    || v["minimum"].as_i64().unwrap_or(i64::MIN)
                        > v["maximum"].as_i64().unwrap_or(i64::MAX))
            {
                return false;
            }
            match v["type"].as_str() {
                Some("object") => {
                    if o.get("properties").is_some_and(|v|!v.is_object())
                        || o.get("required").is_some_and(|v|!v.is_array()) {
                        return false;
                    }
                    let empty_properties=Map::new();let empty_required=vec![];
                    let (Some(properties), Some(required)) =
                        (v["properties"].as_object().or(external.then_some(&empty_properties)), v["required"].as_array().or(external.then_some(&empty_required)))
                    else {
                        return false;
                    };
                    let names: HashSet<_> = required.iter().filter_map(Value::as_str).collect();
                    if (!external && v["additionalProperties"] != false)
                        || (external && v.get("additionalProperties").is_some_and(|v|!v.is_boolean()))
                        || names.len() != required.len()
                        || (!external && names.len() != properties.len())
                        || names.iter().any(|k|!properties.contains_key(*k))
                        || properties.len() > 64
                        || properties.iter().any(|(k, child)| {
                            k.is_empty()
                                || k.len() > 128
                                || (!external && !names.contains(k.as_str()))
                                || !check(child, depth + 1, nodes, external)
                        })
                    {
                        return false;
                    }
                }
                Some("array") if !check(&v["items"], depth + 1, nodes, external) => return false,
                _ => {}
            }
            if let Some(enumeration) = o.get("enum") {
                let Some(values) = enumeration.as_array() else {
                    return false;
                };
                if values.is_empty()
                    || values.len() > 64
                    || values.iter().any(|x| !matches_type(v, x))
                {
                    return false;
                }
            }
            true
        }
        (value["type"] == "object" && check(value, 0, &mut 0, external)).then(|| Self(value.clone()))
    }
    pub fn parse(&self, arguments: &str, normalize: impl FnOnce(&mut Value)) -> Option<Value> {
        if arguments.len() > 65536 {
            return None;
        }
        // Reject malformed JSON and duplicate fields before any normalization.
        let mut value = super::super::strict_json::parse(arguments).ok()?;
        normalize(&mut value);
        accepts(&self.0, &value).then_some(value)
    }
}
fn matches_type(schema: &Value, value: &Value) -> bool {
    match schema["type"].as_str() {
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        // Integer arguments use the signed 64-bit domain, without float coercion.
        Some("integer") => value.as_i64().is_some(),
        Some("number") => value.is_number(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        _ => false,
    }
}
fn accepts(schema: &Value, value: &Value) -> bool {
    if !matches_type(schema, value)
        || schema
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|v| !v.contains(value))
    {
        return false;
    }
    if let Some(o) = value.as_object() {
        let empty=Map::new();
        let fields=schema["properties"].as_object().unwrap_or(&empty);
        return schema["required"].as_array().is_none_or(|a|a.iter().all(|k|k.as_str().is_some_and(|k|o.contains_key(k))))
            && o.iter().all(|(k,v)|match fields.get(k) {Some(s)=>accepts(s,v),None=>schema["additionalProperties"]!=false});
    }
    let length = value
        .as_str()
        .map(|s| (s.chars().count() as u64, "minLength", "maxLength"))
        .or_else(|| {
            value
                .as_array()
                .map(|a| (a.len() as u64, "minItems", "maxItems"))
        });
    if let Some((n, low, high)) = length {
        if n < schema[low].as_u64().unwrap_or(0) || n > schema[high].as_u64().unwrap_or(u64::MAX) {
            return false;
        }
    }
    if schema["type"] == "integer" {
        let n = value.as_i64().expect("checked integer");
        if n < schema["minimum"].as_i64().unwrap_or(i64::MIN)
            || n > schema["maximum"].as_i64().unwrap_or(i64::MAX)
        {
            return false;
        }
    } else if let Some(n) = value.as_f64() {
        if n < schema["minimum"].as_f64().unwrap_or(f64::NEG_INFINITY)
            || n > schema["maximum"].as_f64().unwrap_or(f64::INFINITY)
        {
            return false;
        }
    }
    value
        .as_array()
        .is_none_or(|a| a.iter().all(|v| accepts(&schema["items"], v)))
}
