//! A deliberately closed JSON Schema subset. Unsupported keywords fail at registration;
//! the schema advertised to the model and the native validator cannot drift apart.
use serde::{de, Deserialize, Deserializer};
use serde_json::{Map, Value};
use std::{collections::HashSet, fmt};

#[derive(Clone)]
pub(super) struct Schema(Value);
impl Schema {
    pub fn compile(value: &Value) -> Option<Self> {
        fn check(v: &Value, depth: usize, nodes: &mut usize) -> bool {
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
                !matches!(k.as_str(), "type" | "description" | "enum")
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
                    let (Some(properties), Some(required)) =
                        (v["properties"].as_object(), v["required"].as_array())
                    else {
                        return false;
                    };
                    let names: HashSet<_> = required.iter().filter_map(Value::as_str).collect();
                    if v["additionalProperties"] != false
                        || names.len() != required.len()
                        || names.len() != properties.len()
                        || properties.len() > 64
                        || properties.iter().any(|(k, child)| {
                            k.is_empty()
                                || k.len() > 128
                                || !names.contains(k.as_str())
                                || !check(child, depth + 1, nodes)
                        })
                    {
                        return false;
                    }
                }
                Some("array") if !check(&v["items"], depth + 1, nodes) => return false,
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
        (value["type"] == "object" && check(value, 0, &mut 0)).then(|| Self(value.clone()))
    }
    pub fn parse(&self, arguments: &str) -> Option<Value> {
        if arguments.len() > 65536 {
            return None;
        }
        let value = serde_json::from_str::<UniqueValue>(arguments).ok()?.0;
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
        let fields = schema["properties"]
            .as_object()
            .expect("compiled object schema");
        return o.len() == fields.len()
            && fields
                .iter()
                .all(|(k, s)| o.get(k).is_some_and(|v| accepts(s, v)));
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

// serde_json::Value normally keeps the last duplicate field. Reject ambiguity before
// dispatch, including duplicate fields inside nested objects and arrays.
struct UniqueValue(Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = UniqueValue;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("unambiguous JSON")
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut map = Map::new();
                while let Some(k) = a.next_key::<String>()? {
                    if map.contains_key(&k) {
                        return Err(de::Error::custom("duplicate field"));
                    }
                    map.insert(k, a.next_value::<UniqueValue>()?.0);
                }
                Ok(UniqueValue(Value::Object(map)))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut values = vec![];
                while let Some(v) = a.next_element::<UniqueValue>()? {
                    values.push(v.0);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| UniqueValue(Value::Number(n)))
                    .ok_or_else(|| de::Error::custom("invalid number"))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
        }
        d.deserialize_any(Visitor)
    }
}
