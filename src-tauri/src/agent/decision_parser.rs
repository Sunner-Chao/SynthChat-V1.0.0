use serde_json::{json, Value};

use super::tool_registry::resolve_mcp_tool;
use crate::{
    error::{AppError, AppResult},
    models::ToolDefinition,
};

pub(super) const PROVIDER_TOOL_CALL_META_KEY: &str = "__agentProviderToolCall";
const DECISION_ORIGIN_META_KEY: &str = "__agentDecisionOrigin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ToolCallOrigin {
    ProviderNative,
    PlannerJson,
    HermesMarkup,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct AgentToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments: Value,
    pub origin: ToolCallOrigin,
    pub provider_meta: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum AgentDecision {
    Final {
        content: String,
        raw: Value,
    },
    Tool {
        calls: Vec<AgentToolCall>,
        raw: Value,
    },
}

pub(super) fn provider_tool_call_id(payload: &Value) -> Option<String> {
    payload
        .get(PROVIDER_TOOL_CALL_META_KEY)
        .and_then(|metadata| {
            metadata
                .get("id")
                .or_else(|| metadata.get("call_id"))
                .or_else(|| metadata.get("tool_call_id"))
                .or_else(|| metadata.get("toolCallId"))
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn summarize_planner_step(decision: &Value) -> String {
    let action = decision
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("final");
    match action {
        "tool" => {
            let tools = planned_tool_requests_from_decision(decision)
                .into_iter()
                .map(|(tool, _)| tool)
                .collect::<Vec<_>>();
            if tools.is_empty() {
                "tool:<missing tool>".into()
            } else if tools.len() == 1 {
                format!("tool:{}", tools[0])
            } else {
                format!("tools:{}", tools.join(","))
            }
        }
        other => other.to_string(),
    }
}

pub(super) fn planner_decision_error(decision: &Value) -> Option<String> {
    match decision.get("action").and_then(Value::as_str) {
        Some("tool") if planned_tool_requests_from_decision(decision).is_empty() => {
            Some("tool action missing tool name".into())
        }
        Some(_) => None,
        None => Some("planner output missing action; treated as final".into()),
    }
}

pub(super) fn parse_agent_decision(raw: &str) -> Value {
    let trimmed = raw.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return normalize_agent_decision(value);
    }
    let repaired = repair_tool_arguments_json(trimmed);
    if repaired != trimmed {
        if let Ok(value) = serde_json::from_str::<Value>(&repaired) {
            return normalize_agent_decision(value);
        }
    }
    if let Some(value) = parse_hermes_tool_markup(trimmed) {
        return normalize_agent_decision(value);
    }
    if let Some(json_text) = first_json_object(trimmed) {
        if let Ok(value) = serde_json::from_str::<Value>(&json_text) {
            return normalize_agent_decision(value);
        }
        let repaired = repair_tool_arguments_json(&json_text);
        if let Ok(value) = serde_json::from_str::<Value>(&repaired) {
            return normalize_agent_decision(value);
        }
    }
    if let Some(json_text) = loose_json_object_slice(trimmed) {
        let repaired = repair_tool_arguments_json(json_text);
        if let Ok(value) = serde_json::from_str::<Value>(&repaired) {
            return normalize_agent_decision(value);
        }
    }
    json!({"action": "final", "content": trimmed})
}

pub(super) fn canonical_decision_from_value(decision: &Value) -> AgentDecision {
    let action = decision
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("final");
    if action == "tool" {
        return AgentDecision::Tool {
            calls: canonical_tool_calls_from_decision(decision),
            raw: decision.clone(),
        };
    }
    let content = decision
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_else(|| decision.as_str().unwrap_or(""))
        .to_string();
    AgentDecision::Final {
        content,
        raw: decision.clone(),
    }
}

pub(super) fn canonical_tool_calls_from_decision(decision: &Value) -> Vec<AgentToolCall> {
    planned_tool_requests_from_decision(decision)
        .into_iter()
        .map(|(name, arguments)| {
            let provider_meta = arguments.get(PROVIDER_TOOL_CALL_META_KEY).cloned();
            let id = provider_tool_call_id(&arguments);
            let origin = tool_call_origin(decision, provider_meta.as_ref());
            AgentToolCall {
                id,
                name,
                arguments,
                origin,
                provider_meta,
            }
        })
        .collect()
}

pub(super) fn validated_tool_requests_from_decision(
    decision: &Value,
    available_tools: &[ToolDefinition],
) -> AppResult<Vec<(String, Value)>> {
    canonical_tool_calls_from_decision(decision)
        .into_iter()
        .map(|call| {
            validate_agent_tool_call(&call, available_tools)?;
            Ok((call.name, call.arguments))
        })
        .collect()
}

pub(super) fn validate_agent_tool_call(
    call: &AgentToolCall,
    available_tools: &[ToolDefinition],
) -> AppResult<()> {
    let definition = resolve_mcp_tool(available_tools, &call.name)
        .ok_or_else(|| AppError::BadRequest(format!("tool is not available: {}", call.name)))?;
    validate_tool_call_payload(&definition, &call.arguments)
}

pub(super) fn validate_tool_call_payload(
    definition: &ToolDefinition,
    payload: &Value,
) -> AppResult<()> {
    let payload = strip_provider_tool_call_metadata_for_validation(payload);
    validate_json_schema_subset(&definition.input_schema, &payload, "payload").map_err(|error| {
        AppError::BadRequest(format!(
            "tool {} payload schema validation failed: {error}",
            definition.name
        ))
    })
}

fn normalize_agent_decision(value: Value) -> Value {
    let Some(object) = value.as_object() else {
        return value;
    };
    let action = object
        .get("action")
        .or_else(|| object.get("type"))
        .or_else(|| object.get("decision"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let use_tool = object
        .get("useTool")
        .or_else(|| object.get("use_tool"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let has_explicit_tool_key = object.get("tool").is_some()
        || object.get("toolName").is_some()
        || object.get("tool_name").is_some();
    let has_tool_array = object.get("toolCalls").and_then(Value::as_array).is_some()
        || object.get("tool_calls").and_then(Value::as_array).is_some()
        || object.get("tools").and_then(Value::as_array).is_some()
        || object.get("calls").and_then(Value::as_array).is_some()
        || object
            .get("function_calls")
            .and_then(Value::as_array)
            .is_some();
    let action_requests_tool = matches!(
        action.as_str(),
        "tool" | "use_tool" | "call_tool" | "tools" | "tool_call"
    );

    if action_requests_tool || use_tool || has_explicit_tool_key || has_tool_array {
        let requests = planned_tool_requests(&value);
        if let Some((tool, payload)) = requests.first() {
            let tool_requests = requests
                .iter()
                .map(|(tool, payload)| json!({"tool": tool, "payload": payload}))
                .collect::<Vec<_>>();
            return json!({
                "action": "tool",
                "tool": tool,
                "payload": payload,
                "toolRequests": tool_requests,
                "rawDecision": value,
            });
        }
    }

    if matches!(action.as_str(), "answer" | "respond" | "finish" | "done") {
        let content = object
            .get("content")
            .or_else(|| object.get("answer"))
            .or_else(|| object.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return json!({
            "action": "final",
            "content": content,
            "rawDecision": value,
        });
    }

    value
}

fn first_planned_tool_request(value: &Value) -> Option<(String, Value)> {
    planned_tool_requests(value).into_iter().next()
}

fn planned_tool_requests(value: &Value) -> Vec<(String, Value)> {
    if let Some(calls) = value
        .get("toolCalls")
        .or_else(|| value.get("tool_calls"))
        .or_else(|| value.get("tools"))
        .or_else(|| value.get("calls"))
        .or_else(|| value.get("function_calls"))
        .and_then(Value::as_array)
    {
        return calls
            .iter()
            .flat_map(planned_tool_requests)
            .collect::<Vec<_>>();
    }

    let function_value = value
        .get("function")
        .filter(|function| function.is_object());
    let Some(tool) = value
        .get("tool")
        .or_else(|| value.get("toolName"))
        .or_else(|| value.get("tool_name"))
        .or_else(|| value.get("name"))
        .or_else(|| {
            value
                .get("function")
                .filter(|function| function.is_string())
        })
        .or_else(|| function_value.and_then(|function| function.get("name")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|tool| !tool.is_empty())
        .map(str::to_string)
    else {
        return vec![];
    };
    let mut payload = value
        .get("payload")
        .or_else(|| value.get("arguments"))
        .or_else(|| value.get("args"))
        .or_else(|| value.get("input"))
        .or_else(|| value.get("parameters"))
        .or_else(|| function_value.and_then(|function| function.get("arguments")))
        .map(normalize_tool_payload_value)
        .unwrap_or_else(|| json!({}));
    if let Some(metadata) = provider_tool_call_metadata(value) {
        if let Some(object) = payload.as_object_mut() {
            object.insert(PROVIDER_TOOL_CALL_META_KEY.into(), metadata);
        }
    }
    vec![(tool, payload)]
}

fn provider_tool_call_metadata(value: &Value) -> Option<Value> {
    let mut metadata = serde_json::Map::new();
    for key in ["id", "call_id", "response_item_id", "extra_content"] {
        if let Some(item) = value.get(key).filter(|item| !item.is_null()) {
            metadata.insert(key.into(), item.clone());
        }
    }
    (!metadata.is_empty()).then(|| Value::Object(metadata))
}

fn tool_call_origin(decision: &Value, provider_meta: Option<&Value>) -> ToolCallOrigin {
    if provider_meta.is_some() {
        return ToolCallOrigin::ProviderNative;
    }
    if decision_origin(decision).as_deref() == Some("hermes_markup") {
        return ToolCallOrigin::HermesMarkup;
    }
    ToolCallOrigin::PlannerJson
}

fn decision_origin(decision: &Value) -> Option<String> {
    decision
        .get(DECISION_ORIGIN_META_KEY)
        .or_else(|| {
            decision
                .get("rawDecision")
                .and_then(|raw| raw.get(DECISION_ORIGIN_META_KEY))
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn strip_provider_tool_call_metadata_for_validation(payload: &Value) -> Value {
    let mut payload = payload.clone();
    if let Some(object) = payload.as_object_mut() {
        object.remove(PROVIDER_TOOL_CALL_META_KEY);
    }
    payload
}

fn validate_json_schema_subset(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(schema_object) = schema.as_object() else {
        return Ok(());
    };
    if schema_object.is_empty() {
        return Ok(());
    }
    if let Some(enum_values) = schema_object.get("enum").and_then(Value::as_array) {
        if !enum_values.iter().any(|item| item == value) {
            return Err(format!(
                "{path} must be one of {}",
                enum_values_preview(enum_values)
            ));
        }
    }
    if let Some(expected) = schema_object.get("type") {
        if !schema_type_accepts_value(expected, value) {
            return Err(format!(
                "{path} expected {}, got {}",
                schema_type_label(expected),
                json_value_type(value)
            ));
        }
    }
    if schema_type_declares(schema, "object") || schema_object.contains_key("properties") {
        validate_object_schema_subset(schema, value, path)?;
    }
    if schema_type_declares(schema, "array") || schema_object.contains_key("items") {
        validate_array_schema_subset(schema, value, path)?;
    }
    Ok(())
}

fn validate_object_schema_subset(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(schema_object) = schema.as_object() else {
        return Ok(());
    };
    let Some(value_object) = value.as_object() else {
        if schema_object.contains_key("required") || schema_object.contains_key("properties") {
            return Err(format!(
                "{path} expected object, got {}",
                json_value_type(value)
            ));
        }
        return Ok(());
    };
    if let Some(required) = schema_object.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            if !value_object.contains_key(key) {
                return Err(format!("{path}.{key} is required"));
            }
        }
    }
    if let Some(properties) = schema_object.get("properties").and_then(Value::as_object) {
        for (key, property_schema) in properties {
            if let Some(child) = value_object.get(key) {
                validate_json_schema_subset(property_schema, child, &format!("{path}.{key}"))?;
            }
        }
        if schema_object
            .get("additionalProperties")
            .and_then(Value::as_bool)
            == Some(false)
        {
            for key in value_object.keys() {
                if !properties.contains_key(key) {
                    return Err(format!("{path}.{key} is not allowed by schema"));
                }
            }
        }
    }
    Ok(())
}

fn validate_array_schema_subset(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(schema_object) = schema.as_object() else {
        return Ok(());
    };
    let Some(items) = schema_object.get("items") else {
        return Ok(());
    };
    let Some(values) = value.as_array() else {
        return Err(format!(
            "{path} expected array, got {}",
            json_value_type(value)
        ));
    };
    for (index, item) in values.iter().enumerate() {
        validate_json_schema_subset(items, item, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

fn schema_type_declares(schema: &Value, expected: &str) -> bool {
    schema
        .get("type")
        .map(|value| schema_type_value_contains(value, expected))
        .unwrap_or(false)
}

fn schema_type_accepts_value(schema_type: &Value, value: &Value) -> bool {
    match value {
        Value::Null => schema_type_value_contains(schema_type, "null"),
        Value::Bool(_) => schema_type_value_contains(schema_type, "boolean"),
        Value::Number(number) => {
            schema_type_value_contains(schema_type, "number")
                || (schema_type_value_contains(schema_type, "integer")
                    && (number.is_i64() || number.is_u64()))
        }
        Value::String(_) => schema_type_value_contains(schema_type, "string"),
        Value::Array(_) => schema_type_value_contains(schema_type, "array"),
        Value::Object(_) => schema_type_value_contains(schema_type, "object"),
    }
}

fn schema_type_value_contains(schema_type: &Value, expected: &str) -> bool {
    match schema_type {
        Value::String(value) => value == expected,
        Value::Array(values) => values.iter().any(|value| value.as_str() == Some(expected)),
        _ => false,
    }
}

fn schema_type_label(schema_type: &Value) -> String {
    match schema_type {
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("|"),
        _ => "unspecified".into(),
    }
}

fn json_value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn enum_values_preview(values: &[Value]) -> String {
    values
        .iter()
        .take(6)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn planned_tool_requests_from_decision(decision: &Value) -> Vec<(String, Value)> {
    if let Some(requests) = decision.get("toolRequests").and_then(Value::as_array) {
        let parsed = requests
            .iter()
            .flat_map(planned_tool_requests)
            .collect::<Vec<_>>();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    first_planned_tool_request(decision).into_iter().collect()
}

fn normalize_tool_payload_value(value: &Value) -> Value {
    if let Some(raw) = value.as_str() {
        return parse_tool_arguments_json(raw, "?");
    }
    value.clone()
}

fn parse_hermes_tool_markup(text: &str) -> Option<Value> {
    if let Some(value) = parse_function_equals_tool_markup(text) {
        return Some(value);
    }
    let tool_name = extract_xml_tag(text, "tool_name")
        .or_else(|| extract_xml_tag(text, "tool"))
        .map(|value| decode_basic_xml_entities(value.trim()))
        .filter(|value| !value.trim().is_empty())?;
    let parameters = extract_xml_tag(text, "parameters")
        .or_else(|| extract_xml_tag(text, "arguments"))
        .map(|value| decode_basic_xml_entities(value.trim()))
        .unwrap_or_else(|| "{}".into());
    let payload = if parameters.trim().is_empty() {
        json!({})
    } else {
        parse_tool_arguments_json(&parameters, &tool_name)
    };
    Some(json!({
        "action": "tool",
        "tool": tool_name,
        "payload": payload,
        "__agentDecisionOrigin": "hermes_markup",
    }))
}

fn parse_function_equals_tool_markup(text: &str) -> Option<Value> {
    let open_idx = find_ascii_case_insensitive(text, "<function=", 0)?;
    let name_start = open_idx + "<function=".len();
    let name_end = text[name_start..]
        .find('>')
        .map(|offset| name_start + offset)?;
    let tool_name = decode_basic_xml_entities(
        text[name_start..name_end]
            .trim()
            .trim_matches('"')
            .trim_matches('\''),
    );
    if tool_name.trim().is_empty() {
        return None;
    }
    let body_start = name_end + 1;
    let body_end = find_ascii_case_insensitive(text, "</function>", body_start)
        .or_else(|| find_ascii_case_insensitive(text, "</tool_call>", body_start))
        .unwrap_or(text.len());
    let body = &text[body_start..body_end];
    let mut object = serde_json::Map::new();
    for (name, value) in extract_parameter_tags(body) {
        object.insert(name, Value::String(value));
    }
    Some(json!({
        "action": "tool",
        "tool": tool_name,
        "payload": Value::Object(object),
        "__agentDecisionOrigin": "hermes_markup",
    }))
}

fn extract_parameter_tags(text: &str) -> Vec<(String, String)> {
    let mut params = Vec::new();
    let mut cursor = 0usize;
    while let Some(open_idx) = find_ascii_case_insensitive(text, "<parameter=", cursor) {
        let name_start = open_idx + "<parameter=".len();
        let Some(name_end) = text[name_start..]
            .find('>')
            .map(|offset| name_start + offset)
        else {
            break;
        };
        let name = decode_basic_xml_entities(
            text[name_start..name_end]
                .trim()
                .trim_matches('"')
                .trim_matches('\''),
        );
        let value_start = name_end + 1;
        let Some(value_end) = find_ascii_case_insensitive(text, "</parameter>", value_start) else {
            break;
        };
        if !name.trim().is_empty() {
            params.push((
                name,
                decode_basic_xml_entities(text[value_start..value_end].trim()),
            ));
        }
        cursor = value_end + "</parameter>".len();
    }
    params
}

pub(super) fn parse_tool_arguments_json(raw: &str, tool_name: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "None" {
        return json!({});
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value;
    }
    let repaired = repair_tool_arguments_json(trimmed);
    if let Ok(value) = serde_json::from_str::<Value>(&repaired) {
        return value;
    }
    let escaped = escape_invalid_chars_in_json_strings(&repaired);
    if let Ok(value) = serde_json::from_str::<Value>(&escaped) {
        return value;
    }
    let _ = tool_name;
    json!({})
}

fn repair_tool_arguments_json(raw: &str) -> String {
    let mut fixed = raw.replace(": None", ": null").replace(":None", ":null");
    fixed = quote_unquoted_json_keys(&fixed);
    fixed = strip_trailing_json_commas(&fixed);
    let (open_curly, open_bracket) = json_container_balance(&fixed);
    if open_curly > 0 {
        fixed.push_str(&"}".repeat(open_curly as usize));
    }
    if open_bracket > 0 {
        fixed.push_str(&"]".repeat(open_bracket as usize));
    }
    for _ in 0..50 {
        let (curly, bracket) = json_container_balance(&fixed);
        if curly >= 0 && bracket >= 0 {
            break;
        }
        if curly < 0 && fixed.ends_with('}') {
            fixed.pop();
            continue;
        }
        if bracket < 0 && fixed.ends_with(']') {
            fixed.pop();
            continue;
        }
        break;
    }
    fixed
}

fn quote_unquoted_json_keys(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let chars = raw.char_indices().collect::<Vec<_>>();
    let mut idx = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut expecting_key = true;

    while idx < chars.len() {
        let (byte_idx, ch) = chars[idx];
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            idx += 1;
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                expecting_key = false;
                out.push(ch);
                idx += 1;
            }
            '{' | ',' => {
                expecting_key = true;
                out.push(ch);
                idx += 1;
            }
            ':' => {
                expecting_key = false;
                out.push(ch);
                idx += 1;
            }
            '}' | ']' => {
                expecting_key = false;
                out.push(ch);
                idx += 1;
            }
            ch if expecting_key && ch.is_ascii_alphabetic() || expecting_key && ch == '_' => {
                let start_idx = idx;
                let start_byte = byte_idx;
                idx += 1;
                while idx < chars.len() {
                    let (_, next) = chars[idx];
                    if next.is_ascii_alphanumeric() || next == '_' || next == '-' {
                        idx += 1;
                    } else {
                        break;
                    }
                }
                let end_byte = chars.get(idx).map(|(byte, _)| *byte).unwrap_or(raw.len());
                let mut lookahead = idx;
                while lookahead < chars.len() && chars[lookahead].1.is_whitespace() {
                    lookahead += 1;
                }
                let mut colon_lookahead = lookahead;
                let stray_closing_quote = if colon_lookahead < chars.len()
                    && chars[colon_lookahead].1 == '"'
                {
                    colon_lookahead += 1;
                    while colon_lookahead < chars.len() && chars[colon_lookahead].1.is_whitespace()
                    {
                        colon_lookahead += 1;
                    }
                    true
                } else {
                    false
                };
                if colon_lookahead < chars.len() && chars[colon_lookahead].1 == ':' {
                    out.push('"');
                    out.push_str(&raw[start_byte..end_byte]);
                    out.push('"');
                    if stray_closing_quote {
                        idx = lookahead + 1;
                    }
                    expecting_key = false;
                } else {
                    out.push_str(&raw[start_byte..end_byte]);
                    expecting_key = false;
                }
                if start_idx == idx {
                    idx += 1;
                }
            }
            _ => {
                out.push(ch);
                idx += 1;
            }
        }
    }

    out
}

fn strip_trailing_json_commas(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }
        if ch == ',' {
            let mut lookahead = chars.clone();
            while matches!(lookahead.peek(), Some(next) if next.is_whitespace()) {
                lookahead.next();
            }
            if matches!(lookahead.peek(), Some('}' | ']')) {
                continue;
            }
        }
        out.push(ch);
    }
    out
}

fn json_container_balance(raw: &str) -> (isize, isize) {
    let mut curly = 0isize;
    let mut bracket = 0isize;
    let mut in_string = false;
    let mut escaped = false;
    for ch in raw.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => curly += 1,
            '}' => curly -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            _ => {}
        }
    }
    (curly, bracket)
}

fn escape_invalid_chars_in_json_strings(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in raw.chars() {
        if in_string {
            if escaped {
                out.push(ch);
                escaped = false;
                continue;
            }
            if ch == '\\' {
                out.push(ch);
                escaped = true;
                continue;
            }
            if ch == '"' {
                in_string = false;
                out.push(ch);
                continue;
            }
            if ch.is_control() {
                out.push_str(&format!("\\u{:04x}", ch as u32));
                continue;
            }
            out.push(ch);
        } else {
            if ch == '"' {
                in_string = true;
            }
            out.push(ch);
        }
    }
    out
}

fn extract_xml_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let lower = text.to_ascii_lowercase();
    let open = format!("<{}>", tag.to_ascii_lowercase());
    let close = format!("</{}>", tag.to_ascii_lowercase());
    let start = lower.find(&open)? + open.len();
    let end = lower[start..].find(&close)? + start;
    Some(&text[start..end])
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str, start: usize) -> Option<usize> {
    if start >= haystack.len() {
        return None;
    }
    let haystack_lower = haystack[start..].to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    haystack_lower.find(&needle_lower).map(|idx| start + idx)
}

fn decode_basic_xml_entities(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn first_json_object(text: &str) -> Option<String> {
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if start.is_none() {
            if ch == '{' {
                start = Some(index);
                depth = 1;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && in_string {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let begin = start?;
                    return Some(text[begin..=index].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn loose_json_object_slice(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then_some(&text[start..=end])
}
