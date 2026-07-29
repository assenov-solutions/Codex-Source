use std::net::SocketAddr;

use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::header::AUTHORIZATION;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;

use super::ListenScope;
use super::authorization_is_valid;
use super::error_response;
use super::model_alias_selection;
use super::requires_api_authorization;
use super::unauthorized_response;
use super::upstream_status;
use super::validate_server_options;

const VALID_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTHER_VALID_TOKEN: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

#[test]
fn validates_listen_scope_concurrency_and_token_requirements() {
    let ipv4_loopback: SocketAddr = "127.0.0.1:0".parse().expect("IPv4 loopback address");
    let ipv6_loopback: SocketAddr = "[::1]:0".parse().expect("IPv6 loopback address");
    let all_ipv4: SocketAddr = "0.0.0.0:8080".parse().expect("all IPv4 interfaces");
    let all_ipv6: SocketAddr = "[::]:8080".parse().expect("all IPv6 interfaces");

    assert_eq!(
        validate_server_options(
            ipv4_loopback,
            /*max_concurrency*/ 1,
            /*api_token*/ None
        )
        .expect("valid unauthenticated IPv4 loopback options"),
        ListenScope::Loopback
    );
    assert_eq!(
        validate_server_options(
            ipv6_loopback,
            /*max_concurrency*/ 1,
            /*api_token*/ None
        )
        .expect("valid unauthenticated IPv6 loopback options"),
        ListenScope::Loopback
    );
    assert_eq!(
        validate_server_options(all_ipv4, /*max_concurrency*/ 1, Some(VALID_TOKEN))
            .expect("valid authenticated all-interface IPv4 options"),
        ListenScope::NonLoopback
    );
    assert_eq!(
        validate_server_options(all_ipv6, /*max_concurrency*/ 1, Some(VALID_TOKEN))
            .expect("valid authenticated all-interface IPv6 options"),
        ListenScope::NonLoopback
    );
    assert!(
        validate_server_options(
            ipv4_loopback,
            /*max_concurrency*/ 0,
            /*api_token*/ None
        )
        .is_err()
    );
    assert!(
        validate_server_options(
            ipv4_loopback,
            /*max_concurrency*/ 257,
            /*api_token*/ None
        )
        .is_err()
    );
    assert!(
        validate_server_options(
            all_ipv4, /*max_concurrency*/ 1, /*api_token*/ None
        )
        .is_err()
    );
    assert!(validate_server_options(ipv4_loopback, /*max_concurrency*/ 1, Some("")).is_err());
    assert!(
        validate_server_options(ipv4_loopback, /*max_concurrency*/ 1, Some("too-short")).is_err()
    );
    assert!(
        validate_server_options(
            ipv4_loopback,
            /*max_concurrency*/ 1,
            Some("token with spaces")
        )
        .is_err()
    );
    assert!(
        validate_server_options(
            ipv4_loopback,
            /*max_concurrency*/ 1,
            Some("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!")
        )
        .is_err()
    );
}

#[test]
fn bearer_auth_is_optional_without_a_configured_token_and_strict_when_enabled() {
    let no_headers = HeaderMap::new();
    assert!(authorization_is_valid(
        &no_headers,
        /*expected_token*/ None
    ));

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer raw-local"));
    assert!(authorization_is_valid(
        &headers, /*expected_token*/ None
    ));
    assert!(!authorization_is_valid(&no_headers, Some(VALID_TOKEN)));
    assert!(!authorization_is_valid(&headers, Some(VALID_TOKEN)));

    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static(
            "bearer 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
    );
    assert!(authorization_is_valid(&headers, Some(VALID_TOKEN)));

    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static(
            "Basic 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
    );
    assert!(!authorization_is_valid(&headers, Some(VALID_TOKEN)));

    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static(
            "Bearer fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
        ),
    );
    assert!(!authorization_is_valid(&headers, Some(VALID_TOKEN)));
    assert!(authorization_is_valid(&headers, Some(OTHER_VALID_TOKEN)));

    headers.append(
        AUTHORIZATION,
        HeaderValue::from_static(
            "Bearer 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
    );
    assert!(!authorization_is_valid(&headers, Some(VALID_TOKEN)));
}

#[test]
fn health_is_open_while_ready_and_the_v1_namespace_are_protected() {
    assert!(!requires_api_authorization("/healthz"));
    assert!(!requires_api_authorization("/other"));
    assert!(requires_api_authorization("/readyz"));
    assert!(requires_api_authorization("/v1"));
    assert!(requires_api_authorization("/v1/models"));
    assert!(!requires_api_authorization("/v10/models"));
}

#[test]
fn unauthorized_responses_advertise_bearer_authentication() {
    let response = unauthorized_response();

    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get(axum::http::header::WWW_AUTHENTICATE),
        Some(&HeaderValue::from_static("Bearer"))
    );
}

#[test]
fn maps_typed_upstream_failures_to_useful_http_statuses() {
    assert_eq!(
        upstream_status(&codex_api::ApiError::InvalidRequest {
            message: "bad input".to_string(),
        }),
        axum::http::StatusCode::BAD_REQUEST
    );
    assert_eq!(
        upstream_status(&codex_api::ApiError::QuotaExceeded),
        axum::http::StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        upstream_status(&codex_api::ApiError::ServerOverloaded),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        upstream_status(&codex_api::ApiError::Transport(
            codex_api::TransportError::Http {
                status: axum::http::StatusCode::FORBIDDEN,
                url: Some("https://example.test/images/generations".to_string()),
                headers: None,
                body: Some("forbidden".to_string()),
            }
        )),
        axum::http::StatusCode::FORBIDDEN
    );
}

#[test]
fn api_errors_use_the_openai_envelope() {
    let response = error_response(
        axum::http::StatusCode::BAD_REQUEST,
        "bad input",
        Some("input"),
        Some("invalid_input"),
    );

    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
}

#[test]
fn parses_model_reasoning_effort_aliases() {
    assert_eq!(
        model_alias_selection(Some("gpt-5.6-sol (High)"), None),
        (Some("gpt-5.6-sol".to_string()), Some(ReasoningEffort::High))
    );
    assert_eq!(
        model_alias_selection(Some("gpt-5.6-sol(xhigh)"), None),
        (
            Some("gpt-5.6-sol".to_string()),
            Some(ReasoningEffort::XHigh)
        )
    );
}

#[test]
fn explicit_reasoning_effort_overrides_model_alias() {
    assert_eq!(
        model_alias_selection(Some("gpt-5.6-terra (Ultra)"), Some(ReasoningEffort::Low)),
        (
            Some("gpt-5.6-terra".to_string()),
            Some(ReasoningEffort::Low)
        )
    );
    assert_eq!(
        model_alias_selection(Some("gpt-5.6-terra (Unknown)"), None),
        (Some("gpt-5.6-terra (Unknown)".to_string()), None)
    );
}
