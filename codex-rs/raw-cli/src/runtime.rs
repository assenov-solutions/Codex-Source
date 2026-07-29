use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_api::ImageGenerationRequest;
use codex_api::ImageResponse;
use codex_api::ImagesClient;
use codex_api::Provider;
use codex_api::Reasoning;
use codex_api::ReasoningContext;
use codex_api::ReqwestTransport;
use codex_api::ResponseEvent;
use codex_api::ResponseStream;
use codex_api::ResponsesApiRequest;
use codex_api::ResponsesClient;
use codex_api::ResponsesOptions;
use codex_api::SharedAuthProvider;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthManager;
use codex_login::default_client;
use codex_login::token_data::parse_jwt_expiration;
use codex_model_provider::auth_provider_from_auth_manager;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::ModelsManagerConfig;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::TokenUsage;
use http::HeaderMap;
use http::HeaderValue;

use crate::ensure_chatgpt_auth;
use crate::raw_auth_manager;

const RESPONSES_LITE_HEADER: &str = "x-openai-internal-codex-responses-lite";

/// Process-wide state whose HTTP transport and model cache are reused by every Raw turn.
#[derive(Clone)]
pub(crate) struct RawRuntime {
    auth_manager: Arc<AuthManager>,
    models_manager: SharedModelsManager,
    transport: ReqwestTransport,
    api_provider: Provider,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedRawModel {
    pub(crate) slug: String,
    pub(crate) use_responses_lite: bool,
    pub(crate) context_window: Option<i64>,
}

/// A caller-owned request. Raw adds no instructions, tools, history, or metadata of its own.
#[derive(Clone, Debug)]
pub(crate) struct RawApiRequest {
    pub(crate) model: ResolvedRawModel,
    pub(crate) instructions: String,
    pub(crate) input: Vec<ResponseItem>,
    pub(crate) tools: Vec<serde_json::Value>,
    pub(crate) tool_choice: String,
    pub(crate) parallel_tool_calls: bool,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) service_tier: Option<String>,
}

#[derive(Debug)]
pub(crate) enum RawStreamEvent {
    Created,
    OutputItemAdded(ResponseItem),
    OutputTextDelta(String),
    ToolCallInputDelta {
        item_id: String,
        call_id: Option<String>,
        delta: String,
    },
    OutputItemDone(ResponseItem),
    Completed {
        token_usage: TokenUsage,
    },
}

pub(crate) struct RawResponseStream {
    inner: ResponseStream,
    completed: bool,
}

impl RawRuntime {
    pub(crate) async fn new(raw_home: &Path) -> Result<Self> {
        let auth_manager = raw_auth_manager(raw_home).await;
        let auth = auth_manager
            .auth()
            .await
            .ok_or_else(|| anyhow::anyhow!("Raw is not logged in; run `codex-raw login` first"))?;
        ensure_chatgpt_auth(&auth)?;

        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            Some(Arc::clone(&auth_manager)),
        );
        let models_manager =
            provider.models_manager(raw_home.to_path_buf(), /*model_catalog*/ None);
        let api_provider = provider.api_provider().await?;
        let transport = ReqwestTransport::from_http_client(
            default_client::create_client_without_request_logging(),
        );

        Ok(Self {
            auth_manager,
            models_manager,
            transport,
            api_provider,
        })
    }

    pub(crate) async fn resolve_model(&self, requested: Option<&str>) -> ResolvedRawModel {
        let requested = requested.map(str::to_string);
        let model = self
            .models_manager
            .get_default_model(
                &requested,
                /*allow_provider_model_fallback*/ false,
                RefreshStrategy::OnlineIfUncached,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;
        let model_info = self
            .models_manager
            .get_model_info(&model, &ModelsManagerConfig::default())
            .await;

        ResolvedRawModel {
            slug: model,
            use_responses_lite: model_info.use_responses_lite,
            context_window: model_info.context_window,
        }
    }

    pub(crate) async fn list_models(&self) -> Vec<ModelPreset> {
        self.models_manager
            .list_models(
                RefreshStrategy::OnlineIfUncached,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await
    }

    pub(crate) async fn check_auth_ready(&self) -> Result<()> {
        self.auth_manager.reload().await;
        self.refreshed_api_auth().await.map(|_| ())
    }

    pub(crate) async fn start_api_response(
        &self,
        request: RawApiRequest,
    ) -> Result<RawResponseStream> {
        let model = request.model.clone();
        let request = build_api_request(request)?;
        self.start_response(&model, request).await
    }

    pub(crate) async fn generate_image(
        &self,
        request: ImageGenerationRequest,
    ) -> Result<ImageResponse> {
        let api_auth = self.refreshed_api_auth().await?;
        ImagesClient::new(self.transport.clone(), self.api_provider.clone(), api_auth)
            .generate(&request, HeaderMap::new())
            .await
            .context("failed to generate an image through the Raw ChatGPT login")
    }

    pub(crate) async fn stream_prompt<F>(
        &self,
        model: &ResolvedRawModel,
        prompt: String,
        mut on_delta: F,
    ) -> Result<TokenUsage>
    where
        F: FnMut(&str) -> Result<()>,
    {
        if prompt.trim().is_empty() {
            bail!("prompt must not be empty");
        }

        let request = build_minimal_request(model.slug.clone(), prompt, model.use_responses_lite);
        let mut stream = self.start_response(model, request).await?;
        let mut received_delta = false;

        while let Some(event) = stream.next_event().await? {
            match event {
                RawStreamEvent::OutputTextDelta(delta) => {
                    received_delta = true;
                    on_delta(&delta)?;
                }
                RawStreamEvent::OutputItemDone(item) => {
                    if !received_delta && let Some(text) = output_text(&item) {
                        received_delta = true;
                        on_delta(&text)?;
                    }
                }
                RawStreamEvent::Completed { token_usage, .. } => {
                    return Ok(token_usage);
                }
                RawStreamEvent::Created
                | RawStreamEvent::OutputItemAdded(_)
                | RawStreamEvent::ToolCallInputDelta { .. } => {}
            }
        }

        bail!("Raw response stream ended before the completion event")
    }

    async fn start_response(
        &self,
        model: &ResolvedRawModel,
        request: ResponsesApiRequest,
    ) -> Result<RawResponseStream> {
        let api_auth = self.refreshed_api_auth().await?;
        let client =
            ResponsesClient::new(self.transport.clone(), self.api_provider.clone(), api_auth);
        let mut options = ResponsesOptions::default();
        if model.use_responses_lite {
            options
                .extra_headers
                .insert(RESPONSES_LITE_HEADER, HeaderValue::from_static("true"));
        }

        let inner = client
            .stream_request(request, options)
            .await
            .context("failed to start the Raw response stream")?;
        Ok(RawResponseStream {
            inner,
            completed: false,
        })
    }

    async fn refreshed_api_auth(&self) -> Result<SharedAuthProvider> {
        // auth() performs the guarded proactive refresh. The request auth provider then reads
        // the refreshed snapshot while remaining pinned to the same account identity.
        let auth =
            self.auth_manager.auth().await.ok_or_else(|| {
                anyhow::anyhow!("Raw is not logged in; run `codex-raw login` first")
            })?;
        ensure_chatgpt_auth(&auth)?;
        if self.auth_manager.refresh_failure_for_auth(&auth).is_some() {
            bail!("Raw authentication cannot be refreshed; run `codex-raw login` again");
        }
        let access_token = auth
            .get_token_data()
            .context("Raw ChatGPT token data is unavailable")?
            .access_token;
        let expires_at = parse_jwt_expiration(&access_token)
            .context("Raw ChatGPT access token is not a valid JWT")?
            .context("Raw ChatGPT access token has no expiration")?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_secs() as i64;
        if expires_at.timestamp() <= now {
            bail!("Raw ChatGPT access token is expired; run `codex-raw login` again");
        }
        Ok(auth_provider_from_auth_manager(
            Arc::clone(&self.auth_manager),
            &auth,
        ))
    }
}

impl RawResponseStream {
    pub(crate) async fn next_event(&mut self) -> Result<Option<RawStreamEvent>> {
        while let Some(event) = self.inner.rx_event.recv().await {
            let event = match event.context("Raw response stream failed")? {
                ResponseEvent::Created => RawStreamEvent::Created,
                ResponseEvent::OutputItemAdded(item) => RawStreamEvent::OutputItemAdded(item),
                ResponseEvent::OutputTextDelta(delta) => RawStreamEvent::OutputTextDelta(delta),
                ResponseEvent::ToolCallInputDelta {
                    item_id,
                    call_id,
                    delta,
                } => RawStreamEvent::ToolCallInputDelta {
                    item_id,
                    call_id,
                    delta,
                },
                ResponseEvent::OutputItemDone(item) => RawStreamEvent::OutputItemDone(item),
                ResponseEvent::Completed { token_usage, .. } => {
                    self.completed = true;
                    RawStreamEvent::Completed {
                        token_usage: token_usage
                            .context("Raw response completed without token usage")?,
                    }
                }
                ResponseEvent::SafetyBuffering(_)
                | ResponseEvent::ServerModel(_)
                | ResponseEvent::ModelVerifications(_)
                | ResponseEvent::TurnModerationMetadata(_)
                | ResponseEvent::ServerReasoningIncluded(_)
                | ResponseEvent::ReasoningSummaryDelta { .. }
                | ResponseEvent::ReasoningSummaryDone { .. }
                | ResponseEvent::ReasoningContentDelta { .. }
                | ResponseEvent::ReasoningSummaryPartAdded { .. }
                | ResponseEvent::RateLimits(_)
                | ResponseEvent::ModelsEtag(_) => continue,
            };
            return Ok(Some(event));
        }

        if self.completed {
            Ok(None)
        } else {
            bail!("Raw response stream ended before the completion event")
        }
    }
}

pub(crate) fn build_api_request(mut request: RawApiRequest) -> Result<ResponsesApiRequest> {
    let use_responses_lite = request.model.use_responses_lite;
    if use_responses_lite {
        for item in &mut request.input {
            if let ResponseItem::Message { content, .. } = item {
                for content_item in content {
                    if let ContentItem::InputImage { detail, .. } = content_item {
                        *detail = None;
                    }
                }
            }
        }
    }
    let has_tools = !request.tools.is_empty();
    let tools = if use_responses_lite {
        if has_tools {
            request.input.insert(
                0,
                ResponseItem::AdditionalTools {
                    id: None,
                    role: "developer".to_string(),
                    tools: request.tools,
                },
            );
        }
        None
    } else {
        if has_tools {
            let raw_tools: Arc<serde_json::value::RawValue> = Arc::from(
                serde_json::value::to_raw_value(&request.tools)
                    .context("failed to serialize Raw function tools")?,
            );
            Some(raw_tools.into())
        } else {
            None
        }
    };
    let instructions = if use_responses_lite {
        if !request.instructions.is_empty() {
            request.input.insert(
                usize::from(has_tools),
                ResponseItem::Message {
                    id: None,
                    role: "developer".to_string(),
                    content: vec![ContentItem::InputText {
                        text: request.instructions,
                    }],
                    phase: None,
                    internal_chat_message_metadata_passthrough: None,
                },
            );
        }
        String::new()
    } else {
        request.instructions
    };

    Ok(ResponsesApiRequest {
        model: request.model.slug,
        instructions,
        input: request.input,
        tools,
        tool_choice: request.tool_choice,
        parallel_tool_calls: request.parallel_tool_calls && !use_responses_lite,
        reasoning: (use_responses_lite || request.reasoning_effort.is_some()).then_some(
            Reasoning {
                effort: request.reasoning_effort,
                summary: None,
                context: use_responses_lite.then_some(ReasoningContext::AllTurns),
            },
        ),
        store: false,
        stream: true,
        stream_options: None,
        include: if has_tools {
            vec!["reasoning.encrypted_content".to_string()]
        } else {
            Default::default()
        },
        service_tier: request.service_tier,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    })
}

pub(crate) fn build_minimal_request(
    model: String,
    prompt: String,
    use_responses_lite: bool,
) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model,
        instructions: String::new(),
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: prompt }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        tools: None,
        tool_choice: "none".to_string(),
        parallel_tool_calls: false,
        reasoning: use_responses_lite.then_some(Reasoning {
            effort: None,
            summary: None,
            context: Some(ReasoningContext::AllTurns),
        }),
        store: false,
        stream: true,
        stream_options: None,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}

fn output_text(item: &ResponseItem) -> Option<String> {
    let ResponseItem::Message { content, .. } = item else {
        return None;
    };
    let text = content
        .iter()
        .filter_map(|item| match item {
            ContentItem::OutputText { text } => Some(text.as_str()),
            ContentItem::InputText { .. }
            | ContentItem::InputImage { .. }
            | ContentItem::InputAudio { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}
