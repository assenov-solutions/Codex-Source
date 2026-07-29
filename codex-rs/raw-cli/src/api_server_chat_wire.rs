use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ImageDetail;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;

use crate::api_server_vision::validate_and_count_image;
use crate::api_server_wire::ApiError;
use crate::api_server_wire::ParsedChatRequest;
use crate::api_server_wire::ParsedRequest;
use crate::api_server_wire::parse_tool_choice;
use crate::api_server_wire::validate_function;
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

#[derive(Deserialize)]
struct ChatDto {
    model: Option<String>,
    messages: Option<Vec<Value>>,
    tools: Option<Vec<Value>>,
    tool_choice: Option<Value>,
    parallel_tool_calls: Option<bool>,
    stream: Option<bool>,
    stream_options: Option<StreamOptionsDto>,
    service_tier: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    n: Option<u32>,
    store: Option<bool>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Default, Deserialize)]
struct StreamOptionsDto {
    include_usage: Option<bool>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

pub(crate) fn parse_chat_request(value: Value) -> WireResult<ParsedChatRequest> {
    let dto: ChatDto = decode(value)?;
    validate_text(&dto.model, "model")?;
    validate_text(&dto.service_tier, "service_tier")?;
    if dto.n.unwrap_or(1) != 1 {
        return Err(unsupported("n"));
    }
    if dto.store == Some(true) {
        return Err(unsupported("store"));
    }
    reject_extra(&dto.extra, "", FILTERED_MODEL_PARAMETERS)?;
    let options = dto.stream_options.unwrap_or_default();
    reject_extra(&options.extra, "stream_options.", &[])?;
    let messages = dto.messages.ok_or_else(|| missing("messages"))?;
    if messages.is_empty() {
        return Err(invalid("messages", "'messages' must not be empty."));
    }
    let input = parse_chat_messages(messages)?;
    validate_tool_call_sequence(&input, "messages")?;
    let tools = flatten_chat_tools(dto.tools)?;
    let tool_choice = parse_tool_choice(dto.tool_choice, tools.as_deref())?;
    Ok(ParsedChatRequest {
        request: ParsedRequest {
            model: dto.model,
            instructions: None,
            input,
            tools,
            tool_choice,
            parallel_tool_calls: dto.parallel_tool_calls.unwrap_or(true),
            reasoning_effort: dto.reasoning_effort,
            service_tier: dto.service_tier,
            stream: dto.stream.unwrap_or(false),
        },
        include_usage: options.include_usage.unwrap_or(false),
    })
}

fn flatten_chat_tools(tools: Option<Vec<Value>>) -> WireResult<Option<Vec<Value>>> {
    let Some(tools) = tools else { return Ok(None) };
    let mut flattened = Vec::with_capacity(tools.len());
    for tool in tools {
        let mut object = tool
            .as_object()
            .cloned()
            .ok_or_else(|| invalid("tools", "Each tool must be an object."))?;
        if object
            .remove("type")
            .and_then(|value| value.as_str().map(str::to_string))
            != Some("function".to_string())
        {
            return Err(invalid("tools", "Only function tools are supported."));
        }
        let function = object
            .remove("function")
            .and_then(|value| value.as_object().cloned())
            .ok_or_else(|| invalid("tools", "Function definition is required."))?;
        reject_extra(&object, "tools[].", &[])?;
        validate_function(&function)?;
        let mut flat = function;
        flat.insert("type".to_string(), Value::String("function".to_string()));
        flattened.push(Value::Object(flat));
    }
    Ok(Some(flattened))
}

fn parse_chat_messages(values: Vec<Value>) -> WireResult<Vec<ResponseItem>> {
    let mut input = Vec::new();
    let mut image_count = 0;
    for (index, value) in values.into_iter().enumerate() {
        let mut object = value
            .as_object()
            .cloned()
            .ok_or_else(|| invalid("messages", format!("Message {index} must be an object.")))?;
        let role = take_string(&mut object, "role", "messages")?;
        let content = object.remove("content").filter(|value| !value.is_null());
        let calls = object.remove("tool_calls").filter(|value| !value.is_null());
        let call_id = object
            .remove("tool_call_id")
            .filter(|value| !value.is_null());
        reject_extra(&object, &format!("messages[{index}]."), &[])?;
        match role.as_str() {
            "system" | "developer" | "user" => {
                if calls.is_some() || call_id.is_some() {
                    return Err(invalid_message(
                        index,
                        "tool fields are invalid for this role",
                    ));
                }
                // Responses Lite accepts caller instructions as developer messages. Mapping the
                // legacy Chat `system` role here preserves its instruction semantics.
                let response_role = if role == "system" { "developer" } else { &role };
                input.push(message(
                    response_role,
                    content_items(
                        content,
                        index,
                        if role == "user" {
                            ChatContentKind::UserInput
                        } else {
                            ChatContentKind::TextInput
                        },
                        &mut image_count,
                    )?,
                ));
            }
            "assistant" => {
                if call_id.is_some() {
                    return Err(invalid_message(
                        index,
                        "tool_call_id is invalid for assistant",
                    ));
                }
                let before = input.len();
                if content.is_some() {
                    input.push(message(
                        "assistant",
                        content_items(content, index, ChatContentKind::Output, &mut image_count)?,
                    ));
                }
                if let Some(calls) = calls {
                    input.extend(parse_chat_calls(calls, index)?);
                }
                if input.len() == before {
                    return Err(invalid_message(index, "content or tool_calls is required"));
                }
            }
            "tool" => {
                if calls.is_some() {
                    return Err(invalid_message(index, "tool_calls is invalid for tool"));
                }
                let call_id = value_string(call_id, index, "tool_call_id")?;
                let output = text_parts(content, index)?.join("\n");
                input.push(ResponseItem::FunctionCallOutput {
                    id: None,
                    call_id,
                    output: FunctionCallOutputPayload::from_text(output),
                    internal_chat_message_metadata_passthrough: None,
                });
            }
            _ => return Err(invalid_message(index, "unsupported role")),
        }
    }
    Ok(input)
}

fn parse_chat_calls(value: Value, message_index: usize) -> WireResult<Vec<ResponseItem>> {
    let Value::Array(calls) = value else {
        return Err(invalid_message(
            message_index,
            "tool_calls must be an array",
        ));
    };
    calls
        .into_iter()
        .map(|call| {
            let mut call = call
                .as_object()
                .cloned()
                .ok_or_else(|| invalid_message(message_index, "tool call must be an object"))?;
            let id = take_string(&mut call, "id", "messages")?;
            let kind = take_string(&mut call, "type", "messages")?;
            let mut function = call
                .remove("function")
                .and_then(|value| value.as_object().cloned())
                .ok_or_else(|| invalid_message(message_index, "function is required"))?;
            reject_extra(
                &call,
                &format!("messages[{message_index}].tool_calls[]."),
                &[],
            )?;
            let name = take_string(&mut function, "name", "messages")?;
            let arguments = take_string(&mut function, "arguments", "messages")?;
            reject_extra(
                &function,
                &format!("messages[{message_index}].tool_calls[].function."),
                &[],
            )?;
            if kind != "function" || id.trim().is_empty() || name.trim().is_empty() {
                return Err(invalid_message(message_index, "invalid function tool call"));
            }
            Ok(ResponseItem::FunctionCall {
                id: None,
                name,
                namespace: None,
                arguments,
                call_id: id,
                internal_chat_message_metadata_passthrough: None,
            })
        })
        .collect()
}

fn take_string(object: &mut Map<String, Value>, field: &str, param: &str) -> WireResult<String> {
    object
        .remove(field)
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| invalid(param, format!("'{field}' must be a string.")))
}

fn value_string(value: Option<Value>, index: usize, field: &str) -> WireResult<String> {
    value
        .and_then(|value| value.as_str().map(str::to_string))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid_message(index, &format!("{field} is required")))
}

#[derive(Clone, Copy)]
enum ChatContentKind {
    TextInput,
    UserInput,
    Output,
}

fn content_items(
    value: Option<Value>,
    index: usize,
    content_kind: ChatContentKind,
    image_count: &mut usize,
) -> WireResult<Vec<ContentItem>> {
    let value = value.ok_or_else(|| invalid_message(index, "content is required"))?;
    if let Value::String(text) = value {
        return Ok(vec![text_content_item(text, content_kind)]);
    }
    let Value::Array(parts) = value else {
        return Err(invalid_message(
            index,
            "content must be text or content parts",
        ));
    };
    parts
        .into_iter()
        .map(|part| {
            let object = part
                .as_object()
                .ok_or_else(|| invalid_message(index, "content part must be an object"))?;
            match object.get("type").and_then(Value::as_str) {
                Some("text" | "input_text" | "output_text") => {
                    let text = object
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .ok_or_else(|| invalid_message(index, "content part text is required"))?;
                    Ok(text_content_item(text, content_kind))
                }
                Some("image_url") if matches!(content_kind, ChatContentKind::UserInput) => {
                    let image = object
                        .get("image_url")
                        .and_then(Value::as_object)
                        .ok_or_else(|| {
                            invalid_message(index, "image_url content requires an object")
                        })?;
                    let image_url = image.get("url").and_then(Value::as_str).ok_or_else(|| {
                        invalid_message(index, "image_url content requires a URL")
                    })?;
                    validate_and_count_image(image_url, image_count, "messages")?;
                    if let Some((field, _)) = image.iter().find(|(field, value)| {
                        !matches!(field.as_str(), "url" | "detail") && !value.is_null()
                    }) {
                        return Err(unsupported(&format!(
                            "messages[{index}].content[].image_url.{field}"
                        )));
                    }
                    let detail = image
                        .get("detail")
                        .filter(|value| !value.is_null())
                        .map(|value| {
                            serde_json::from_value::<ImageDetail>(value.clone()).map_err(|_| {
                                invalid_message(
                                    index,
                                    "image detail must be auto, low, high, or original",
                                )
                            })
                        })
                        .transpose()?;
                    Ok(ContentItem::InputImage {
                        image_url: image_url.to_string(),
                        detail,
                    })
                }
                Some(_) | None => Err(invalid_message(index, "unsupported message content part")),
            }
        })
        .collect()
}

fn text_content_item(text: String, content_kind: ChatContentKind) -> ContentItem {
    match content_kind {
        ChatContentKind::TextInput | ChatContentKind::UserInput => ContentItem::InputText { text },
        ChatContentKind::Output => ContentItem::OutputText { text },
    }
}

fn text_parts(value: Option<Value>, index: usize) -> WireResult<Vec<String>> {
    let value = value.ok_or_else(|| invalid_message(index, "content is required"))?;
    if let Value::String(text) = value {
        return Ok(vec![text]);
    }
    let Value::Array(parts) = value else {
        return Err(invalid_message(index, "content must be text or text parts"));
    };
    parts
        .into_iter()
        .map(|part| {
            let object = part
                .as_object()
                .ok_or_else(|| invalid_message(index, "content part must be an object"))?;
            if !matches!(
                object.get("type").and_then(Value::as_str),
                Some("text" | "input_text" | "output_text")
            ) {
                return Err(invalid_message(
                    index,
                    "only text content parts are supported",
                ));
            }
            object
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| invalid_message(index, "content part text is required"))
        })
        .collect()
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

fn invalid_message(index: usize, detail: &str) -> ApiError {
    invalid("messages", format!("Invalid message {index}: {detail}."))
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
