use regex::Regex;
use serde_json::Value;

use crate::corpus::types::{NoiseClass, NoiseSet};

pub(super) fn detect_noise_classes(content: &str) -> NoiseSet {
    let mut classes = NoiseSet::new();
    if content.contains("\"signature\"") || content.contains("signature:") {
        classes.insert(NoiseClass::Signature);
    }
    if content.contains("thoughtSignature") {
        classes.insert(NoiseClass::ThoughtSignature);
    }
    if content.contains("\"type\":\"thinking\"") || content.contains("\"type\": \"thinking\"") {
        classes.insert(NoiseClass::InlineThinkingJson);
    }
    if content.contains("\"thinking\":\"\"") || content.contains("\"thinking\": \"\"") {
        classes.insert(NoiseClass::EmptyThinking);
    }
    if content.contains("frame_kind: internal_thought")
        || content.contains("\"frame_kind\":\"internal_thought\"")
        || content.contains("\"frame_kind\": \"internal_thought\"")
    {
        classes.insert(NoiseClass::InternalThoughtFrame);
    }
    if content.lines().any(|line| {
        line.len() > 4_000
            && (line.contains("\"tool_use\"")
                || line.contains("\"tool_result\"")
                || line.contains("\"input\""))
    }) {
        classes.insert(NoiseClass::MassiveToolJson);
    }
    classes
}

pub(super) fn repair_markdown_content(content: &str) -> (String, NoiseSet) {
    let signature_re =
        Regex::new(r#"\s*,?\s*"(signature|thoughtSignature)"\s*:\s*"[^"]*""#).unwrap();
    let mut out = Vec::new();
    let mut removed = NoiseSet::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(repaired_thinking) = repair_inline_thinking_json_line(line) {
            removed.insert(NoiseClass::InlineThinkingJson);
            if is_empty_thinking_signature_line(trimmed) {
                removed.insert(NoiseClass::EmptyThinking);
            }
            if line.contains("thoughtSignature") {
                removed.insert(NoiseClass::ThoughtSignature);
            }
            if line.contains("\"signature\"") {
                removed.insert(NoiseClass::Signature);
            }
            if let Some(repaired_thinking) = repaired_thinking {
                out.push(repaired_thinking);
            }
            continue;
        }

        let mut repaired = signature_re.replace_all(line, "").to_string();
        if repaired != line {
            if line.contains("thoughtSignature") {
                removed.insert(NoiseClass::ThoughtSignature);
            }
            if line.contains("\"signature\"") {
                removed.insert(NoiseClass::Signature);
            }
        }
        repaired = normalize_json_commas(repaired);
        if !is_empty_thinking_signature_line(repaired.trim()) {
            out.push(repaired);
        }
    }

    let mut repaired = out.join("\n");
    if content.ends_with('\n') {
        repaired.push('\n');
    }
    (repaired, removed)
}

fn repair_inline_thinking_json_line(line: &str) -> Option<Option<String>> {
    let (prefix, candidate) = split_markdown_json_prefix(line);
    let value = serde_json::from_str::<Value>(candidate).ok()?;
    let object = value.as_object()?;
    let is_thinking = object.get("type").and_then(Value::as_str) == Some("thinking")
        || object.get("thought").and_then(Value::as_bool) == Some(true);
    if !is_thinking {
        return None;
    }

    for key in ["thinking", "text", "content", "summary"] {
        if let Some(text) = object
            .get(key)
            .and_then(extract_text_from_repair_json_value)
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
        {
            return Some(Some(format!("{prefix}{text}")));
        }
    }

    Some(None)
}

fn split_markdown_json_prefix(line: &str) -> (&str, &str) {
    let first_json = line.find('{').unwrap_or(line.len());
    line.split_at(first_json)
}

fn extract_text_from_repair_json_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .filter_map(extract_text_from_repair_json_value)
                .collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(object) => ["text", "content", "summary", "thinking"]
            .iter()
            .filter_map(|key| object.get(*key))
            .find_map(extract_text_from_repair_json_value),
        _ => None,
    }
}

fn is_empty_thinking_signature_line(line: &str) -> bool {
    let candidate = line
        .trim_start_matches('>')
        .trim_start_matches('-')
        .trim_start_matches('*')
        .trim();
    candidate.contains("\"type\"")
        && candidate.contains("\"thinking\"")
        && (candidate.contains("\"thinking\":\"\"") || candidate.contains("\"thinking\": \"\""))
        && (candidate.contains("\"signature\"") || candidate.contains("thoughtSignature"))
}

fn normalize_json_commas(line: String) -> String {
    line.replace("{,", "{")
        .replace(",}", "}")
        .replace("[,", "[")
        .replace(",]", "]")
}
