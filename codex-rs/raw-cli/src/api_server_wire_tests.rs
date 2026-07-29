use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ImageDetail;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::TokenUsage;
use http::StatusCode;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::ApiError;
use super::ParsedChatRequest;
use super::ParsedRequest;
use super::parse_responses_request;
use crate::api_server_chat_wire::parse_chat_request;
use crate::api_server_response::chat_response;
use crate::api_server_response::models_response;
use crate::api_server_response::responses_response;
use crate::api_server_vision::MAX_IMAGE_INPUTS;

fn message(role: &str, content: ContentItem) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![content],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn usage() -> TokenUsage {
    TokenUsage {
        input_tokens: 7,
        cached_input_tokens: 2,
        cache_write_input_tokens: 3,
        output_tokens: 11,
        reasoning_output_tokens: 5,
        total_tokens: 18,
    }
}

#[test]
fn parses_responses_string_without_hidden_context() {
    let parsed = parse_responses_request(json!({
        "input": "hello",
        "instructions": "Be concise",
        "reasoning": {"effort": "low"},
        "service_tier": "default",
        "stream": true,
    }))
    .expect("parse Responses request");

    assert_eq!(
        parsed,
        ParsedRequest {
            model: None,
            instructions: Some("Be concise".to_string()),
            input: vec![message(
                "user",
                ContentItem::InputText {
                    text: "hello".to_string(),
                },
            )],
            tools: None,
            tool_choice: "none".to_string(),
            parallel_tool_calls: true,
            reasoning_effort: Some(ReasoningEffort::Low),
            service_tier: Some("default".to_string()),
            stream: true,
        }
    );
}

#[test]
fn filters_unsupported_responses_model_parameters() {
    let expected = parse_responses_request(json!({"input": "hello"}))
        .expect("parse baseline Responses request");
    let parsed = parse_responses_request(json!({
        "input": "hello",
        "frequency_penalty": 0.5,
        "logprobs": true,
        "max_completion_tokens": 100,
        "max_output_tokens": 100,
        "max_tokens": 100,
        "presence_penalty": 0.5,
        "response_format": {"type": "json_object"},
        "seed": 42,
        "stop": ["done"],
        "temperature": 0.0,
        "top_logprobs": 5,
        "top_p": 0.9
    }))
    .expect("filter unsupported Responses model parameters");

    assert_eq!(parsed, expected);
}

#[test]
fn parses_responses_array_and_function_round_trip() {
    let parsed = parse_responses_request(json!({
        "model": "gpt-test",
        "input": [
            {"role": "developer", "content": "Call the provided tool"},
            {
                "type": "function_call",
                "id": "fc_1",
                "name": "weather",
                "arguments": "{\"city\":\"Sofia\"}",
                "call_id": "call_1",
                "status": "completed"
            },
            {"type": "function_call_output", "call_id": "call_1", "output": "24 C"}
        ],
        "tools": [{
            "type": "function",
            "name": "weather",
            "parameters": {"type": "object"}
        }],
        "tool_choice": "required",
        "parallel_tool_calls": false,
        "store": false,
        "previous_response_id": null
    }))
    .expect("parse function round trip");

    assert_eq!(parsed.model.as_deref(), Some("gpt-test"));
    assert_eq!(parsed.input.len(), 3);
    assert_eq!(parsed.tool_choice, "required");
    assert_eq!(parsed.parallel_tool_calls, false);
    assert_eq!(parsed.tools.as_ref().map(Vec::len), Some(1));
    assert_eq!(
        parsed.input[1],
        ResponseItem::FunctionCall {
            id: Some(ResponseItemId::from_server("fc_1".to_string())),
            name: "weather".to_string(),
            namespace: None,
            arguments: "{\"city\":\"Sofia\"}".to_string(),
            call_id: "call_1".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }
    );
    assert_eq!(
        parsed.input[2],
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "call_1".to_string(),
            output: FunctionCallOutputPayload::from_text("24 C".to_string()),
            internal_chat_message_metadata_passthrough: None,
        }
    );
}

#[test]
fn parses_responses_and_chat_vision_inputs() {
    let responses = parse_responses_request(json!({
        "model": "gpt-5.6-luna",
        "input": [{
            "role": "user",
            "content": [
                {"type": "input_text", "text": "Extract the text"},
                {
                    "type": "input_image",
                    "image_url": "https://example.test/page-1.png",
                    "detail": "high"
                }
            ]
        }],
        "reasoning": {"effort": "low"}
    }))
    .expect("parse Responses vision request");
    let chat = parse_chat_request(json!({
        "model": "gpt-5.6-luna",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "Extract the text"},
                {
                    "type": "image_url",
                    "image_url": {
                        "url": "data:image/png;base64,iVBORw0KGgo=",
                        "detail": "original"
                    }
                }
            ]
        }],
        "reasoning_effort": "low"
    }))
    .expect("parse Chat vision request");

    assert_eq!(
        responses.input,
        vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![
                ContentItem::InputText {
                    text: "Extract the text".to_string(),
                },
                ContentItem::InputImage {
                    image_url: "https://example.test/page-1.png".to_string(),
                    detail: Some(ImageDetail::High),
                },
            ],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }]
    );
    assert_eq!(
        chat.request.input,
        vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![
                ContentItem::InputText {
                    text: "Extract the text".to_string(),
                },
                ContentItem::InputImage {
                    image_url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                    detail: Some(ImageDetail::Original),
                },
            ],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }]
    );
}

#[test]
fn accepts_ten_images_and_rejects_the_eleventh() {
    let images = (0..MAX_IMAGE_INPUTS)
        .map(|index| {
            json!({
                "type": "input_image",
                "image_url": format!("https://example.test/page-{index}.png")
            })
        })
        .collect::<Vec<_>>();
    let accepted = parse_responses_request(json!({
        "input": [{"role": "user", "content": images}]
    }))
    .expect("ten images must be accepted");
    let mut too_many = images;
    too_many.push(json!({
        "type": "input_image",
        "image_url": "https://example.test/page-10.png"
    }));
    let error = parse_responses_request(json!({
        "input": [{"role": "user", "content": too_many}]
    }))
    .expect_err("eleven images must be rejected");

    assert_eq!(accepted.input.len(), 1);
    assert_eq!(error.param.as_deref(), Some("input"));
}

#[test]
fn rejects_state_unknown_items_and_internal_metadata() {
    let cases = [
        (json!({"input": "hello", "store": true}), "store"),
        (
            json!({"input": "hello", "previous_response_id": "resp_1"}),
            "previous_response_id",
        ),
        (
            json!({"input": "hello", "unknown_parameter": true}),
            "unknown_parameter",
        ),
        (json!({"input": [{"type": "unknown"}]}), "input"),
        (
            json!({"input": [{
                "type": "message",
                "role": "user",
                "content": "hello",
                "internal_chat_message_metadata_passthrough": {"secret": true}
            }]}),
            "input[0].internal_chat_message_metadata_passthrough",
        ),
        (
            json!({"input": [{"type": "message", "role": "tool", "content": "hello"}]}),
            "input",
        ),
        (
            json!({"input": [{"type": "function_call_output", "call_id": "missing", "output": "x"}]}),
            "input",
        ),
    ];

    for (request, param) in cases {
        let error = parse_responses_request(request).expect_err("request must fail");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.param.as_deref(), Some(param));
    }
}

#[test]
fn tool_choice_is_a_validated_string() {
    let object = parse_responses_request(json!({
        "input": "hello",
        "tool_choice": {"type": "function", "name": "weather"}
    }))
    .expect_err("object tool choice is unsupported");
    let required = parse_responses_request(json!({
        "input": "hello",
        "tool_choice": "required"
    }))
    .expect_err("required without tools is invalid");

    assert_eq!(object.param.as_deref(), Some("tool_choice"));
    assert_eq!(required.param.as_deref(), Some("tool_choice"));
}

#[test]
fn rejects_invalid_or_extended_function_tool_shapes() {
    let responses = parse_responses_request(json!({
        "input": "hello",
        "tools": [{"type": "function", "name": "weather", "strict": "yes"}]
    }))
    .expect_err("strict must be boolean");
    let chat = parse_chat_request(json!({
        "messages": [{"role": "user", "content": "hello"}],
        "tools": [{
            "type": "function",
            "function": {"name": "weather"},
            "unexpected": true
        }]
    }))
    .expect_err("unknown Chat tool fields must fail");

    assert_eq!(responses.param.as_deref(), Some("tools"));
    assert_eq!(chat.param.as_deref(), Some("tools[].unexpected"));
}

#[test]
fn parses_chat_history_tools_and_usage_option() {
    let parsed = parse_chat_request(json!({
        "model": "gpt-test",
        "messages": [
            {"role": "system", "content": "system context"},
            {"role": "developer", "content": "developer context"},
            {"role": "user", "content": [{"type": "text", "text": "weather?"}]},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "weather",
                        "arguments": "{\"city\":\"Sofia\"}"
                    }
                }]
            },
            {"role": "tool", "tool_call_id": "call_1", "content": "24 C"}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "weather",
                "description": "Get weather",
                "parameters": {"type": "object"},
                "strict": true
            }
        }],
        "stream": true,
        "stream_options": {"include_usage": true},
        "n": 1
    }))
    .expect("parse Chat request");

    assert_eq!(parsed.request.input.len(), 5);
    assert_eq!(parsed.request.stream, true);
    assert_eq!(parsed.include_usage, true);
    assert_eq!(
        parsed.request.tools,
        Some(vec![json!({
            "type": "function",
            "name": "weather",
            "description": "Get weather",
            "parameters": {"type": "object"},
            "strict": true
        })])
    );
    assert_eq!(parsed.request.tool_choice, "auto");
    assert!(matches!(
        &parsed.request.input[0],
        ResponseItem::Message { role, .. } if role == "developer"
    ));
    assert_eq!(
        parsed.request.input[3],
        ResponseItem::FunctionCall {
            id: None,
            name: "weather".to_string(),
            namespace: None,
            arguments: "{\"city\":\"Sofia\"}".to_string(),
            call_id: "call_1".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }
    );
}

#[test]
fn filters_unsupported_chat_model_parameters() {
    let expected = parse_chat_request(json!({
        "messages": [{"role": "user", "content": "hello"}]
    }))
    .expect("parse baseline Chat request");
    let parsed = parse_chat_request(json!({
        "messages": [{"role": "user", "content": "hello"}],
        "frequency_penalty": 0.5,
        "logprobs": true,
        "max_completion_tokens": 100,
        "max_output_tokens": 100,
        "max_tokens": 100,
        "presence_penalty": 0.5,
        "response_format": {"type": "json_object"},
        "seed": 42,
        "stop": ["done"],
        "temperature": 0.0,
        "top_logprobs": 5,
        "top_p": 0.9
    }))
    .expect("filter unsupported Chat model parameters");

    assert_eq!(parsed, expected);
}

#[test]
fn rejects_unmatched_chat_tool_output() {
    let error = parse_chat_request(json!({
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "tool", "tool_call_id": "missing", "content": "result"}
        ]
    }))
    .expect_err("unmatched tool output must fail");

    assert_eq!(error.param.as_deref(), Some("messages"));
}

#[test]
fn rejects_chat_multiple_choices_and_unknown_parameters() {
    let multiple = parse_chat_request(json!({
        "messages": [{"role": "user", "content": "hello"}],
        "n": 2
    }))
    .expect_err("multiple choices are unsupported");
    let unknown = parse_chat_request(json!({
        "messages": [{"role": "user", "content": "hello"}],
        "unknown_parameter": true
    }))
    .expect_err("unknown parameter is unsupported");

    assert_eq!(multiple.param.as_deref(), Some("n"));
    assert_eq!(unknown.param.as_deref(), Some("unknown_parameter"));
}

#[test]
fn renders_models_and_standard_error_envelopes() {
    assert_eq!(
        models_response(&["gpt-a".to_string(), "gpt-b".to_string()], 123),
        json!({
            "object": "list",
            "data": [
                {"id": "gpt-a", "object": "model", "created": 123, "owned_by": "openai"},
                {"id": "gpt-b", "object": "model", "created": 123, "owned_by": "openai"}
            ]
        })
    );
    let error = ApiError::invalid("Bad input", Some("input"));
    assert_eq!(
        error.body(),
        json!({"error": {
            "message": "Bad input",
            "type": "invalid_request_error",
            "param": "input",
            "code": null
        }})
    );
}

#[test]
fn renders_responses_messages_calls_and_nested_usage() {
    let request = parse_responses_request(json!({
        "model": "gpt-test",
        "input": "hello",
        "instructions": "explicit",
        "tools": [{"type": "function", "name": "weather"}],
        "reasoning": {"effort": "high"}
    }))
    .expect("parse request");
    let output = vec![
        ResponseItem::Message {
            id: Some(ResponseItemId::from_server("msg_1".to_string())),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "Checking".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCall {
            id: Some(ResponseItemId::from_server("fc_1".to_string())),
            name: "weather".to_string(),
            namespace: None,
            arguments: "{\"city\":\"Sofia\"}".to_string(),
            call_id: "call_1".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    let response = responses_response("resp_1", "gpt-test", 123, &request, &output, &usage());

    assert_eq!(response["object"], "response");
    assert_eq!(response["instructions"], "explicit");
    assert_eq!(response["output"][0]["status"], "completed");
    assert_eq!(
        response["output"][0]["content"][0]["annotations"],
        json!([])
    );
    assert_eq!(response["output"][1]["arguments"], "{\"city\":\"Sofia\"}");
    assert_eq!(
        response["usage"]["input_tokens_details"]["cached_tokens"],
        2
    );
    assert_eq!(
        response["usage"]["output_tokens_details"]["reasoning_tokens"],
        5
    );
    assert_eq!(response["usage"].get("cache_write_input_tokens"), None);
}

#[test]
fn renders_chat_text_and_tool_calls() {
    let request = ParsedChatRequest {
        request: parse_responses_request(json!({"input": "hello"})).expect("request"),
        include_usage: false,
    };
    let text = chat_response(
        "chatcmpl_1",
        "gpt-test",
        123,
        &request,
        &[message(
            "assistant",
            ContentItem::OutputText {
                text: "Hello".to_string(),
            },
        )],
        &usage(),
    );
    let tool = chat_response(
        "chatcmpl_2",
        "gpt-test",
        124,
        &request,
        &[ResponseItem::FunctionCall {
            id: None,
            name: "weather".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            call_id: "call_1".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }],
        &usage(),
    );

    assert_eq!(text["choices"][0]["message"]["content"], "Hello");
    assert_eq!(text["choices"][0]["finish_reason"], "stop");
    assert_eq!(tool["choices"][0]["message"]["content"], json!(null));
    assert_eq!(
        tool["choices"][0]["message"]["tool_calls"][0]["id"],
        "call_1"
    );
    assert_eq!(tool["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(tool["usage"]["completion_tokens"], 11);
}
