use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::Request;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::header::WWW_AUTHENTICATE;
use axum::middleware;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use codex_api::ApiError as CodexApiError;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::TokenUsage;
use constant_time_eq::constant_time_eq;
use serde_json::Value;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;

use crate::api_server_chat_wire::parse_chat_request;
use crate::api_server_http_stream::chat_sse;
use crate::api_server_http_stream::responses_sse;
use crate::api_server_image_wire::DEFAULT_IMAGE_MODEL;
use crate::api_server_image_wire::image_generation_response;
use crate::api_server_image_wire::parse_image_generation_request;
use crate::api_server_response::chat_response;
use crate::api_server_response::models_response;
use crate::api_server_response::responses_response;
use crate::api_server_wire::ApiError;
use crate::api_server_wire::ParsedRequest;
use crate::api_server_wire::parse_responses_request;
use crate::runtime::RawApiRequest;
use crate::runtime::RawResponseStream;
use crate::runtime::RawRuntime;
use crate::runtime::RawStreamEvent;
use crate::runtime::ResolvedRawModel;

const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_CONCURRENCY: usize = 256;
const MIN_API_TOKEN_BYTES: usize = 32;
const MAX_API_TOKEN_BYTES: usize = 512;

pub(crate) struct ApiServerOptions {
    pub(crate) listen: SocketAddr,
    pub(crate) max_concurrency: usize,
    pub(crate) api_token: Option<String>,
}

#[derive(Clone)]
struct ApiState {
    runtime: RawRuntime,
    default_model: ResolvedRawModel,
    inference_limit: Arc<Semaphore>,
    next_id: Arc<AtomicU64>,
}

pub(crate) async fn run(
    raw_home: &Path,
    default_model: Option<String>,
    options: ApiServerOptions,
) -> Result<()> {
    let listen_scope = validate_server_options(
        options.listen,
        options.max_concurrency,
        options.api_token.as_deref(),
    )?;
    let api_token = options.api_token.map(Arc::<str>::from);
    let listener = TcpListener::bind(options.listen)
        .await
        .with_context(|| format!("failed to bind the Raw API server to {}", options.listen))?;
    let actual_address = listener.local_addr()?;
    let runtime = RawRuntime::new(raw_home).await?;
    let default_model = runtime.resolve_model(default_model.as_deref()).await;
    let state = ApiState {
        runtime,
        default_model,
        inference_limit: Arc::new(Semaphore::new(options.max_concurrency)),
        next_id: Arc::new(AtomicU64::new(1)),
    };

    if listen_scope == ListenScope::NonLoopback {
        eprintln!();
        eprintln!(
            "WARNING: Codex Raw API is listening on a non-loopback address ({actual_address})."
        );
        eprintln!(
            "Bearer authentication is enabled, but plain HTTP does not protect the token in transit."
        );
        eprintln!(
            "Use a hardened reverse proxy with TLS, connection/header timeouts, and connection limits."
        );
        eprintln!(
            "Firewall the Raw port so only that proxy or explicitly trusted clients can reach it."
        );
        eprintln!();
    }
    if api_token.is_some() {
        eprintln!("Bearer authentication enabled for /readyz and /v1.");
    }
    eprintln!("Codex Raw API listening on http://{actual_address}");
    eprintln!("OpenAI-compatible base URL: http://{actual_address}/v1");
    if actual_address.ip().is_unspecified() {
        eprintln!(
            "Remote clients must replace the wildcard address with this machine's LAN IP or DNS name."
        );
    }

    axum::serve(listener, router(state, api_token))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Raw API server failed")
}

fn router(state: ApiState, api_token: Option<Arc<str>>) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/v1/models", get(list_models))
        .route("/v1/responses", post(create_response))
        .route("/v1/chat/completions", post(create_chat_completion))
        .route("/v1/images/generations", post(create_image_generation))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(middleware::from_fn_with_state(
            api_token,
            require_api_authorization,
        ))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn ready(State(state): State<ApiState>) -> Response {
    match state.runtime.check_auth_ready().await {
        Ok(()) => Json(json!({ "status": "ready" })).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready" })),
        )
            .into_response(),
    }
}

async fn require_api_authorization(
    State(api_token): State<Option<Arc<str>>>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(value) = request.headers_mut().get_mut(AUTHORIZATION) {
        value.set_sensitive(true);
    }
    if requires_api_authorization(request.uri().path())
        && !authorization_is_valid(request.headers(), api_token.as_deref())
    {
        return unauthorized_response();
    }
    next.run(request).await
}

fn requires_api_authorization(path: &str) -> bool {
    path == "/readyz" || path == "/v1" || path.starts_with("/v1/")
}

fn authorization_is_valid(headers: &HeaderMap, expected_token: Option<&str>) -> bool {
    let Some(expected_token) = expected_token else {
        return true;
    };
    let mut authorization_values = headers.get_all(AUTHORIZATION).iter();
    let Some(header) = authorization_values.next() else {
        return false;
    };
    if authorization_values.next().is_some() {
        return false;
    }
    let Ok(header) = header.to_str() else {
        return false;
    };
    let Some((scheme, supplied_token)) = header.split_once(' ') else {
        return false;
    };
    let supplied_token = supplied_token.trim().as_bytes();
    scheme.eq_ignore_ascii_case("Bearer")
        && constant_time_eq(supplied_token, expected_token.as_bytes())
}

fn unauthorized_response() -> Response {
    let mut response = error_response(
        StatusCode::UNAUTHORIZED,
        "A valid Raw API bearer token is required",
        None,
        Some("invalid_api_token"),
    );
    response
        .headers_mut()
        .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

async fn list_models(State(state): State<ApiState>) -> Response {
    let models = state.runtime.list_models().await;
    let mut model_ids = model_ids_with_effort_aliases(&models);
    if !model_ids.iter().any(|model| model == DEFAULT_IMAGE_MODEL) {
        model_ids.push(DEFAULT_IMAGE_MODEL.to_string());
    }
    Json(models_response(&model_ids, unix_seconds())).into_response()
}

async fn create_image_generation(
    State(state): State<ApiState>,
    headers: HeaderMap,
    payload: std::result::Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Some(response) = browser_origin_rejection(&headers) {
        return response;
    }
    let payload = match payload {
        Ok(Json(payload)) => payload,
        Err(error) => return json_rejection_response(error),
    };
    let request = match parse_image_generation_request(payload) {
        Ok(request) => request,
        Err(error) => return wire_error_response(error),
    };
    let _permit = match acquire_inference_permit(&state) {
        Ok(permit) => permit,
        Err(response) => return response,
    };
    match state.runtime.generate_image(request).await {
        Ok(response) => Json(image_generation_response(response)).into_response(),
        Err(error) => upstream_error_response(error),
    }
}

async fn create_response(
    State(state): State<ApiState>,
    headers: HeaderMap,
    payload: std::result::Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Some(response) = browser_origin_rejection(&headers) {
        return response;
    }
    let payload = match payload {
        Ok(Json(payload)) => payload,
        Err(error) => return json_rejection_response(error),
    };
    let mut request = match parse_responses_request(payload) {
        Ok(request) => request,
        Err(error) => return wire_error_response(error),
    };
    let permit = match acquire_inference_permit(&state) {
        Ok(permit) => permit,
        Err(response) => return response,
    };
    let (requested_model, reasoning_effort) =
        model_alias_selection(request.model.as_deref(), request.reasoning_effort.take());
    request.reasoning_effort = reasoning_effort;
    let model = resolve_model(&state, requested_model.as_deref()).await;
    let response_id = state.allocate_id("resp");
    let created = unix_seconds();
    let stream = match state
        .runtime
        .start_api_response(runtime_request(&request, model.clone()))
        .await
    {
        Ok(stream) => stream,
        Err(error) => return upstream_error_response(error),
    };

    if request.stream {
        responses_sse(stream, request, model.slug, response_id, created, permit)
    } else {
        match collect_response(stream).await {
            Ok(collected) => Json(responses_response(
                &response_id,
                &model.slug,
                created,
                &request,
                &collected.items,
                &collected.usage,
            ))
            .into_response(),
            Err(error) => upstream_error_response(error),
        }
    }
}

async fn create_chat_completion(
    State(state): State<ApiState>,
    headers: HeaderMap,
    payload: std::result::Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Some(response) = browser_origin_rejection(&headers) {
        return response;
    }
    let payload = match payload {
        Ok(Json(payload)) => payload,
        Err(error) => return json_rejection_response(error),
    };
    let mut request = match parse_chat_request(payload) {
        Ok(request) => request,
        Err(error) => return wire_error_response(error),
    };
    let permit = match acquire_inference_permit(&state) {
        Ok(permit) => permit,
        Err(response) => return response,
    };
    let (requested_model, reasoning_effort) = model_alias_selection(
        request.request.model.as_deref(),
        request.request.reasoning_effort.take(),
    );
    request.request.reasoning_effort = reasoning_effort;
    let model = resolve_model(&state, requested_model.as_deref()).await;
    let response_id = state.allocate_id("chatcmpl");
    let created = unix_seconds();
    let stream = match state
        .runtime
        .start_api_response(runtime_request(&request.request, model.clone()))
        .await
    {
        Ok(stream) => stream,
        Err(error) => return upstream_error_response(error),
    };

    if request.request.stream {
        chat_sse(stream, request, model.slug, response_id, created, permit)
    } else {
        match collect_response(stream).await {
            Ok(collected) => Json(chat_response(
                &response_id,
                &model.slug,
                created,
                &request,
                &collected.items,
                &collected.usage,
            ))
            .into_response(),
            Err(error) => upstream_error_response(error),
        }
    }
}

struct CollectedResponse {
    items: Vec<ResponseItem>,
    usage: TokenUsage,
}

async fn collect_response(mut stream: RawResponseStream) -> Result<CollectedResponse> {
    let mut items = Vec::new();
    let mut streamed_text = String::new();
    while let Some(event) = stream.next_event().await? {
        match event {
            RawStreamEvent::OutputTextDelta(delta) => streamed_text.push_str(&delta),
            RawStreamEvent::OutputItemDone(item) => items.push(item),
            RawStreamEvent::Completed { token_usage, .. } => {
                if !streamed_text.is_empty() && !items.iter().any(item_has_output_text) {
                    items.push(ResponseItem::Message {
                        id: None,
                        role: "assistant".to_string(),
                        content: vec![ContentItem::OutputText {
                            text: streamed_text,
                        }],
                        phase: None,
                        internal_chat_message_metadata_passthrough: None,
                    });
                }
                return Ok(CollectedResponse {
                    items,
                    usage: token_usage,
                });
            }
            RawStreamEvent::Created
            | RawStreamEvent::OutputItemAdded(_)
            | RawStreamEvent::ToolCallInputDelta { .. } => {}
        }
    }
    bail!("Raw response stream ended without a completion event")
}

fn runtime_request(request: &ParsedRequest, model: ResolvedRawModel) -> RawApiRequest {
    RawApiRequest {
        model,
        instructions: request.instructions.clone().unwrap_or_default(),
        input: request.input.clone(),
        tools: request.tools.clone().unwrap_or_default(),
        tool_choice: request.tool_choice.clone(),
        parallel_tool_calls: request.parallel_tool_calls,
        reasoning_effort: request.reasoning_effort.clone(),
        service_tier: request.service_tier.clone(),
    }
}

async fn resolve_model(state: &ApiState, requested: Option<&str>) -> ResolvedRawModel {
    if requested.is_none_or(|requested| requested == state.default_model.slug) {
        state.default_model.clone()
    } else {
        state.runtime.resolve_model(requested).await
    }
}

fn model_alias_selection(
    requested: Option<&str>,
    explicit_effort: Option<ReasoningEffort>,
) -> (Option<String>, Option<ReasoningEffort>) {
    let Some(requested) = requested else {
        return (None, explicit_effort);
    };
    let Some(without_closing_parenthesis) = requested.strip_suffix(')') else {
        return (Some(requested.to_string()), explicit_effort);
    };
    let Some((model, effort)) = without_closing_parenthesis.rsplit_once('(') else {
        return (Some(requested.to_string()), explicit_effort);
    };
    let Some(alias_effort) = parse_reasoning_effort_alias(effort.trim()) else {
        return (Some(requested.to_string()), explicit_effort);
    };
    let model = model.trim_end();
    if model.is_empty() {
        return (Some(requested.to_string()), explicit_effort);
    }

    (
        Some(model.to_string()),
        explicit_effort.or(Some(alias_effort)),
    )
}

fn parse_reasoning_effort_alias(effort: &str) -> Option<ReasoningEffort> {
    if effort.eq_ignore_ascii_case("none") {
        Some(ReasoningEffort::None)
    } else if effort.eq_ignore_ascii_case("minimal") {
        Some(ReasoningEffort::Minimal)
    } else if effort.eq_ignore_ascii_case("low") {
        Some(ReasoningEffort::Low)
    } else if effort.eq_ignore_ascii_case("medium") {
        Some(ReasoningEffort::Medium)
    } else if effort.eq_ignore_ascii_case("high") {
        Some(ReasoningEffort::High)
    } else if effort.eq_ignore_ascii_case("xhigh") {
        Some(ReasoningEffort::XHigh)
    } else if effort.eq_ignore_ascii_case("max") {
        Some(ReasoningEffort::Max)
    } else if effort.eq_ignore_ascii_case("ultra") {
        Some(ReasoningEffort::Ultra)
    } else {
        None
    }
}

fn model_ids_with_effort_aliases(models: &[ModelPreset]) -> Vec<String> {
    models
        .iter()
        .flat_map(|model| {
            std::iter::once(model.model.clone()).chain(
                model.supported_reasoning_efforts.iter().map(|preset| {
                    format!(
                        "{} ({})",
                        model.model,
                        reasoning_effort_alias_label(&preset.effort)
                    )
                }),
            )
        })
        .collect()
}

fn reasoning_effort_alias_label(effort: &ReasoningEffort) -> String {
    match effort {
        ReasoningEffort::None => "None".to_string(),
        ReasoningEffort::Minimal => "Minimal".to_string(),
        ReasoningEffort::Low => "Low".to_string(),
        ReasoningEffort::Medium => "Medium".to_string(),
        ReasoningEffort::High => "High".to_string(),
        ReasoningEffort::XHigh => "XHigh".to_string(),
        ReasoningEffort::Max => "Max".to_string(),
        ReasoningEffort::Ultra => "Ultra".to_string(),
        ReasoningEffort::Custom(effort) => effort.clone(),
    }
}

fn acquire_inference_permit(
    state: &ApiState,
) -> std::result::Result<OwnedSemaphorePermit, Response> {
    Arc::clone(&state.inference_limit)
        .try_acquire_owned()
        .map_err(|_| {
            error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "Raw API concurrency limit reached",
                None,
                Some("rate_limit_exceeded"),
            )
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenScope {
    Loopback,
    NonLoopback,
}

fn validate_server_options(
    listen: SocketAddr,
    max_concurrency: usize,
    api_token: Option<&str>,
) -> Result<ListenScope> {
    if !(1..=MAX_CONCURRENCY).contains(&max_concurrency) {
        bail!("--max-concurrency must be between 1 and {MAX_CONCURRENCY}");
    }
    if let Some(api_token) = api_token
        && (!(MIN_API_TOKEN_BYTES..=MAX_API_TOKEN_BYTES).contains(&api_token.len())
            || !api_token.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
            }))
    {
        bail!(
            "--api-token/CODEX_RAW_API_TOKEN must contain {MIN_API_TOKEN_BYTES}-{MAX_API_TOKEN_BYTES} RFC 6750 bearer-token characters"
        );
    }
    let listen_scope = if listen.ip().is_loopback() {
        ListenScope::Loopback
    } else {
        ListenScope::NonLoopback
    };
    if listen_scope == ListenScope::NonLoopback && api_token.is_none() {
        bail!(
            "refusing a non-loopback --listen address without --api-token or CODEX_RAW_API_TOKEN"
        );
    }
    Ok(listen_scope)
}

fn browser_origin_rejection(headers: &HeaderMap) -> Option<Response> {
    headers.contains_key("origin").then(|| {
        error_response(
            StatusCode::FORBIDDEN,
            "Browser-origin requests are not accepted by the Raw API server",
            None,
            Some("browser_origin_forbidden"),
        )
    })
}

fn wire_error_response(error: ApiError) -> Response {
    (error.status, Json(error.body())).into_response()
}

fn json_rejection_response(error: JsonRejection) -> Response {
    error_response(
        error.status(),
        &format!("Invalid JSON request: {error}"),
        None,
        Some("invalid_json"),
    )
}

fn upstream_error_response(error: anyhow::Error) -> Response {
    let status = error
        .downcast_ref::<CodexApiError>()
        .map(upstream_status)
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let message = match status {
        StatusCode::BAD_REQUEST => "The upstream service rejected the request",
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            "The upstream service rejected Raw authentication or permissions"
        }
        StatusCode::TOO_MANY_REQUESTS => "The upstream service rate limit was reached",
        StatusCode::SERVICE_UNAVAILABLE => "The upstream service is temporarily unavailable",
        _ => "The upstream request failed",
    };
    error_response(status, message, None, Some("upstream_error"))
}

fn upstream_status(error: &CodexApiError) -> StatusCode {
    match error {
        CodexApiError::Api { status, .. } => *status,
        CodexApiError::Transport(codex_api::TransportError::Http { status, .. }) => *status,
        CodexApiError::InvalidRequest { .. }
        | CodexApiError::ContextWindowExceeded
        | CodexApiError::CyberPolicy { .. } => StatusCode::BAD_REQUEST,
        CodexApiError::QuotaExceeded | CodexApiError::RateLimit(_) => StatusCode::TOO_MANY_REQUESTS,
        CodexApiError::Retryable { .. } | CodexApiError::ServerOverloaded => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        CodexApiError::Transport(_)
        | CodexApiError::Stream(_)
        | CodexApiError::UsageNotIncluded => StatusCode::BAD_GATEWAY,
    }
}

fn error_response(
    status: StatusCode,
    message: &str,
    param: Option<&str>,
    code: Option<&str>,
) -> Response {
    let error_type = match status {
        StatusCode::UNAUTHORIZED => "authentication_error",
        StatusCode::FORBIDDEN => "permission_error",
        StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
        status if status.is_server_error() => "server_error",
        _ => "invalid_request_error",
    };
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": error_type,
                "param": param,
                "code": code,
            }
        })),
    )
        .into_response()
}

async fn not_found() -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        "Endpoint not found",
        None,
        Some("not_found"),
    )
}

fn item_has_output_text(item: &ResponseItem) -> bool {
    matches!(
        item,
        ResponseItem::Message { content, .. }
            if content
                .iter()
                .any(|content| matches!(content, ContentItem::OutputText { .. }))
    )
}

impl ApiState {
    fn allocate_id(&self, prefix: &str) -> String {
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}_raw_{}_{sequence}", unix_millis())
    }
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    eprintln!("Raw API SIGTERM listener failed: {error}");
                    if let Err(error) = tokio::signal::ctrl_c().await {
                        eprintln!("Raw API shutdown listener failed: {error}");
                    }
                    return;
                }
            };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    eprintln!("Raw API shutdown listener failed: {error}");
                }
            }
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("Raw API shutdown listener failed: {error}");
    }
}

#[cfg(test)]
#[path = "api_server_tests.rs"]
mod tests;
