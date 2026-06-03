#![allow(unused_imports)]
use super::*;

pub(crate) fn render_json_inline(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

pub(crate) fn render_claude_thinking_block(
    block: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    ["thinking", "text", "content", "summary"]
        .iter()
        .filter_map(|key| block.get(*key))
        .find_map(extract_text_from_json_value)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

pub(crate) fn render_claude_tool_block(
    block: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let mut parts = Vec::new();
    if let Some(name) = block.get("name").and_then(|value| value.as_str()) {
        parts.push(format!("name: {name}"));
    }
    if let Some(id) = block
        .get("id")
        .or_else(|| block.get("tool_use_id"))
        .and_then(|value| value.as_str())
    {
        parts.push(format!("id: {id}"));
    }
    if let Some(input) = block.get("input") {
        parts.push(format!("input: {}", render_json_inline(input)));
    }
    if let Some(content) = block.get("content").and_then(extract_text_from_json_value) {
        parts.push(format!("content: {content}"));
    }

    if parts.is_empty() {
        render_json_inline(&serde_json::Value::Object(block.clone()))
    } else {
        parts.join("\n")
    }
}

pub(crate) fn extract_text_from_json_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .filter_map(extract_text_from_json_value)
                .collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        serde_json::Value::Object(map) => ["text", "content", "message", "body", "value"]
            .iter()
            .filter_map(|key| map.get(*key))
            .find_map(extract_text_from_json_value),
        _ => None,
    }
}

pub(crate) fn extract_message_text(message: &Option<serde_json::Value>) -> String {
    match message {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|item| {
                if let Some(obj) = item.as_object()
                    && obj.get("type").and_then(|t| t.as_str()) == Some("text")
                {
                    return obj.get("text").and_then(|t| t.as_str()).map(String::from);
                }
                None
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(serde_json::Value::Object(obj)) => {
            if let Some(content) = obj.get("content") {
                match content {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Array(arr) => arr
                        .iter()
                        .filter_map(|item| {
                            if let Some(block) = item.as_object()
                                && block.get("type").and_then(|t| t.as_str()) == Some("text")
                            {
                                return block
                                    .get("text")
                                    .and_then(|t| t.as_str())
                                    .map(String::from);
                            }
                            None
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                }
            } else if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                text.to_string()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}
