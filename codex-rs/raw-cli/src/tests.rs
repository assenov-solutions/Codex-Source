use std::ffi::OsString;
use std::fs;

use clap::Parser;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

use super::PROTECTED_CODEX_HOME_DIR_NAMES;
use super::RAW_HOME_DIR_NAME;
use super::RAW_HOME_MARKER;
use super::RAW_HOME_MARKER_CONTENT;
use super::RawCli;
use super::RawCommand;
use super::claim_raw_home;
use super::ensure_not_protected;
use super::prepare_command_raw_home;
use super::runtime::RawApiRequest;
use super::runtime::ResolvedRawModel;
use super::runtime::build_api_request;
use super::runtime::build_minimal_request;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ImageDetail;
use codex_protocol::models::ResponseItem;

#[test]
fn raw_home_has_a_distinct_default_name() {
    assert_eq!(RAW_HOME_DIR_NAME, ".codex-raw");
}

#[test]
fn codex2_home_name_is_protected() {
    assert!(PROTECTED_CODEX_HOME_DIR_NAMES.contains(&".codex2"));
    assert!(PROTECTED_CODEX_HOME_DIR_NAMES.contains(&".codex-2"));
}

#[test]
fn minimal_request_contains_only_one_user_message() {
    let request = build_minimal_request("gpt-test".to_string(), "hello".to_string(), false);
    let value = serde_json::to_value(request).expect("serialize minimal request");

    assert_eq!(value.get("instructions"), None);
    assert_eq!(value.get("tools"), None);
    assert_eq!(value["tool_choice"], "none");
    assert_eq!(value["parallel_tool_calls"], false);
    assert_eq!(value["input"].as_array().map(Vec::len), Some(1));
    assert_eq!(value["input"][0]["role"], "user");
    assert_eq!(value["input"][0]["content"][0]["text"], "hello");
}

#[test]
fn responses_lite_adds_only_required_reasoning_context() {
    let request = build_minimal_request("gpt-test".to_string(), "hello".to_string(), true);
    let value = serde_json::to_value(request).expect("serialize minimal request");

    assert_eq!(
        value["reasoning"],
        serde_json::json!({ "context": "all_turns" })
    );
    assert_eq!(value.get("instructions"), None);
    assert_eq!(value.get("tools"), None);
    assert_eq!(value["input"].as_array().map(Vec::len), Some(1));
    assert_eq!(value["input"][0]["role"], "user");
    assert_eq!(value["input"][0]["content"][0]["text"], "hello");
}

#[test]
fn existing_unclaimed_directory_is_rejected() {
    let parent = tempdir().expect("create temporary parent");
    let raw_home = parent.path().join("existing");
    fs::create_dir(&raw_home).expect("create unclaimed directory");

    let error = claim_raw_home(&raw_home).expect_err("unclaimed directory must be rejected");

    assert!(error.to_string().contains("valid Raw marker"));
}

#[test]
fn new_raw_home_is_claimed_and_reusable() {
    let parent = tempdir().expect("create temporary parent");
    let raw_home = parent.path().join("raw");

    claim_raw_home(&raw_home).expect("claim new Raw home");
    claim_raw_home(&raw_home).expect("reuse claimed Raw home");

    assert_eq!(
        fs::read_to_string(raw_home.join(RAW_HOME_MARKER)).expect("read Raw marker"),
        RAW_HOME_MARKER_CONTENT
    );
}

#[test]
fn protected_home_and_descendants_are_rejected() {
    let parent = tempdir().expect("create temporary parent");
    let protected = parent.path().join(".codex");

    assert!(ensure_not_protected(&protected, std::slice::from_ref(&protected)).is_err());
    assert!(ensure_not_protected(&protected.join("raw"), &[protected]).is_err());
}

#[test]
fn unknown_subcommand_is_treated_as_prompt() {
    let cli = RawCli::try_parse_from(["codex-raw", "hello"]).expect("parse prompt");

    match cli.command {
        RawCommand::Prompt(values) => assert_eq!(values, vec![OsString::from("hello")]),
        command => panic!("unexpected command: {command:?}"),
    }
}

#[test]
fn app_server_is_a_real_subcommand() {
    let cli = RawCli::try_parse_from(["codex-raw", "app-server"]).expect("parse app-server");

    assert!(matches!(cli.command, RawCommand::AppServer));
}

#[test]
fn api_server_is_a_real_subcommand() {
    let cli = RawCli::try_parse_from([
        "codex-raw",
        "api-server",
        "--listen",
        "0.0.0.0:0",
        "--max-concurrency",
        "4",
    ])
    .expect("parse api-server");

    assert!(matches!(
        cli.command,
        RawCommand::ApiServer {
            listen,
            max_concurrency: 4,
            api_token: _,
        } if listen.to_string() == "0.0.0.0:0"
    ));
}

#[test]
fn update_check_is_a_real_subcommand() {
    let cli =
        RawCli::try_parse_from(["codex-raw", "update", "--check"]).expect("parse update check");

    assert!(matches!(cli.command, RawCommand::Update { check: true }));
}

#[test]
fn update_check_does_not_prepare_or_claim_a_raw_home() {
    let parent = tempdir().expect("temporary parent");
    let unclaimed = parent.path().join("must-remain-unclaimed");
    let command = RawCommand::Update { check: true };

    let raw_home =
        prepare_command_raw_home(&command, Some(unclaimed.clone())).expect("skip Raw home");

    assert_eq!(raw_home, None);
    assert!(!unclaimed.exists());
}

#[test]
fn api_server_accepts_an_explicit_bearer_token() {
    let token = "test-api-token";
    let cli = RawCli::try_parse_from(["codex-raw", "api-server", "--api-token", token])
        .expect("parse api-server token");

    assert!(matches!(
        cli.command,
        RawCommand::ApiServer {
            api_token: Some(token),
            ..
        } if token == "test-api-token"
    ));
}

#[test]
fn api_request_passes_only_explicit_tools_and_context() {
    let request = build_api_request(RawApiRequest {
        model: resolved_model(/*use_responses_lite*/ false),
        instructions: "Be concise".to_string(),
        input: vec![user_message("hello")],
        tools: vec![serde_json::json!({
            "type": "function",
            "name": "get_weather",
            "parameters": { "type": "object" },
        })],
        tool_choice: "auto".to_string(),
        parallel_tool_calls: true,
        reasoning_effort: None,
        service_tier: None,
    })
    .expect("build API request");
    let value = serde_json::to_value(request).expect("serialize API request");

    assert_eq!(value["instructions"], "Be concise");
    assert_eq!(value["input"].as_array().map(Vec::len), Some(1));
    assert_eq!(value["tools"].as_array().map(Vec::len), Some(1));
    assert_eq!(value["tools"][0]["name"], "get_weather");
    assert_eq!(value["tool_choice"], "auto");
    assert_eq!(value["parallel_tool_calls"], true);
    assert_eq!(value["store"], false);
    assert_eq!(value.get("client_metadata"), None);
}

#[test]
fn responses_lite_embeds_only_client_supplied_tools_and_instructions() {
    let request = build_api_request(RawApiRequest {
        model: resolved_model(/*use_responses_lite*/ true),
        instructions: "Be concise".to_string(),
        input: vec![user_message("hello")],
        tools: vec![serde_json::json!({
            "type": "function",
            "name": "get_weather",
            "parameters": { "type": "object" },
        })],
        tool_choice: "auto".to_string(),
        parallel_tool_calls: true,
        reasoning_effort: None,
        service_tier: None,
    })
    .expect("build Lite API request");
    let value = serde_json::to_value(request).expect("serialize Lite API request");

    assert_eq!(value.get("instructions"), None);
    assert_eq!(value.get("tools"), None);
    assert_eq!(value["parallel_tool_calls"], false);
    assert_eq!(value["input"].as_array().map(Vec::len), Some(3));
    assert_eq!(value["input"][0]["type"], "additional_tools");
    assert_eq!(value["input"][0]["tools"][0]["name"], "get_weather");
    assert_eq!(value["input"][1]["role"], "developer");
    assert_eq!(value["input"][1]["content"][0]["text"], "Be concise");
    assert_eq!(value["input"][2]["role"], "user");
}

#[test]
fn responses_lite_strips_unsupported_image_detail() {
    let request = build_api_request(RawApiRequest {
        model: resolved_model(/*use_responses_lite*/ true),
        instructions: String::new(),
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputImage {
                image_url: "https://example.test/page.png".to_string(),
                detail: Some(ImageDetail::High),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        tools: Vec::new(),
        tool_choice: "none".to_string(),
        parallel_tool_calls: false,
        reasoning_effort: None,
        service_tier: None,
    })
    .expect("build Lite vision request");
    let value = serde_json::to_value(request).expect("serialize Lite vision request");

    assert_eq!(
        value["input"][0]["content"][0],
        serde_json::json!({
            "type": "input_image",
            "image_url": "https://example.test/page.png"
        })
    );
}

fn resolved_model(use_responses_lite: bool) -> ResolvedRawModel {
    ResolvedRawModel {
        slug: "gpt-test".to_string(),
        use_responses_lite,
        context_window: Some(128_000),
    }
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}
