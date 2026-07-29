# Codex Raw

> [!WARNING]
> Codex Raw is an independent, unofficial community fork. It is not affiliated
> with, endorsed by, or supported by OpenAI. “OpenAI” and “Codex” are marks of
> their respective owner.

## The core idea

> [!IMPORTANT] > **Codex Raw removes the Codex agent layer—not the account controls.**
>
> It sends caller-controlled, stateless model requests without the standard
> Codex agent prompt, built-in tools, repository or workspace discovery,
> hidden project context, or conversation history.
>
> It authenticates through its own browser-based ChatGPT/Codex sign-in and can
> expose that transport as a local OpenAI-compatible HTTP API. It is **not** an
> official OpenAI API service, does not accept an OpenAI API key for upstream
> authentication, and does not bypass model availability, account
> entitlements, rate limits, safety controls, or platform policies.

```text
your prompt / local OpenAI-compatible request
                        |
                        v
                    codex-raw
(no agent prompt · no built-in tools · no repository context · no hidden history)
                        |
                        v
       browser-authenticated ChatGPT/Codex account transport
               (normal account controls still apply)
                        |
                        v
                  model response
```

It ships as a separate executable named `codex-raw` and offers three ways to
use the same minimal runtime:

| Mode             | Command                | Use it for                                                       |
| ---------------- | ---------------------- | ---------------------------------------------------------------- |
| One-shot CLI     | `codex-raw "hello"`    | People, shell scripts, and single stateless prompts              |
| Persistent JSONL | `codex-raw app-server` | Parent processes that communicate over stdin/stdout              |
| Local HTTP API   | `codex-raw api-server` | OpenAI SDKs and applications using Responses or Chat Completions |

[Download a release](https://github.com/bproject07/Codex-Source/releases) ·
[Download source ZIP](https://github.com/bproject07/Codex-Source/archive/refs/heads/main.zip) ·
[Installation guide](INSTALL.md) · [API guide](API.md) ·
[Benchmarks](BENCHMARKS.md) · [Security policy](SECURITY.md) ·
[Changelog](CHANGELOG.md)

## Quick start

### 1. Install the executable

For most users, download the archive that matches the operating system and CPU
from [GitHub Releases](https://github.com/bproject07/Codex-Source/releases).
Prebuilt binaries do not require Rust.

| Platform      | CPU       | Asset suffix                       |
| ------------- | --------- | ---------------------------------- |
| Windows       | x64       | `x86_64-pc-windows-msvc.zip`       |
| Linux (glibc) | x64       | `x86_64-unknown-linux-gnu.tar.gz`  |
| Linux (glibc) | ARM64     | `aarch64-unknown-linux-gnu.tar.gz` |
| macOS         | Intel x64 | `x86_64-apple-darwin.tar.gz`       |

Download `SHA256SUMS` from the same release and verify the archive before
installing it. The release workflow produces checksummed archives but does not
currently code-sign or notarize them. See [INSTALL.md](INSTALL.md) for exact
verification, extraction, `PATH`, and platform instructions.

If no public release exists yet, or if no archive matches the platform, build
from source. Download the
[main-branch source ZIP](https://github.com/bproject07/Codex-Source/archive/refs/heads/main.zip)
or clone the repository:

```shell
git clone https://github.com/bproject07/Codex-Source.git
cd Codex-Source/codex-rs
rustup toolchain install 1.95.0
cargo +1.95.0 build --locked --release --package codex-raw
```

The result is `target/release/codex-raw` on Unix or
`target\release\codex-raw.exe` on Windows. Before continuing, use the
[source-build installation commands](INSTALL.md#build-from-source) to copy it
to a user binary directory and activate `codex-raw` on `PATH`.

### 2. Sign in

```shell
codex-raw login
codex-raw status
```

The login opens a browser flow and stores Raw-only state under
`~/.codex-raw`. It does not reuse or modify the standard `~/.codex`
directory.

### 3. Send a prompt

```shell
codex-raw "Reply with a short hello."
```

The explicit form is equivalent:

```shell
codex-raw send "Reply with a short hello."
```

Raw uses the account's active default model when `--model` is omitted:

```shell
codex-raw --model MODEL_ID send "Explain this in one paragraph."
```

### 4. Optionally start the local API

```shell
codex-raw api-server
```

The safe default is `127.0.0.1:8080`. In another terminal:

```shell
curl -sS http://127.0.0.1:8080/readyz
curl -sS http://127.0.0.1:8080/v1/responses \
  -H 'Content-Type: application/json' \
  --data '{"input":"hello"}'
```

See [API.md](API.md) for PowerShell, Python SDK, streaming, Chat Completions,
image generation, function tools, JSONL, and network-security examples.

## The idea

The standard Codex CLI is a complete coding agent. It may assemble model
instructions, tools, sandbox and approval rules, workspace information,
repository guidance, skills, plugins, MCP context, and prior conversation
state.

Codex Raw intentionally takes a smaller path:

```text
your prompt or request
        |
        v
codex-raw  +  isolated login in ~/.codex-raw
        |
        v
minimal stateless model request
```

A one-shot Raw request contains the caller's user message, empty instructions,
no tools, no parallel tool calls, `store: false`, and streaming enabled. Some
model transports require additional protocol framing, but Raw does not inject
the Codex agent prompt or silently discover project context.

This design is useful when an application needs:

- explicit control over what is sent to the model;
- predictable stateless calls;
- a lightweight local bridge for an OpenAI-shaped client;
- a persistent process without a full coding-agent session;
- a clean transport layer for testing or benchmarking.

## Codex compared with Codex Raw

| Behavior                 | Standard Codex                                   | Codex Raw                                                            |
| ------------------------ | ------------------------------------------------ | -------------------------------------------------------------------- |
| Primary purpose          | Interactive coding agent                         | Minimal model transport                                              |
| Instructions             | Agent and environment instructions               | Empty by default or supplied by the API caller                       |
| Built-in tools           | Shell, files, search, MCP, and other agent tools | None                                                                 |
| Repository discovery     | Yes                                              | No                                                                   |
| Conversation persistence | Supported                                        | No hidden persistence; callers replay what they need                 |
| Function calls           | Agent can run tools                              | Raw transports caller-defined function calls but never executes them |
| Executable               | `codex`                                          | `codex-raw`                                                          |
| Default state            | `~/.codex`                                       | `~/.codex-raw`                                                       |

Codex Raw is not intended to replace Codex for autonomous coding work. Both
executables can be installed on the same machine because their filenames and
state directories are separate.

## Capabilities

- Browser-based ChatGPT sign-in with guarded token refresh.
- Minimal one-shot prompts with streamed output and upstream usage reporting.
- Persistent JSONL process for low-overhead child-process integrations.
- OpenAI-compatible local Responses and Chat Completions routes.
- Typed Responses SSE and Chat Completions SSE streaming.
- Model discovery through `/v1/models`.
- Image inputs on user messages through HTTPS or supported base64 data URLs.
- Caller-defined function schemas, calls, and matching outputs.
- Subscription-backed image generation through `gpt-image-2`.
- Strict validation instead of silently ignoring unsupported request fields.
- A user-triggered self-updater that verifies immutable release metadata, two
  independent SHA-256 sources, and the staged binary version.

Codex Raw deliberately does not provide:

- the Codex coding-agent loop or base instructions;
- shell, filesystem, web, MCP, or other built-in tools;
- `AGENTS.md`, skills, plugin, app, or workspace loading;
- automatic execution of caller-defined functions;
- stateful text responses or thread resume;
- a public, multi-tenant, or per-user authorization service.

## Authentication and isolated state

Raw stores its ownership marker, model metadata, and file-backed login under:

```text
~/.codex-raw
```

Treat `~/.codex-raw/auth.json` as a plaintext secret:

- restrict access to the account that runs Raw;
- never commit, upload, print, or include it in a support report;
- never use it as the HTTP API bearer token;
- run `codex-raw logout` when the credentials are no longer needed.

A dedicated state directory can be selected with:

```shell
codex-raw --home /path/to/raw-state login
```

or the `CODEX_RAW_HOME` environment variable.

Raw refuses known Codex state directories and their descendants. It also
refuses an existing directory that does not contain the expected Raw ownership
marker. The guard is lexical; do not use a symbolic link or junction to point
a Raw home at another application's state directory. Logging out removes only
Raw credentials.

## Updating

Builds that expose the `update` command include a user-triggered self-updater:

```shell
codex-raw update --check
codex-raw update
```

Use `sudo codex-raw update` for a root-owned Linux installation.

The updater is **not** a background service. It runs only when the command is
invoked. It selects the exact OS/CPU archive, accepts only a newer stable
immutable release, verifies GitHub's asset digest and the matching
`SHA256SUMS` entry, stages and version-checks the binary, then replaces the
installed version with rollback protection.

On Linux, Raw manages only the active fixed unit `codex-raw-api.service` after
verifying that its `MainPID` uses the exact installed binary and its command
line contains `api-server`; a mismatch aborts the update. Startup and health
failures trigger a best-effort rollback only after the failed service is safely
stopped. Other Unix processes must be restarted manually. Windows uses a
detached helper and Windows Restart Manager for the exact installed executable.
It requests graceful shutdown and attempts to restart eligible application
processes without force-killing by process name; successful relaunch is not
guaranteed.

See [INSTALL.md](INSTALL.md#updating) for the complete behavior and supported
platforms.

## HTTP API at a glance

Start on loopback:

```shell
codex-raw api-server --listen 127.0.0.1:8080 --max-concurrency 16
```

| Method | Path                     | Purpose                      |
| ------ | ------------------------ | ---------------------------- |
| `GET`  | `/healthz`               | Process liveness             |
| `GET`  | `/readyz`                | Current login readiness      |
| `GET`  | `/v1/models`             | Available model catalog      |
| `POST` | `/v1/responses`          | Responses JSON or SSE        |
| `POST` | `/v1/chat/completions`   | Chat Completions JSON or SSE |
| `POST` | `/v1/images/generations` | Base64 image generation      |

The compatibility surface is intentionally partial. User messages can contain
validated image inputs, common unsupported model-generation controls are
accepted as compatibility no-ops, function tools remain caller-owned, and text
requests are stateless. Consult `codex-raw api-server --help` and
[API.md](API.md) for the exact behavior of the installed version.

## Security essentials

- Keep the default listener on `127.0.0.1` unless network access is required.
- Token-free mode is loopback-only. Any local process reaching that port can
  use the signed-in account's available allowance.
- Non-loopback listeners require `CODEX_RAW_API_TOKEN` or `--api-token`.
- Prefer `CODEX_RAW_API_TOKEN`; command-line secrets may appear in process
  lists and shell history.
- `/readyz` and `/v1` require the bearer token when configured.
  `/healthz` intentionally remains unauthenticated.
- The built-in server uses plain HTTP and one shared secret. It is not
  multi-user authorization.
- For network traffic, use a hardened TLS reverse proxy and firewall the Raw
  port so it is not directly reachable from the public internet.
- Never expose credentials, private prompts, tokens, or generated sensitive
  data in issues, logs, commits, or release archives.

Read [SECURITY.md](SECURITY.md) before exposing the server outside a trusted
local environment.

## Project status and boundaries

Codex Raw is experimental. The supported prebuilt and self-update targets are
Windows x64, Linux x64, Linux ARM64, and macOS Intel. There is currently no
prebuilt Windows ARM64 or native macOS Apple Silicon release.

Models and account capabilities are discovered upstream. A model name shown in
an example may not be available to every account. Generated output, latency,
usage, and supported upstream controls can change independently of this
repository.

Release archives are built from tags by GitHub Actions and accompanied by
`SHA256SUMS`. The current workflow does not code-sign or notarize them.

## Source tree and documentation

Codex Raw is maintained as an isolated crate inside an upstream-derived Codex
source tree:

```text
codex-rs/raw-cli/
  Cargo.toml
  README.md
  scripts/
  src/
```

The rest of the repository includes substantial inherited Codex source and
documentation. Files describing the standard Codex CLI or SDKs are retained
for upstream development and are not automatically documentation for Raw.

Use this map:

- [README.md](README.md) — project concept and first run;
- [INSTALL.md](INSTALL.md) — binaries, source builds, checksums, updates, and
  troubleshooting;
- [API.md](API.md) — CLI, JSONL, HTTP, SDK, request, and security reference;
- [BENCHMARKS.md](BENCHMARKS.md) — release-bound benchmark methodology,
  results, and raw-sample requirements;
- [codex-rs/raw-cli/README.md](codex-rs/raw-cli/README.md) — implementation
  invariants and maintainer notes;
- [CHANGELOG.md](CHANGELOG.md) — fork release history;
- [UPSTREAM.md](UPSTREAM.md) — upstream provenance and synchronization policy;
- [CONTRIBUTING.md](CONTRIBUTING.md) — contribution entry point;
- [SUPPORT.md](SUPPORT.md) — where to ask for help;
- [SECURITY.md](SECURITY.md) — private vulnerability reporting.

## Contributing

Read `AGENTS.md` and [CONTRIBUTING.md](CONTRIBUTING.md) before changing the
source. The non-mutating, CI-aligned Raw checks run from `codex-rs`:

```shell
cargo fmt --package codex-raw -- --config imports_granularity=Item --check
cargo clippy --locked --package codex-raw --tests --no-deps -- -D warnings
just test -p codex-raw
cargo build --locked --release --package codex-raw
```

`just fix -p codex-raw` and `just fmt` modify source files; review their diff
before committing.

Changes to commands, endpoints, accepted fields, authentication, update
behavior, or security boundaries must update the relevant public guide and the
implementation notes.

## License and attribution

This fork retains the [Apache License 2.0](LICENSE) and applicable notices in
[NOTICE](NOTICE). See [UPSTREAM.md](UPSTREAM.md) for the exact upstream base.
The Codex Raw name identifies this independent fork only; it is not presented
as an official OpenAI product or as a replacement for the standard Codex
coding agent.
