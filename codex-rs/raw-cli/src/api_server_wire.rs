use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use http::StatusCode;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::api_server_vision::validate_and_count_image;
use crate::api_server_wire_validation::validate_tool_call_sequence;

type WireResult<T> = Result<T, ApiError>;

const FILTERED_MODEL_PARAMETERS: &[&str] = &[
    "frequency_penalty",
    "logprobs",
    "max_completion_tokens",
    "max_output_tokens",
    "max_tokens",
    "presence_penalty",
    "response_format",
    "seed",
    "stop",
    "temperature",
    "top_logprobs",
    "top_p",
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParsedRequest {
    pub(crate) model: Option<String>,
    pub(crate) instructions: Option<String>,
    pub(crate) input: Vec<ResponseItem>,
    pub(crate) tools: Option<Vec<Value>>,
    pub(crate) tool_choice: String,
    pub(crate) parallel_tool_calls: bool,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) service_tier: Option<String>,
    pub(crate) stream: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParsedChatRequest {
    pub(crate) request: ParsedRequest,
    pub(crate) include_usage: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
    pub(crate) error_type: &'static str,
    pub(crate) param: Option<String>,
    pub(crate) code: Option<String>,
}

impl ApiError {
    pub(crate) fn invalid(message: impl Into<String>, param: Option<&str>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            error_type: "invalid_request_error",
            param: param.map(str::to_string),
            code: None,
        }
    }

    pub(crate) fn body(&self) -> Value {
        json!({"error": {
            "message": self.message,
            "type": self.error_type,
            "param": self.param,
            "code": self.code,
        }})
    }
}

#[derive(Deserialize)]
struct ResponsesDto {
    model: Option<String>,
    input: Option<Value>,
    instructions: Option<String>,
    tools: Option<Vec<Value>>,
    tool_choice: Option<Value>,
    parallel_tool_calls: Option<bool>,
    stream: Option<bool>,
    service_tier: Option<String>,
    reasoning: Option<ReasoningDto>,
    previous_response_id: Option<Value>,
    store: Option<bool>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Default, Deserialize)]
struct ReasoningDto {
    effort: Option<ReasoningEffort>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

pub(crate) fn parse_responses_request(value: Value) -> WireResult<ParsedRequest> {
    let dto: ResponsesDto = decode(value)?;
    validate_text(&dto.model, "model")?;
    validate_text(&dto.service_tier, "service_tier")?;
    if dto.previous_response_id.is_some() {
        return Err(unsupported("previous_response_id"));
    }
    if dto.store == Some(true) {
        return Err(unsupported("store"));
    }
    reject_extra(&dto.extra, "", FILTERED_MODEL_PARAMETERS)?;
    let reasoning = dto.reasoning.unwrap_or_default();
    reject_extra(&reasoning.extra, "reasoning.", &[])?;
    let input = dto.input.ok_or_else(|| missing("input"))?;
    let input = parse_responses_input(input)?;
    validate_tool_call_sequence(&input, "input")?;
    let tools = validate_responses_tools(dto.tools)?;
    let tool_choice = parse_tool_choice(dto.tool_choice, tools.as_deref())?;
    Ok(ParsedRequest {
        model: dto.model,
        instructions: dto.instructions,
        input,
        tools,
        tool_choice,
        parallel_tool_calls: dto.parallel_tool_calls.unwrap_or(true),
        reasoning_effort: reasoning.effort,
        service_tier: dto.service_tier,
        stream: dto.stream.unwrap_or(false),
    })
}

fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> WireResult<T> {
    serde_json::from_value(value)
        .map_err(|error| ApiError::invalid(format!("Invalid request body: {error}"), None))
}

fn invalid(param: &str, message: impl Into<String>) -> ApiError {
    ApiError::invalid(message, Some(param))
}

fn missing(param: &str) -> ApiError {
    invalid(param, format!("Missing required parameter: '{param}'."))
}

fn unsupported(param: &str) -> ApiError {
    ApiError {
        code: Some("unsupported_parameter".to_string()),
        ..invalid(param, format!("Unsupported parameter: '{param}'."))
    }
}

fn validate_text(value: &Option<String>, param: &str) -> WireResult<()> {
    if value.as_ref().is_some_and(|value| value.trim().is_empty()) {
        return Err(invalid(param, format!("'{param}' must not be empty.")));
    }
    Ok(())
}

fn reject_extra(
    extra: &Map<String, Value>,
    prefix: &str,
    filtered_parameters: &[&str],
) -> WireResult<()> {
    if let Some((key, _)) = extra
        .iter()
        .find(|(key, value)| !value.is_null() && !filtered_parameters.contains(&key.as_str()))
    {
        return Err(unsupported(&format!("{prefix}{key}")));
    }
    Ok(())
}

pub(crate) fn parse_tool_choice(
    choice: Option<Value>,
    tools: Option<&[Value]>,
) -> WireResult<String> {
    let default = if tools.is_some_and(|tools| !tools.is_empty()) {
        "auto"
    } else {
        "none"
    };
    let choice = match choice {
        None | Some(Value::Null) => default.to_string(),
        Some(Value::String(value)) if matches!(value.as_str(), "none" | "auto" | "required") => {
            value
        }
        Some(_) => {
            return Err(invalid(
                "tool_choice",
                "'tool_choice' must be 'none', 'auto', or 'required'.",
            ));
        }
    };
    if choice == "required" && tools.is_none_or(<[Value]>::is_empty) {
        return Err(invalid(
            "tool_choice",
            "'required' needs at least one tool.",
        ));
    }
    Ok(choice)
}

fn parse_responses_input(value: Value) -> WireResult<Vec<ResponseItem>> {
    if let Value::String(text) = value {
        return Ok(vec![message("user", vec![ContentItem::InputText { text }])]);
    }
    let Value::Array(items) = value else {
        return Err(invalid("input", "'input' must be a string or array."));
    };
    if items.is_empty() {
        return Err(invalid("input", "'input' must not be empty."));
    }
    let mut image_count = 0;
    items
        .into_iter()
        .enumerate()
        .map(|(index, value)| parse_response_item(index, value, &mut image_count))
        .collect()
}

fn parse_response_item(
    index: usize,
    mut value: Value,
    image_count: &mut usize,
) -> WireResult<ResponseItem> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid("input", format!("Input item {index} must be an object.")))?;
    if !object.contains_key("type") && object.contains_key("role") {
        object.insert("type".to_string(), Value::String("message".to_string()));
    }
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(
        kind,
        "message" | "function_call" | "function_call_output" | "reasoning"
    ) {
        return Err(invalid(
            "input",
            format!("Unsupported input item type '{kind}'."),
        ));
    }
    for field in [
        "internal_chat_message_metadata_passthrough",
        "phase",
        "namespace",
    ] {
        if object.get(field).is_some_and(|value| !value.is_null()) {
            return Err(unsupported(&format!("input[{index}].{field}")));
        }
    }
    if kind == "message" {
        let role = object
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !matches!(role.as_str(), "user" | "assistant" | "system" | "developer") {
            return Err(invalid(
                "input",
                format!("Unsupported message role '{role}'."),
            ));
        }
        normalize_message_content(object, &role, image_count)?;
    }
    let item: ResponseItem = serde_json::from_value(value)
        .map_err(|error| invalid("input", format!("Invalid input item {index}: {error}")))?;
    if item == ResponseItem::Other {
        return Err(invalid("input", format!("Unsupported input item {index}.")));
    }
    Ok(item)
}

fn normalize_message_content(
    object: &mut Map<String, Value>,
    role: &str,
    image_count: &mut usize,
) -> WireResult<()> {
    let content = object
        .get_mut("content")
        .ok_or_else(|| missing("input.content"))?;
    if let Value::String(text) = content {
        *content = json!([{"type": "input_text", "text": text}]);
        return Ok(());
    }
    let Value::Array(parts) = content else {
        return Err(invalid(
            "input",
            "Message content must be text or an array.",
        ));
    };
    for part in parts {
        let Value::Object(part) = part else {
            return Err(invalid("input", "Message content parts must be objects."));
        };
        let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
        if kind == "text" {
            part.insert("type".to_string(), Value::String("input_text".to_string()));
        } else if kind == "input_image" {
            if role != "user" {
                return Err(invalid(
                    "input",
                    "Image content is supported only in user messages.",
                ));
            }
            let image_url = part
                .get("image_url")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("input", "Image content requires an 'image_url' string."))?;
            validate_and_count_image(image_url, image_count, "input")?;
            if let Some((field, _)) = part.iter().find(|(field, value)| {
                !matches!(field.as_str(), "type" | "image_url" | "detail") && !value.is_null()
            }) {
                return Err(unsupported(&format!("input.content.{field}")));
            }
        } else if !matches!(kind, "input_text" | "output_text") {
            return Err(invalid(
                "input",
                format!("Unsupported message content type '{kind}'."),
            ));
        }
    }
    Ok(())
}

fn validate_responses_tools(tools: Option<Vec<Value>>) -> WireResult<Option<Vec<Value>>> {
    let Some(tools) = tools else { return Ok(None) };
    for tool in &tools {
        let object = tool
            .as_object()
            .ok_or_else(|| invalid("tools", "Each tool must be an object."))?;
        if object.get("type").and_then(Value::as_str) != Some("function") {
            return Err(invalid("tools", "Only function tools are supported."));
        }
        validate_function(object)?;
    }
    Ok(Some(tools))
}

pub(crate) fn validate_function(function: &Map<String, Value>) -> WireResult<()> {
    if let Some((field, _)) = function.iter().find(|(field, value)| {
        !matches!(
            field.as_str(),
            "type" | "name" | "description" | "parameters" | "strict"
        ) && !value.is_null()
    }) {
        return Err(invalid(
            "tools",
            format!("Unsupported function tool field '{field}'."),
        ));
    }
    if function
        .get("name")
        .and_then(Value::as_str)
        .is_none_or(|name| name.trim().is_empty())
    {
        return Err(invalid(
            "tools",
            "Function name must be a non-empty string.",
        ));
    }
    if function
        .get("parameters")
        .is_some_and(|value| !value.is_null() && !value.is_object())
    {
        return Err(invalid("tools", "Function parameters must be an object."));
    }
    if function
        .get("description")
        .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return Err(invalid("tools", "Function description must be a string."));
    }
    if function
        .get("strict")
        .is_some_and(|value| !value.is_null() && !value.is_boolean())
    {
        return Err(invalid("tools", "Function strict must be a boolean."));
    }
    Ok(())
}

fn message(role: &str, content: Vec<ContentItem>) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content,
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[cfg(test)]
#[path = "api_server_wire_tests.rs"]
mod tests;
