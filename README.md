# Claude Subscription Proxy

A small Tauri 2 desktop app that runs a **local HTTP server** exposing your Claude
Code **subscription** (Pro / Max) as both an **OpenAI-compatible**
(`/v1/chat/completions`) and **Anthropic-compatible** (`/v1/messages`) API. Other
tools on your LAN point at the proxy, send a proxy-issued key, and their traffic
is billed against your subscription instead of a metered API key.

The proxy never forwards raw OAuth tokens. It runs every request through the real
[`claude`](https://docs.claude.com) CLI in headless mode
(`claude -p --output-format stream-json`), so Claude Code itself makes the
upstream call under your authenticated session.

## Prerequisites

- **Claude Code** installed and on `PATH`. Verify with `claude --version`.
- A **Pro / Max subscription** account that supports headless CLI calls.
  Confirm with:
  ```bash
  claude auth status    # must exit 0; the GUI also surfaces this
  ```
  If `claude auth status` exits non-zero, run `claude auth login` once to
  authenticate, then reopen the app and click **Recheck**.
- **Node 20+** and **Rust stable** with the Tauri 2 prerequisites
  (Xcode CLT on macOS, `webkit2gtk` on Linux, MSVC + WebView2 on Windows).

## Build & run

```bash
npm install
npm run tauri dev          # launch the desktop app (debug)
npm run tauri build        # produce a release bundle for your platform
```

The first launch creates `config.json` and `keys.json` in your platform's app
config dir (`~/Library/Application Support/dev.local.claude-subscription-proxy`
on macOS) and a `scratch/` working dir under app data.

## Pointing a client at the proxy

Once the server is **Running** at, e.g. `http://192.168.1.10:8787`:

### OpenAI-compatible client

```bash
curl http://192.168.1.10:8787/v1/chat/completions \
  -H "Authorization: Bearer csp-<your-key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"Reply with exactly: OK"}]}'
```

- `base_url`: `http://<host>:8787/v1`
- Auth: send the proxy key as the OpenAI key (`Authorization: Bearer …` or
  `x-api-key`). The proxy verifies against a local store of sha256 hashes — the
  raw key is shown to you exactly once at creation time.
- Model strings are mapped to Claude equivalents by the `Config → Model map`
  field. The default map is `gpt-4o → opus`, `gpt-4o-mini → haiku`,
  `gpt-4 → opus`, `gpt-3.5-turbo → haiku`. Any name starting with `claude` is
  passed through untouched.
- Sampling params (`temperature`, `top_p`, `max_tokens`, `n`, …) are accepted
  and silently ignored — the `claude` CLI exposes no equivalent knobs.

### Anthropic-compatible client

```bash
curl http://192.168.1.10:8787/v1/messages \
  -H "x-api-key: csp-<your-key>" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{"model":"claude-sonnet-4-5","max_tokens":50,"messages":[{"role":"user","content":"Reply with exactly: OK"}]}'
```

- `base_url`: `http://<host>:8787` (the SDK's own `/v1/messages` is appended)
- Auth: `x-api-key: <proxy key>` (or `Authorization: Bearer …`).
- Anthropic mode **passes through** the CLI's own stream events, so SSE clients
  see `message_start` → `content_block_*` → `message_delta` → `message_stop`
  with the message `id` and `model` rewritten to your locally generated ids.

### Endpoints

| Method | Path | Notes |
|---|---|---|
| `GET`  | `/v1/models` | Lists Claude models plus your model map keys. |
| `POST` | `/v1/chat/completions` | OpenAI shape, non-streaming + SSE. |
| `POST` | `/v1/messages` | Anthropic shape, non-streaming + SSE passthrough. |

## What the proxy does (and does not)

- **Spawns** `claude -p "<final user message>" --output-format stream-json` once
  per request, in a per-request scratch dir with `--tools ""` and
  `--max-turns 1`. Project tools and agents are intentionally unreachable.
- **Isolates** from your personal Claude config via `--setting-sources ""` and
  `--strict-mcp-config`: the proxy loads no user/project/local settings, so your
  own hooks, output styles, and MCP servers never leak into proxied responses.
- **Flattens** OpenAI / Anthropic `messages` arrays: system text is concatenated
  and routed to `--append-system-prompt` (the default "You are Claude Code"
  identity is preserved); prior turns are written to the child's stdin as a
  transcript. Image and non-text parts are rejected with HTTP 400.
- **Streams** Anthropic events verbatim; translates to OpenAI `chat.completion.chunk`
  for the OpenAI shape, ending with `data: [DONE]`.
- **Caps** total request time at `Config → Timeout seconds` (default 600) and
  bounds concurrency at `Config → Max concurrency` (default 4) — extra requests
  wait, not reject.
- **Does not** touch your auth tokens, does not expose OAuth, does not bypass
  rate limits.

## Subscription, Agent SDK credits, and policy

Anthropic disallows raw-token third-party harnesses as of 2026-04-04, which is
why the proxy uses the official CLI. As of 2026-06-15, headless `claude -p`
calls on a subscription draw from a **separate monthly "Agent SDK credit"
pool** rather than your interactive Pro / Max limits. The proxy has no special
treatment — every call you make through it consumes that pool, and you are
responsible for staying within Anthropic's subscription terms.

## Layout

- `src-tauri/src/config.rs` — persistent config (`<app_config_dir>/config.json`)
- `src-tauri/src/keys.rs` — proxy API key store (`<app_config_dir>/keys.json`),
  sha256-hashed, raw shown once
- `src-tauri/src/claude_auth.rs` — `claude auth status` parsing + macOS login
  helper (spawns Terminal via `osascript`)
- `src-tauri/src/server/claude.rs` — the core: spawns the CLI, parses NDJSON
  stream-json output, classifies events
- `src-tauri/src/server/openai.rs` — `/v1/models` + `/v1/chat/completions`
- `src-tauri/src/server/anthropic.rs` — `/v1/messages` with verbatim event
  passthrough
- `src-tauri/src/server/translate.rs` — message-array → `(system, final_user,
  history_stdin)` flattening
- `src-tauri/src/server/auth.rs` — bearer / x-api-key middleware
- `src-tauri/src/commands.rs` — Tauri commands the frontend calls
- `src/` — vanilla-TS dashboard (panels: Server, Auth, Keys, Config, Logs)

## Development

```bash
# Backend tests (config, keys, auth, claude runner, translation,
# OpenAI/Anthropic response shaping, server lifecycle)
cd src-tauri && cargo test

# Frontend helpers (view-model)
npm test

# Type-check + production bundle
npm run build

# End-to-end smoke against a running server
npm run tauri dev
```

## License

This project is provided as-is, with no warranty. Use of the proxy is governed
by Anthropic's subscription terms.
