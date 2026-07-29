# `codex-raw` implementation notes

`codex-raw` is an isolated Rust crate that provides a minimal, stateless model
client without entering the standard Codex agent/session pipeline. This file
documents the implementation boundaries that maintainers must preserve.

> [!NOTE]
> This is the maintainer and implementation reference. New users should start
> with the [project overview](../../README.md), then use the
> [installation guide](../../INSTALL.md) and
> [interface guide](../../API.md).

## Documentation contract

Changes to commands, endpoints, wire fields, defaults, model controls,
authentication, update behavior, or network security must update this file,
the applicable public guide (`README.md`, `INSTALL.md`, or `API.md`), and the
rendered help in `src/lib.rs` before the change series is published. Verify
`codex-raw --help`, `codex-raw update --help`, and
`codex-raw api-server --help` before publishing a build.

## Core invariants

Every change to this crate should preserve these properties:

1. The executable is named `codex-raw`; it never replaces the standard
   `codex` executable.
2. Runtime state is stored in a dedicated Raw home, `~/.codex-raw` by default.
3. Only the Raw ChatGPT login is accepted. Other auth modes are rejected.
4. One-shot turns contain exactly one caller-supplied user message.
5. One-shot turns add no Codex instructions, tools, history, or metadata.
6. API turns contain only the instructions, items, and function definitions
   explicitly supplied by the HTTP caller.
7. Requests are stateless: `store` is false and hidden server-side conversation
   continuation is not used.
8. The HTTP server defaults to loopback, requires bearer authentication for a
   non-loopback bind, and has bounded concurrency.
9. The app-server and HTTP server share the same auth/model/transport runtime.
10. Exact upstream text token usage is surfaced to text-response callers.

## Source map

| File                                | Responsibility                                                                                                        |
| ----------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `src/lib.rs`                        | CLI parsing, command dispatch, login, logout, status, and Raw home ownership                                          |
| `src/update/`                       | Verified GitHub release lookup, bounded archive extraction, platform replacement, service health checks, and rollback |
| `src/runtime.rs`                    | Shared auth manager, model manager, text/image transport, request construction, and stream normalization              |
| `src/app_server.rs`                 | JSONL process lifecycle, thread handles, and sequential turns                                                         |
| `src/app_server_wire.rs`            | Small app-server parsers and response/notification builders                                                           |
| `src/api_server.rs`                 | Axum router, listen-scope warning, concurrency, and request execution                                                 |
| `src/api_server_wire.rs`            | Responses request parsing and public wire types                                                                       |
| `src/api_server_chat_wire.rs`       | Chat message and tool translation into Responses items                                                                |
| `src/api_server_image_wire.rs`      | Image generation request validation and response mapping                                                              |
| `src/api_server_response.rs`        | Non-streaming Responses and Chat result mapping                                                                       |
| `src/api_server_stream.rs`          | Typed Responses SSE lifecycle                                                                                         |
| `src/api_server_chat_stream.rs`     | Chat Completions chunks, usage, and `[DONE]`                                                                          |
| `src/api_server_http_stream.rs`     | Upstream event adaptation for HTTP streaming                                                                          |
| `src/api_server_wire_validation.rs` | Function-call and function-output correlation                                                                         |
| `src/*_tests.rs`                    | Unit, wire, response, and streaming tests                                                                             |
| `scripts/openai_sdk_smoke.py`       | Live OpenAI Python SDK compatibility check                                                                            |
| `scripts/benchmark_persistent.ps1`  | Paired persistent app-server/API latency benchmark                                                                    |

## CLI dispatch

`RawCli` defines the global options:

- `--home DIR`, also available through `CODEX_RAW_HOME`;
- `--model MODEL` or `-m MODEL`.

The explicit subcommands are `login`, `logout`, `status`, `update`, `send`,
`app-server`, and `api-server`. Clap external-subcommand handling converts any
unknown command into prompt text, allowing `codex-raw hello` to behave like
`codex-raw send hello`.

All prompt arguments are joined with a single space. Empty prompts are rejected
before a model request starts.

Typical commands:

```powershell
codex-raw login
codex-raw status
codex-raw update --check
codex-raw update
codex-raw -m gpt-5.4-mini send "Explain Tokio in one paragraph"
codex-raw api-server --listen 127.0.0.1:8080 --max-concurrency 16
```

`api-server` accepts `--api-token TOKEN` or `CODEX_RAW_API_TOKEN`. Prefer the
environment variable because a CLI argument can be exposed in shell history and
process listings. Tokens must contain 32-512 RFC 6750 bearer-token characters;
use a cryptographically random value (the public API guide includes generation
commands). Raw never prints the configured token.

`--model` is the default text model for one-shot, app-server, Responses, and
Chat requests. A per-request `model` overrides it. Image generation has its own
request model and defaults to `gpt-image-2`.

## Verified binary updates

`codex-raw update --check` queries the latest stable release without writing
files, creating a Raw home, or touching a service. `codex-raw update` installs a
newer release for Windows x64, Linux x64, Linux ARM64, or macOS Intel.

The updater is pinned to `bproject07/Codex-Source`. It accepts only a canonical
`codex-raw-v<semver>` release that the current GitHub API reports as stable,
published, and immutable. The platform archive and `SHA256SUMS` must each be
the single exact expected asset with an exact repository download URL and a
GitHub SHA-256 digest. The downloaded archive must match both its GitHub digest
and its exact `SHA256SUMS` entry. Extraction reads only the expected regular
binary at the expected bundle path; it never unpacks arbitrary archive paths.
An equal SemVer precedence is reported as already up to date and is not
reinstalled; a release older than the running binary is rejected.

The candidate binary is staged in the detected installation layout, made
executable where applicable, and required to report the release version before
replacement. Standalone installs retain a timestamped hidden backup; managed
Unix installs retain the previous release directory.

On Windows, the running executable cannot replace itself. Raw launches a
detached helper, waits for it to open the shared lock file, releases its own
byte-range lock, and exits only after the helper proves ownership of that exact
range. The helper uses Restart Manager to request graceful shutdown only from
processes whose identity and full image path match the exact installed
`codex-raw.exe`. It never force-kills a target Raw process or kills by name.
The updater helper's lifecycle waits are bounded and failures are written to
its status file. If replacement occurred and the new binary fails its version
check, the helper attempts rollback. After a successful version check, Restart
Manager attempts to restart eligible registered application processes; a
restart failure can leave the verified new binary installed and is reported.

On Linux, use `sudo codex-raw update` for a root-owned installation. If
`codex-raw-api.service` is active, Raw verifies that its `MainPID` is executing
the exact binary being updated and that its command line contains
`api-server`; a mismatch aborts the update. Raw requests `systemctl stop` and
waits for the unit to become fully stopped, then installs the candidate and
starts the same unit. If startup or verification fails, Raw attempts
best-effort rollback only after safely stopping the failed service. If that
safe stop fails, rollback is not attempted and recovery errors are reported.
Raw does not kill processes by name. When the fixed unit is inactive, other
running Unix processes are left alone and must be restarted manually.

Repository owners must enable GitHub immutable releases. A mutable release is
deliberately unusable by this updater.

The current crate includes this updater. A build without the `update` command
requires one manual verified replacement before it can self-update. The GitHub
fetch path can be exercised end to end only against a published immutable
release with all exact assets; local selection, archive, replacement, and
rollback tests do not substitute for that live check.

## Raw home ownership

`prepare_raw_home` resolves the requested directory or defaults to
`~/.codex-raw`. It then:

1. normalizes the path lexically;
2. rejects known Codex state directories and descendants;
3. rejects the configured `CODEX_HOME` and descendants;
4. creates a new directory when necessary;
5. writes `.codex-raw-home` with the versioned marker
   `codex-raw-home-v1`;
6. refuses an existing directory without the exact marker.

The guard does not canonicalize symbolic-link or junction targets. Users must
not use links to point a Raw home at another state directory.

The marker prevents an accidental custom `--home` from claiming an unrelated
existing directory. Credentials are stored in `auth.json` beneath the claimed
Raw home and must never be committed.

## Authentication lifecycle

`raw_auth_manager` constructs the shared Codex `AuthManager` with:

- file-backed credentials;
- API-key environment auth disabled;
- no forced workspace;
- no custom ChatGPT base URL;
- no custom auth route configuration.

`ensure_chatgpt_auth` accepts only ChatGPT auth modes. Header, API-key, agent
identity, personal access token, and Bedrock modes are rejected.

The login command uses the Codex local callback server and stores the resulting
credentials only in the Raw home. Logout revokes the refresh token when
possible and removes only the Raw credential file.

`AuthManager::auth()` is called before status and before each upstream request.
That call performs guarded proactive refresh when necessary. Persistent
processes therefore do not remain pinned to the access token present at
startup.

## Shared runtime

`RawRuntime` owns process-wide reusable state:

```text
AuthManager
ModelsManager
ReqwestTransport
API Provider
```

One-shot mode creates one runtime per process. App-server and API-server create
one runtime and reuse it across sequential or concurrent turns.

`resolve_model` uses an explicit model when supplied; otherwise it asks the
model manager for the active catalog default. It also records whether the model
uses Responses Lite and its known context window.

## Minimal one-shot request

`build_minimal_request` constructs a `ResponsesApiRequest` with:

```text
instructions        empty
input               one user message with one input_text item
tools               none
tool_choice         none
parallel_tool_calls false
store               false
stream              true
service_tier        none
client_metadata     none
```

For a Responses Lite model, the request also contains the required reasoning
context and the transport supplies the Responses Lite header. These fields are
protocol requirements, not Codex agent instructions.

`stream_prompt` forwards text deltas immediately. If the upstream emits only a
completed message item, it extracts that output text as a fallback. A response
without a completion event or token usage is treated as an error.

## Caller-owned API request

`RawApiRequest` contains only data supplied by the HTTP translator:

- resolved model metadata;
- instructions;
- input items;
- function tools;
- tool choice;
- parallel-tool setting;
- reasoning effort;
- service tier.

For standard Responses transport, those fields map directly to the upstream
request. For Responses Lite, function definitions and caller instructions are
inserted as explicit developer input items because the Lite transport does not
use the standard top-level fields, and unsupported image detail settings are
removed. If tools are present, encrypted reasoning is included so a stateless
client can replay the full output in the next request.

Raw never adds its own instructions or tools to this request.

## App-server protocol

The app-server is a line-delimited JSON protocol over stdin/stdout. It supports:

- `initialize`;
- `initialized`;
- `thread/start`;
- `turn/start`;
- numeric and string request IDs;
- an optional model override at thread or turn start;
- exactly one non-empty text input per turn;
- turn, item, text-delta, usage, completion, and error notifications.

The wire shape follows the relevant Codex app-server v2 structures but does not
implement the complete protocol. There is no persistence, resume, interrupt,
tool approval, multimodal input, or parallel turn execution.

The server responds to `turn/start` before emitting notifications. Upstream
failures produce an error notification and a terminal failed
`turn/completed` notification without terminating the process.

Protocol stdout must contain JSONL only. Diagnostics belong on stderr, and each
JSON line must be flushed immediately.

## HTTP API

The API server uses Axum and exposes:

```text
GET  /healthz
GET  /readyz
GET  /v1/models
POST /v1/responses
POST /v1/chat/completions
POST /v1/images/generations
```

Startup accepts a concurrency limit from 1 through 256 and rejects bearer
tokens outside the documented length/alphabet. The safe listen default is
`127.0.0.1:8080`; loopback may run without a token for
OpenAI SDK dummy-key compatibility. `--listen 0.0.0.0:8080` binds all IPv4
interfaces, while `--listen [::]:8080` binds IPv6 interfaces and may also accept
IPv4 depending on the OS. Raw refuses either non-loopback bind unless
`--api-token` or `CODEX_RAW_API_TOKEN` is configured. Remote clients must replace
the wildcard address with the machine's real LAN IP or DNS name. The request
body limit is 32 MiB. A non-blocking semaphore returns HTTP 429 when all inference
slots are occupied.

When a token is configured, `Authorization: Bearer TOKEN` is required for
`/readyz` and the entire `/v1` namespace, including unknown `/v1` paths.
`/healthz` intentionally remains open as a process liveness probe. `/readyz`
performs a fresh runtime auth check (including the normal guarded token refresh)
and returns HTTP 503 with `{"status":"not_ready"}` when the isolated Raw login is
not currently usable.

Inference POST requests carrying a browser `Origin` header are rejected. This
reduces the risk of an arbitrary web page calling the local service, but it is
not a substitute for bearer authentication and network isolation. The built-in
server uses plain HTTP, so a bearer token is not protected in transit. For
network access, use a hardened reverse proxy that enforces TLS,
connection/header timeouts, and connection limits. Firewall the Raw port so
only that proxy or other explicitly trusted clients can reach it.

All POST routes use strict parsers. Unsupported non-null fields return an
OpenAI-shaped error, except for the documented model-generation compatibility
controls that are explicitly accepted as no-ops.

`GET /v1/models` returns the current text model catalog, aliases for advertised
reasoning efforts such as `gpt-model (High)`, and the dedicated `gpt-image-2`
image model. Supplying an effort alias selects the base model and effort; an
explicit effort field takes precedence. Listing a model does not make it valid
on every endpoint: use `gpt-image-2` through `/v1/images/generations`, not as a
text model.

### Supported request settings

Raw deliberately implements a small, explicit subset of the OpenAI wire
formats. Model/account capabilities still decide whether a forwarded setting is
accepted upstream.

| Endpoint                 | Supported fields and behavior                                                                                                                                                                                                                                                                                                                                           |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `/v1/responses`          | `model`; required `input` string or supported text/user-image/reasoning/function-call item array; `instructions`; function `tools`; `tool_choice` = `none`, `auto`, or `required`; `parallel_tool_calls` (default `true`); `reasoning.effort`; non-empty `service_tier`; `stream` (default `false`); `store` omitted, `null`, or `false`                                        |
| `/v1/chat/completions`   | `model`; non-empty text/user-image/function-tool-loop `messages`; function `tools`; `tool_choice` = `none`, `auto`, or `required`; `parallel_tool_calls` (default `true`); `reasoning_effort`; non-empty `service_tier`; `stream` (default `false`); `stream_options.include_usage` (default `false`); `n` omitted or `1`; `store` omitted, `null`, or `false`                  |
| `/v1/images/generations` | required non-empty `prompt`; `model` (default `gpt-image-2`); `n` from 1 to 4; `quality` = `auto` (default), `low`, `medium`, or `high`; non-empty model-supported `size` such as `auto` (default), `1024x1024`, `1536x1024`, or `1024x1536`; `background` = `auto` (default), `opaque`, or `transparent` on supporting models; `response_format` omitted or `b64_json` |

Known reasoning effort spellings are `none`, `minimal`, `low`, `medium`,
`high`, `xhigh`, `max`, and `ultra`. The parser also forwards other non-empty
effort names for forward compatibility. Not every model accepts every effort.
When omitted, Raw does not invent a reasoning effort; the upstream model default
applies. `service_tier` is likewise an upstream value and is not silently
defaulted by Raw.

`tool_choice` defaults to `auto` when non-empty function tools are supplied and
to `none` otherwise.

The text routes are stateless. `store` is always false, and callers must replay
the prior output and any matching function result on a subsequent tool-loop
request. Raw transports function definitions and calls but does not execute
them. The Responses route is preferred for reasoning-model tool loops because
Chat Completions cannot faithfully replay opaque encrypted reasoning items.
For Responses Lite models, Raw accepts `parallel_tool_calls` but sends it as
false because that transport packs caller tools into developer input items.

User messages may include at most 10 validated image inputs. Responses accepts
`input_image` content and Chat Completions accepts `image_url` content. Each
URL must use HTTPS or a base64 PNG, JPEG, WEBP, or GIF data URL; remote URLs
are capped at 8 KiB and data URLs at 3 MiB. Responses Lite omits image
`detail`, which that transport does not support.

For SDK compatibility, the text routes accept and discard these controls:
`frequency_penalty`, `logprobs`, `max_completion_tokens`,
`max_output_tokens`, `max_tokens`, `presence_penalty`, `response_format`,
`seed`, `stop`, `temperature`, `top_logprobs`, and `top_p`. Other unsupported
non-null fields are rejected, including `model_verbosity`/`text.verbosity`,
`reasoning.summary`, `background` text jobs, stateful `store: true`,
`previous_response_id`, named/object `tool_choice`, and built-in
web/shell/MCP tools.

Although the public HTTP response can be non-streaming, the ChatGPT backend
transport itself uses a stream and Raw collects it when `stream` is false.

OpenAI SDK constructors require an API-key string. When Raw is running on
loopback without a configured API token, set a dummy value such as `raw-local`;
never put a real OpenAI API key in a Raw client. When Raw bearer authentication
is enabled, pass the configured Raw token as the SDK `api_key` instead. For the
unauthenticated loopback compatibility case:

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:8080/v1",
    api_key="raw-local",  # Dummy SDK requirement, not server authentication.
)
```

### Responses mapping

The Responses route supports text messages, reasoning items, function calls,
and matching function-call outputs. It produces either a complete Responses
object or a typed SSE lifecycle:

```text
response.created
response.in_progress
response.output_item.added
response.content_part.added
response.output_text.delta
response.output_text.done
response.content_part.done
response.output_item.done
response.completed
```

Function calls also produce argument delta and done events. When the upstream
parser exposes only completed arguments, Raw emits one synthesized complete
arguments delta before the done event.

### Chat Completions mapping

Text and function-tool-loop Chat messages are translated to Responses input
items. Function tool definitions are flattened to the upstream function format.
Non-streaming results map back to Chat choices and usage; streams emit role,
text, tool-call, finish, optional usage chunks, and a final `[DONE]` marker.

The Chat format cannot faithfully replay opaque encrypted reasoning items.
Reasoning-model tool loops should therefore use the Responses route.

### Image generation mapping

The image route calls the existing Codex image transport directly with the
isolated Raw ChatGPT login. It does not ask a text model to call a tool and does
not require an API key. Its OpenAI-shaped response contains `created`, optional
resolved `background`, `quality`, and `size`, plus:

```json
{
  "data": [{ "b64_json": "..." }]
}
```

`gpt-image-2` is the default and subscription-backed model used by this source
tree. A different non-empty image model ID is forwarded, but the ChatGPT backend
may reject models unavailable to the logged-in account. Image edits are not
exposed by this minimal route. Treat `size`, `quality`, and `background` as
upstream controls and inspect the returned metadata for the resolved result; the
subscription backend can return a different output size.

`gpt-image-2` does not support native `background=transparent`; that setting is
only for a model that supports transparency. Its explicit requested dimensions
must have both edges divisible by 16, a maximum edge of 3840 pixels, an aspect
ratio no greater than 3:1, and 655,360 to 8,294,400 total pixels. Raw forwards
non-empty sizes and lets the backend validate them. The current low-level
`ImageResponse` retains the base64 image and resolved metadata, but not upstream
image usage or `output_format`.

PowerShell example that generates and saves a PNG:

```powershell
$response = Invoke-RestMethod `
  -Method Post `
  -Uri http://127.0.0.1:8080/v1/images/generations `
  -ContentType "application/json" `
  -Body '{"prompt":"A crystal observatory above a storm, intricate cinematic concept art","model":"gpt-image-2","quality":"high","size":"1536x1024"}'

$outputPath = Join-Path (Get-Location) "codex-raw-image.png"
[IO.File]::WriteAllBytes(
  $outputPath,
  [Convert]::FromBase64String($response.data[0].b64_json)
)
$outputPath
```

Text request with explicit reasoning controls:

```powershell
$body = @{
  model = "gpt-5.4-mini"
  input = "Compare two safe deployment designs"
  reasoning = @{ effort = "high" }
  # Optional upstream passthrough; remove it if the model/account rejects it.
  service_tier = "auto"
  store = $false
} | ConvertTo-Json -Depth 5

Invoke-RestMethod `
  -Method Post `
  -Uri http://127.0.0.1:8080/v1/responses `
  -ContentType "application/json" `
  -Body $body
```

## Function-call ownership

Raw never executes caller-defined functions. It validates and transports the
schema, returns the model's requested function and arguments, and accepts a
matching `function_call_output` in a later stateless request.

Validation rejects:

- outputs without a corresponding call;
- duplicate outputs for the same call;
- ambiguous or malformed function items;
- unsupported tool types.

The caller must execute the function, preserve the complete prior output, and
send the matching result back to the model.

## Testing

Run the non-mutating CI-aligned checks from `codex-rs`:

```shell
cargo fmt --package codex-raw -- --config imports_granularity=Item --check
cargo clippy --locked --package codex-raw --tests --no-deps -- -D warnings
just test -p codex-raw
cargo build --locked --release --package codex-raw
```

When changes need automatic cleanup, `just fix -p codex-raw` and `just fmt`
mutate source files; review their diff before committing.

The suite covers home isolation, verified release selection and extraction,
replacement rollback, minimal request construction, app-server wire behavior,
HTTP validation, Responses, Chat and image mapping, SSE lifecycle ordering,
usage, and function-call correlation.

For a live, logged-in API process:

```shell
uv run --with 'openai>=2,<3' python \
  raw-cli/scripts/openai_sdk_smoke.py \
  --base-url http://127.0.0.1:8080/v1 \
  --model gpt-5.4-mini \
  --include-tools
```

If the server uses bearer authentication, set `CODEX_RAW_API_TOKEN` in the
smoke-test environment; the script uses it automatically without printing it.

For a paired app-server/API latency sample on Windows:

```powershell
.\raw-cli\scripts\benchmark_persistent.ps1 `
  -Exe .\target\release\codex-raw.exe `
  -Model gpt-5.4-mini `
  -Prompt hello `
  -Runs 10
```

## Upstream-sensitive integration points

After rebasing onto a newer Codex revision, inspect and retest:

- `codex_api::ResponsesApiRequest` fields and serialization;
- `codex_api::ImageGenerationRequest`, `ImageResponse`, and `ImagesClient`;
- `codex_api::ResponseEvent` completion, text, and function-call events;
- `codex_protocol::models::ResponseItem` and `ContentItem` variants;
- token usage fields in `codex_protocol::protocol::TokenUsage`;
- `AuthManager::auth()` refresh semantics;
- model catalog defaults and `use_responses_lite` metadata;
- Responses Lite headers, reasoning context, and developer-item packing;
- app-server v2 result and notification shapes.

If upstream begins exposing incremental standard function-argument deltas,
remove the Raw synthesis only after tests prove that doing so cannot duplicate
arguments.

Keep adaptation local to this crate whenever possible. Patching the standard
Codex agent pipeline would weaken the isolation that makes Raw easy to review,
test, and carry across upstream updates.
