//! Small embedded UI dictionary. User content is never translated or sent to a service.
use std::sync::LazyLock;
static EN: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../assets/en.json")).expect("validated UI translations")
});
pub fn language() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s| s.get_item("streamcap.language").ok().flatten())
            .filter(|s| s == "en")
            .unwrap_or_else(|| "zh_CN".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        "zh_CN".into()
    }
}
pub fn t(value: &'static str) -> &'static str {
    if language() == "en" {
        EN.get(value)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(value)
    } else {
        value
    }
}
pub fn message(value: String) -> String {
    if language() == "en" {
        EN.get(&value)
            .and_then(serde_json::Value::as_str)
            .map(String::from)
            .unwrap_or(value)
    } else {
        value
    }
}
pub fn set_language(value: &str) {
    #[cfg(target_arch = "wasm32")]
    if let Some(window) = web_sys::window() {
        if !matches!(value, "en" | "zh_CN") {
            return;
        }
        if let Ok(Some(storage)) = window.local_storage() {
            if storage.set_item("streamcap.language", value).is_ok() {
                let _ = window.location().reload();
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = value;
}
fn parts(template: &str) -> (Vec<&str>, Vec<&str>) {
    let mut literals = Vec::new();
    let mut slots = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        literals.push(&rest[..start]);
        slots.push(&rest[start + 1..start + end]);
        rest = &rest[start + end + 1..];
    }
    literals.push(rest);
    (literals, slots)
}
fn interpolate(original: &str, translated: &str, rendered: String) -> String {
    let (literals, slots) = parts(original);
    let mut remaining = rendered.as_str();
    let mut values = Vec::new();
    if let Some(tail) = remaining.strip_prefix(literals[0]) {
        remaining = tail;
    } else {
        return rendered;
    }
    for (index, _) in slots.iter().enumerate() {
        let next = literals[index + 1];
        let boundary = if next.is_empty() {
            remaining.len()
        } else {
            match remaining.find(next) {
                Some(i) => i,
                None => return rendered,
            }
        };
        values.push(&remaining[..boundary]);
        remaining = &remaining[boundary + next.len()..];
    }
    if !remaining.is_empty() {
        return rendered;
    }
    let (labels, fields) = parts(translated);
    let mut result = labels[0].to_owned();
    for (index, field) in fields.iter().enumerate() {
        let name = field.split(':').next().unwrap_or("");
        let source = if name.is_empty() {
            Some(index)
        } else {
            slots
                .iter()
                .position(|slot| slot.split(':').next() == Some(name))
        };
        let Some(value) = source.and_then(|position| values.get(position)) else {
            return rendered;
        };
        result.push_str(value);
        result.push_str(labels[index + 1]);
    }
    result
}
pub fn formatted(template: &'static str, value: String) -> String {
    let translated = t(template);
    if translated == template {
        value
    } else {
        interpolate(template, translated, value)
    }
}
#[macro_export]
macro_rules! tr_format { ($template:literal $(, $($rest:tt)*)?) => { $crate::app::i18n::formatted($template, format!($template $(, $($rest)*)?)) }; }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_verification_translations_are_complete() {
        for key in [
            "待验证",
            "检测异常",
            "重新验证",
            "检测失败：{}",
            "请在快手窗口完成验证",
            "已打开快手验证窗口",
            "等待手动验证",
            "等待检测恢复",
            "有需要处理的任务，请查看下方提示。",
        ] {
            assert!(EN
                .get(key)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty() && value != key));
        }
    }
    #[test]
    fn placeholders_preserve_user_values() {
        assert_eq!(
            interpolate(
                "已添加 {count} 个直播间",
                "Added {count} rooms",
                "已添加 12 个直播间".into()
            ),
            "Added 12 rooms"
        );
        assert_eq!(
            interpolate("每 {} 秒", "Every {} seconds", "每 4500 秒".into()),
            "Every 4500 seconds"
        );
        assert_eq!(
            interpolate(
                "{name}: {count} 个任务",
                "{count} tasks: {name}",
                "主播: 3 个任务".into()
            ),
            "3 tasks: 主播"
        );
    }
}
