# Codex Raw interfaces

Codex Raw exposes the same minimal, stateless model transport through three
interfaces:

| Interface        | Start command             | Best for                                                                      |
| ---------------- | ------------------------- | ----------------------------------------------------------------------------- |
| One-shot CLI     | `codex-raw "your prompt"` | Humans, shell scripts, and simple automation                                  |
| Persistent JSONL | `codex-raw app-server`    | A parent process that wants low-overhead stdin/stdout RPC                     |
| Local HTTP API   | `codex-raw api-server`    | OpenAI SDKs and applications that already speak Responses or Chat Completions |

All three use the isolated ChatGPT sign-in stored under `~/.codex-raw`. Codex
Raw is not an OpenAI API-key proxy and does not turn a subscription into a
public or multi-user service.

For installation and the first login, start with
[README.md](README.md#quick-start) or [INSTALL.md](INSTALL.md).

## One-shot CLI

Send one stateless prompt:

```shell
codex-raw "Explain this error in one paragraph."
```

The explicit command is equivalent:

```shell
codex-raw send "Explain this error in one paragraph."
```

Select a model when needed:

```shell
codex-raw --model gpt-5.4-mini send "Reply briefly."
```

If no model is supplied, Raw uses the active default from the signed-in
account's model catalog. Availability depends on the account and can change
upstream.

Assistant text streams to stdout. Diagnostics and the final upstream usage
object are written to stderr. Each invocation creates one request and does not
store or replay previous turns.

## Local OpenAI-compatible HTTP API

Start the server on its safe loopback default:

```shell
codex-raw api-server
```

The equivalent explicit command is:

```shell
codex-raw --model gpt-5.4-mini api-server \
  --listen 127.0.0.1:8080 \
  --max-concurrency 16
```

On PowerShell:

```powershell
codex-raw --model gpt-5.4-mini api-server --listen 127.0.0.1:8080 --max-concurrency 16
```

The process runs in the foreground and stops on `Ctrl+C`.
`--max-concurrency` defaults to 16 and accepts values from 1 through 256.

### Endpoints

| Method | Path                     | Purpose                                               |
| ------ | ------------------------ | ----------------------------------------------------- |
| `GET`  | `/healthz`               | Process liveness; always unauthenticated              |
| `GET`  | `/readyz`                | Current Raw login readiness                           |
| `GET`  | `/v1/models`             | Text model catalog plus the configured image-model ID |
| `POST` | `/v1/responses`          | Responses JSON or typed SSE                           |
| `POST` | `/v1/chat/completions`   | Chat Completions JSON or SSE                          |
| `POST` | `/v1/images/generations` | Subscription-backed base64 image generation           |

`/readyz` returns HTTP 503 when the isolated Raw login is unavailable. Listing
an image-model ID does not guarantee that the account can use it on every
endpoint. Inference requests return HTTP 429 rather than queueing when all
configured concurrency slots are occupied. Request bodies are limited to 32 MiB
so they can carry validated base64 image inputs. Inference POST requests
carrying any browser `Origin` header are rejected with HTTP 403.

### Quick requests

POSIX shell:

```shell
curl -sS http://127.0.0.1:8080/readyz

curl -sS http://127.0.0.1:8080/v1/responses \
  -H 'Content-Type: application/json' \
  --data '{"model":"gpt-5.4-mini","input":"hello"}'

curl -sS http://127.0.0.1:8080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  --data '{"model":"gpt-5.4-mini","messages":[{"role":"user","content":"hello"}]}'

curl -N http://127.0.0.1:8080/v1/responses \
  -H 'Content-Type: application/json' \
  --data '{"input":"stream this reply","stream":true}'
```

PowerShell pipes the JSON body to `curl.exe` so native argument parsing cannot
change its quotes:

```powershell
curl.exe --silent http://127.0.0.1:8080/readyz

'{"model":"gpt-5.4-mini","input":"hello"}' |
  curl.exe --silent http://127.0.0.1:8080/v1/responses `
    -H "Content-Type: application/json" --data-binary '@-'

'{"model":"gpt-5.4-mini","messages":[{"role":"user","content":"hello"}]}' |
  curl.exe --silent http://127.0.0.1:8080/v1/chat/completions `
    -H "Content-Type: application/json" --data-binary '@-'
```

### OpenAI Python SDK

The local server supports the standard client shape:

```python
import os

from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:8080/v1",
    api_key=os.environ.get("CODEX_RAW_API_TOKEN", "raw-local"),
)

response = client.responses.create(
    model="gpt-5.4-mini",
    input="hello",
)

print(response.output_text)
print(response.usage)
```

`raw-local` is only a placeholder required by the SDK constructor when the
server is token-free on loopback. It is not a real OpenAI API key. When Raw
bearer authentication is enabled, the SDK sends `CODEX_RAW_API_TOKEN` to the
local server. Raw does not forward that inbound bearer token upstream.

### Network access and bearer authentication

Loopback is the only token-free mode. Any non-loopback listener requires a
strong bearer token. Prefer the environment variable so the value is not
stored in shell history or exposed in process arguments.

The token can come from `CODEX_RAW_API_TOKEN` or `--api-token` and must contain
32–512 RFC 6750 bearer-token characters.

PowerShell example:

```powershell
$tokenBytes = [byte[]]::new(32)
[Security.Cryptography.RandomNumberGenerator]::Fill($tokenBytes)
$env:CODEX_RAW_API_TOKEN = [Convert]::ToHexString($tokenBytes)

codex-raw api-server --listen 0.0.0.0:8080
```

POSIX shell example, using a locally generated random value:

```shell
export CODEX_RAW_API_TOKEN="$(openssl rand -hex 32)"
codex-raw api-server --listen 0.0.0.0:8080
```

With a token, `/readyz` and every `/v1` route require:

```text
Authorization: Bearer <CODEX_RAW_API_TOKEN>
```

`/healthz` intentionally remains unauthenticated for liveness checks.

> [!WARNING]
> The built-in server uses plain HTTP and one shared token; it is not
> multi-user authorization. Do not expose it directly to the public internet.
> For network use, place it behind a hardened reverse proxy with TLS,
> connection/header timeouts, and connection limits. Firewall the Raw port so
> only the proxy or explicitly trusted clients can reach it.

Any local process that can reach an unprotected loopback port can use the
signed-in account's available model and image allowance. Configure a token on
loopback too when local processes are not mutually trusted.

## Request compatibility

The HTTP surface is deliberately smaller than the complete OpenAI API.
Unsupported non-null fields are normally rejected. A documented set of common
model-generation controls is accepted and discarded as a compatibility no-op.
Model/account capabilities still determine whether a forwarded option is
accepted upstream.

`GET /v1/models` includes aliases such as `gpt-model (High)` for each model's
advertised reasoning efforts. Supplying one of those aliases selects the base
model and effort. An explicit `reasoning.effort` or `reasoning_effort` takes
precedence over the alias.

### Responses

`POST /v1/responses` supports:

- `model`;
- `input` as a string or supported text, user-image, reasoning, function-call,
  and function-call-output items;
- caller-owned `instructions`;
- function `tools`;
- `tool_choice` values `none`, `auto`, or `required`;
- `parallel_tool_calls`;
- `reasoning.effort`;
- `service_tier`;
- `stream`;
- `store` omitted, `null`, or `false`.

Responses streams use typed events and end with `response.completed`.
Responses Lite models accept `parallel_tool_calls` in the public request but
force its upstream value to `false`.

### Chat Completions

`POST /v1/chat/completions` supports:

- `model`;
- text, user `image_url`, and function-tool-loop messages;
- function `tools`;
- `tool_choice` values `none`, `auto`, or `required`;
- `parallel_tool_calls`;
- `reasoning_effort`;
- `service_tier`;
- `stream`;
- `stream_options.include_usage`;
- `n` omitted or set to `1`;
- `store` omitted, `null`, or `false`.

Chat streams finish with `[DONE]`. Use Responses for reasoning-model function
loops because Chat messages cannot faithfully replay opaque encrypted
reasoning items.

Image inputs are accepted only in user messages. Responses uses
`{"type":"input_image","image_url":"..."}`; Chat Completions uses
`{"type":"image_url","image_url":{"url":"..."}}`. URLs must be HTTPS or base64
PNG, JPEG, WEBP, or GIF data URLs. A request may contain at most 10 images;
remote URLs are capped at 8 KiB and data URLs at 3 MiB each. Responses Lite
forwards images but omits the unsupported `detail` setting.

For SDK compatibility, the text routes accept and discard these model
generation controls: `frequency_penalty`, `logprobs`,
`max_completion_tokens`, `max_output_tokens`, `max_tokens`,
`presence_penalty`, `response_format`, `seed`, `stop`, `temperature`,
`top_logprobs`, and `top_p`. They do not change the upstream request.

### Image generation

Generate subscription-backed image data:

```shell
curl -sS http://127.0.0.1:8080/v1/images/generations \
  -H 'Content-Type: application/json' \
  --data '{
    "prompt":"A crystal observatory above a storm",
    "model":"gpt-image-2",
    "quality":"high",
    "size":"1536x1024"
  }'
```

The route supports a non-empty `prompt`, `n` from 1 through 4, supported
`quality`, `size`, and `background` values, and
`response_format="b64_json"`. It returns base64 image data and resolved image
metadata. Account and model availability still apply.

### Intentionally unsupported behavior

The current text routes reject or do not implement:

- stateful `store: true` and `previous_response_id`;
- built-in web, shell, file, MCP, or agent tools;
- structured-output enforcement;
- background text jobs;
- automatic execution of caller-defined functions.

Consult `codex-raw api-server --help` for the exact fields accepted by the
installed version.

## Function tools are caller-owned

Raw transports function definitions and results but never runs the function:

```text
client -> sends a function schema
model  -> returns a function_call and JSON arguments
client -> validates and executes the function
client -> sends a matching function_call_output
model  -> uses the result and returns final text
```

Example request that allows one caller-owned function:

```shell
curl -sS http://127.0.0.1:8080/v1/responses \
  -H 'Content-Type: application/json' \
  --data '{
    "input":"What is the weather in Sofia?",
    "tools":[{
      "type":"function",
      "name":"get_weather",
      "description":"Return weather for a city",
      "parameters":{
        "type":"object",
        "properties":{"city":{"type":"string"}},
        "required":["city"],
        "additionalProperties":false
      },
      "strict":true
    }],
    "tool_choice":"required",
    "parallel_tool_calls":false
  }'
```

The caller must preserve the complete previous Responses output, execute the
requested function safely, and send the matching result in the next stateless
request. Raw rejects unmatched or duplicate function outputs.

## Persistent JSONL app-server

Start a reusable child process:

```shell
codex-raw --model gpt-5.4-mini app-server
```

It reads one JSON object per line from stdin and writes one flushed JSON object
per line to stdout. The supported protocol is a small subset of Codex
app-server v2 and does not include a `"jsonrpc":"2.0"` field.

Initialize:

```json
{"id":1,"method":"initialize","params":{"clientInfo":{"name":"example","version":"1.0.0"}}}
{"method":"initialized"}
```

Create an ephemeral handle and start a turn:

```json
{"id":2,"method":"thread/start","params":{"model":"gpt-5.4-mini","ephemeral":true}}
{"id":3,"method":"turn/start","params":{"threadId":"raw-thread-1","input":[{"type":"text","text":"hello"}]}}
```

Supported methods are `initialize`, `initialized`, `thread/start`, and
`turn/start`. The protocol thread is a client handle only: prior turns are not
sent back to the model. The persistent process reuses authentication, model
metadata, its HTTP client, and connection pool.

Stdout is protocol-only. A parent process must read stderr concurrently so a
full diagnostic pipe cannot block the child.

## Authentication boundary

Run:

```shell
codex-raw login
codex-raw status
```

Credentials are stored only in the dedicated Raw home, normally
`~/.codex-raw`. Treat `~/.codex-raw/auth.json` as a plaintext secret. Never
commit it, copy it into a support report, or reuse it as an API token.

Raw accepts its own ChatGPT login only. API-key and arbitrary upstream-header
authentication modes are disabled. Account entitlements, rate limits, safety
controls, and platform policies continue to apply.

## Implementation reference

Maintainers and integrators can find internal invariants, wire mappings, test
commands, and upstream-sensitive integration points in
[`codex-rs/raw-cli/README.md`](codex-rs/raw-cli/README.md).
