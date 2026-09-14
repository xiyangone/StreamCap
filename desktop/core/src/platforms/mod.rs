//! Native resolvers. No external interpreter or fallback gateway.
pub mod catalog;
pub mod custom;
pub mod douyin;
pub mod douyin_sign;
pub mod extended;
pub mod http;
pub mod kuaishou;
pub mod kuaishou_login;
pub mod migu_wasm;
pub mod signatures;

use serde_json::Value;

pub fn quality_index(value: Option<&str>) -> Result<usize, String> {
    match value.unwrap_or("OD").to_ascii_uppercase().as_str() {
        "OD" | "0" => Ok(0),
        "UHD" | "1" => Ok(1),
        "HD" | "2" => Ok(2),
        "SD" | "3" => Ok(3),
        "LD" | "4" => Ok(4),
        _ => Err("清晰度无效".into()),
    }
}
pub fn quality_name(index: usize) -> &'static str {
    ["OD", "UHD", "HD", "SD", "LD"][index.min(4)]
}

/// Parse only the first JSON value after a known marker, preserving escapes and nested objects.
pub fn json_after(text: &str, marker: &str) -> Option<Value> {
    let start = text.find(marker)? + marker.len();
    serde_json::Deserializer::from_str(text[start..].trim_start())
        .into_iter::<Value>()
        .next()?
        .ok()
}
pub fn find_object<'a>(
    value: &'a Value,
    predicate: &impl Fn(&Value) -> bool,
    depth: usize,
) -> Option<&'a Value> {
    if depth > 40 {
        return None;
    }
    if predicate(value) {
        return Some(value);
    }
    match value {
        Value::Object(map) => map
            .values()
            .find_map(|v| find_object(v, predicate, depth + 1)),
        Value::Array(values) => values
            .iter()
            .find_map(|v| find_object(v, predicate, depth + 1)),
        _ => None,
    }
}
pub fn text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|n| n.to_string()))
        .unwrap_or_default()
}
