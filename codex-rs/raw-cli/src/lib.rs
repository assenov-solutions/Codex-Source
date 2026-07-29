use std::env;
use std::ffi::OsString;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Parser;
use clap::Subcommand;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::AuthRouteConfig;
use codex_login::CLIENT_ID;
use codex_login::CodexAuth;
use codex_login::ServerOptions;
use codex_login::logout_with_revoke;
use codex_login::run_login_server;
use codex_protocol::auth::AuthMode;

use crate::runtime::RawRuntime;

mod api_server;
mod api_server_chat_stream;
mod api_server_chat_wire;
mod api_server_http_stream;
mod api_server_image_wire;
mod api_server_response;
mod api_server_stream;
mod api_server_vision;
mod api_server_wire;
mod api_server_wire_validation;
mod app_server;
mod app_server_wire;
mod runtime;
mod update;

const RAW_HOME_DIR_NAME: &str = ".codex-raw";
const RAW_HOME_MARKER: &str = ".codex-raw-home";
const RAW_HOME_MARKER_CONTENT: &str = "codex-raw-home-v1\n";
const PROTECTED_CODEX_HOME_DIR_NAMES: [&str; 3] = [".codex", ".codex2", ".codex-2"];
const RAW_AFTER_HELP: &str = r#"Examples:
  codex-raw login
  codex-raw status
  codex-raw update --check
  codex-raw update
  codex-raw -m gpt-5.4-mini send "Explain this error"
  codex-raw api-server --listen 127.0.0.1:8080
  codex-raw api-server --listen 0.0.0.0:8080 --api-token TOKEN

Run `codex-raw api-server --help` for endpoints, JSON settings, image generation,
reasoning controls, streaming behavior, and network security notes."#;
const API_SERVER_AFTER_HELP: &str = r#"HTTP endpoints:
  GET  /healthz
  GET  /readyz
  GET  /v1/models
  POST /v1/responses
  POST /v1/chat/completions
  POST /v1/images/generations

Responses JSON settings:
  model, input, instructions, tools (function only), tool_choice
  parallel_tool_calls, reasoning.effort, service_tier, stream, store=false
  input accepts a string or supported text/reasoning/function-call item array.
  Known reasoning efforts: none, minimal, low, medium, high, xhigh, max, ultra.
  tool_choice accepts none, auto, or required; its default is auto with tools,
  otherwise none. Responses Lite forces parallel_tool_calls=false. Raw returns
  tool calls but never executes them. Effort/tier support is model-dependent.

Chat Completions JSON settings:
  model, messages, tools (function only), tool_choice, parallel_tool_calls
  reasoning_effort, service_tier, stream, stream_options.include_usage, n=1,
  store=false. Messages may carry text and function-tool loop items.

Image generation JSON settings:
  prompt (required), model (default: gpt-image-2), n (1-4)
  quality (default auto; low, medium, high)
  size (default auto or a model-supported WIDTHxHEIGHT; validated upstream)
  gpt-image-2 requested size: edges multiple of 16, max 3840, aspect at most
  3:1, and 655360-8294400 total pixels.
  background (default auto; opaque; transparent on supporting models only)
  response_format=b64_json. gpt-image-2 has no native transparent background.
  Images use the isolated ChatGPT subscription login and return base64 image data.
  The backend can resolve a different size; response metadata reports the result.

Examples:
  Text:  POST /v1/responses {"model":"gpt-5.4-mini","input":"hello"}
  Image: POST /v1/images/generations {"prompt":"a glass city at dawn"}

Raw is stateless and never executes caller-defined function tools. Unsupported
non-null settings are rejected, including model_verbosity, temperature, top_p,
output-token limits, reasoning summaries, store=true, and previous_response_id.

Security:
  The safe default is loopback. Non-loopback binds require --api-token or
  CODEX_RAW_API_TOKEN. Prefer the environment variable so the token is not
  exposed in shell history or process arguments. Tokens must contain 32-512
  RFC 6750 bearer-token characters. When configured, Bearer auth is required
  by /readyz and every /v1 endpoint; /healthz remains public.
  For network traffic, use a hardened reverse proxy that enforces TLS,
  connection/header timeouts, and connection limits.
  Without a configured token on loopback, SDKs may use api_key="raw-local" as
  a dummy value; never use a real OpenAI API key for that compatibility case."#;

/// Minimal Codex client that keeps its state separate from all regular Codex installations.
#[derive(Debug, Parser)]
#[command(
    name = "codex-raw",
    version,
    about = "Minimal, isolated ChatGPT-subscription client and stateless API server",
    subcommand_negates_reqs = true,
    after_help = RAW_AFTER_HELP
)]
struct RawCli {
    /// Raw-only state directory. Defaults to ~/.codex-raw.
    #[arg(long, global = true, env = "CODEX_RAW_HOME", value_name = "DIR")]
    home: Option<PathBuf>,

    /// Default text model. When omitted, use the active catalog default.
    #[arg(long, short = 'm', global = true, value_name = "MODEL")]
    model: Option<String>,

    #[command(subcommand)]
    command: RawCommand,
}

#[derive(Debug, Subcommand)]
enum RawCommand {
    /// Sign in with ChatGPT and store credentials only under the Raw home.
    Login,

    /// Remove only the Raw login credentials.
    Logout,

    /// Show the Raw login status.
    Status,

    /// Check for or install the latest verified stable Codex Raw release.
    Update {
        /// Only report whether an update is available; do not write files or stop services.
        #[arg(long)]
        check: bool,
    },

    /// Send a minimal, stateless prompt.
    Send {
        #[arg(required = true, value_name = "PROMPT")]
        prompt: Vec<String>,
    },

    /// Run a persistent, minimal JSONL app-server over stdin/stdout.
    AppServer,

    /// Run an OpenAI-compatible HTTP API.
    #[command(after_help = API_SERVER_AFTER_HELP)]
    ApiServer {
        /// Address to listen on. Non-loopback addresses require an API token.
        #[arg(long, default_value = "127.0.0.1:8080", value_name = "IP:PORT")]
        listen: SocketAddr,

        /// Maximum simultaneous model requests, from 1 through 256.
        #[arg(long, default_value_t = 16, value_name = "COUNT")]
        max_concurrency: usize,

        /// Bearer token (32-512 characters) for /readyz and /v1. Prefer CODEX_RAW_API_TOKEN.
        #[arg(
            long,
            env = "CODEX_RAW_API_TOKEN",
            hide_env_values = true,
            value_name = "TOKEN"
        )]
        api_token: Option<String>,
    },

    /// Treat an unrecognized command as a prompt, so `codex-raw hello` works.
    #[command(external_subcommand)]
    Prompt(Vec<OsString>),
}

pub async fn run() -> Result<()> {
    run_with_args(env::args_os()).await
}

async fn run_with_args(args: impl IntoIterator<Item = OsString>) -> Result<()> {
    let cli = RawCli::parse_from(args);
    let raw_home = prepare_command_raw_home(&cli.command, cli.home)?;

    match cli.command {
        RawCommand::Update { check } => {
            let mode = if check {
                update::UpdateMode::CheckOnly
            } else {
                update::UpdateMode::Install
            };
            update::run(mode).await
        }
        RawCommand::Login => login(raw_home.as_deref().context("Raw home was not prepared")?).await,
        RawCommand::Logout => {
            logout(raw_home.as_deref().context("Raw home was not prepared")?).await
        }
        RawCommand::Status => {
            status(raw_home.as_deref().context("Raw home was not prepared")?).await
        }
        RawCommand::Send { prompt } => {
            send(
                raw_home.as_deref().context("Raw home was not prepared")?,
                cli.model,
                prompt.join(" "),
            )
            .await
        }
        RawCommand::AppServer => {
            app_server::run(
                raw_home.as_deref().context("Raw home was not prepared")?,
                cli.model,
            )
            .await
        }
        RawCommand::ApiServer {
            listen,
            max_concurrency,
            api_token,
        } => {
            api_server::run(
                raw_home.as_deref().context("Raw home was not prepared")?,
                cli.model,
                api_server::ApiServerOptions {
                    listen,
                    max_concurrency,
                    api_token,
                },
            )
            .await
        }
        RawCommand::Prompt(prompt) => {
            let prompt = prompt
                .into_iter()
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            send(
                raw_home.as_deref().context("Raw home was not prepared")?,
                cli.model,
                prompt,
            )
            .await
        }
    }
}

fn prepare_command_raw_home(
    command: &RawCommand,
    requested: Option<PathBuf>,
) -> Result<Option<PathBuf>> {
    if matches!(command, RawCommand::Update { .. }) {
        Ok(None)
    } else {
        prepare_raw_home(requested).map(Some)
    }
}

async fn login(raw_home: &Path) -> Result<()> {
    let options = ServerOptions::new(
        raw_home.to_path_buf(),
        CLIENT_ID.to_string(),
        /*forced_chatgpt_workspace_id*/ None,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
        raw_auth_route_config(),
    );
    let server = run_login_server(options).context("failed to start the ChatGPT login server")?;

    eprintln!(
        "Raw login server: http://localhost:{}\nOpen this URL if the browser does not start:\n\n{}\n",
        server.actual_port, server.auth_url
    );
    server
        .block_until_done()
        .await
        .context("Raw ChatGPT login did not complete")?;
    eprintln!(
        "Raw login completed. Credentials are isolated in {}",
        raw_home.display()
    );
    Ok(())
}

async fn logout(raw_home: &Path) -> Result<()> {
    let auth_route_config = raw_auth_route_config();
    let removed = logout_with_revoke(
        raw_home,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
        &auth_route_config,
    )
    .await
    .context("failed to remove the isolated Raw login")?;

    if removed {
        eprintln!("Raw login removed. Regular Codex logins were not changed.");
    } else {
        eprintln!("Raw is not logged in.");
    }
    Ok(())
}

async fn status(raw_home: &Path) -> Result<()> {
    let auth_manager = raw_auth_manager(raw_home).await;
    match auth_manager.auth().await {
        Some(auth) => eprintln!(
            "Raw is logged in using {}. Home: {}",
            auth_mode_label(auth.auth_mode()),
            raw_home.display()
        ),
        None => eprintln!("Raw is not logged in. Home: {}", raw_home.display()),
    }
    Ok(())
}

async fn send(raw_home: &Path, model: Option<String>, prompt: String) -> Result<()> {
    let runtime = RawRuntime::new(raw_home).await?;
    let model = runtime.resolve_model(model.as_deref()).await;
    let mut wrote_text = false;
    let usage = runtime
        .stream_prompt(&model, prompt, |delta| {
            wrote_text = true;
            print!("{delta}");
            std::io::stdout().flush()?;
            Ok(())
        })
        .await?;
    if wrote_text {
        println!();
    }
    eprintln!("[usage] {}", serde_json::to_string(&usage)?);
    Ok(())
}

async fn raw_auth_manager(raw_home: &Path) -> Arc<AuthManager> {
    AuthManager::shared(
        raw_home.to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        raw_auth_route_config(),
    )
    .await
}

fn raw_auth_route_config() -> AuthRouteConfig {
    AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
        OutboundProxyPolicy::ReqwestDefault,
    ))
}

pub(crate) fn ensure_chatgpt_auth(auth: &CodexAuth) -> Result<()> {
    match auth.auth_mode() {
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => Ok(()),
        AuthMode::ApiKey
        | AuthMode::Headers
        | AuthMode::AgentIdentity
        | AuthMode::PersonalAccessToken
        | AuthMode::BedrockApiKey => {
            bail!("codex-raw requires its own ChatGPT login; run `codex-raw login`")
        }
    }
}

fn auth_mode_label(mode: AuthMode) -> &'static str {
    match mode {
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => "ChatGPT",
        AuthMode::ApiKey => "an API key",
        AuthMode::Headers => "HTTP headers",
        AuthMode::AgentIdentity => "an access token",
        AuthMode::PersonalAccessToken => "a personal access token",
        AuthMode::BedrockApiKey => "an Amazon Bedrock API key",
    }
}

fn prepare_raw_home(requested: Option<PathBuf>) -> Result<PathBuf> {
    let user_home = user_home_dir()?;
    let raw_home =
        normalize_absolute(requested.unwrap_or_else(|| user_home.join(RAW_HOME_DIR_NAME)))?;
    let mut protected_homes = PROTECTED_CODEX_HOME_DIR_NAMES
        .iter()
        .map(|name| user_home.join(name))
        .collect::<Vec<_>>();
    if let Some(codex_home) = env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
        protected_homes.push(PathBuf::from(codex_home));
    }
    ensure_not_protected(&raw_home, &protected_homes)?;
    claim_raw_home(&raw_home)?;
    Ok(raw_home)
}

fn user_home_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    let home = env::var_os("USERPROFILE").or_else(|| {
        let drive = env::var_os("HOMEDRIVE")?;
        let path = env::var_os("HOMEPATH")?;
        let mut home = PathBuf::from(drive);
        home.push(path);
        Some(home.into_os_string())
    });

    #[cfg(not(windows))]
    let home = env::var_os("HOME");

    home.map(PathBuf::from)
        .context("could not determine the user home directory for codex-raw")
}

fn normalize_absolute(path: PathBuf) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path
    } else {
        env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

fn ensure_not_protected(raw_home: &Path, protected_homes: &[PathBuf]) -> Result<()> {
    for protected_home in protected_homes {
        let protected_home = normalize_absolute(protected_home.clone())?;
        if raw_home == protected_home || raw_home.starts_with(&protected_home) {
            bail!(
                "refusing to use protected Codex directory {} as the Raw home",
                raw_home.display()
            );
        }
    }
    Ok(())
}

fn claim_raw_home(raw_home: &Path) -> Result<()> {
    let marker = raw_home.join(RAW_HOME_MARKER);
    if raw_home.exists() {
        if !raw_home.is_dir() {
            bail!("Raw home is not a directory: {}", raw_home.display());
        }
        let content = fs::read_to_string(&marker).with_context(|| {
            format!(
                "refusing to use existing directory without a valid Raw marker: {}",
                raw_home.display()
            )
        })?;
        if content != RAW_HOME_MARKER_CONTENT {
            bail!("invalid Raw home marker in {}", raw_home.display());
        }
        return Ok(());
    }

    fs::create_dir(raw_home)
        .with_context(|| format!("failed to create Raw home {}", raw_home.display()))?;
    let mut marker_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
        .with_context(|| format!("failed to claim Raw home {}", raw_home.display()))?;
    marker_file.write_all(RAW_HOME_MARKER_CONTENT.as_bytes())?;
    Ok(())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
