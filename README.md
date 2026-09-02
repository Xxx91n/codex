<p align="center"><strong>Codex CLI</strong> is a coding agent from OpenAI that runs locally on your computer.
<p align="center">
  <img src="https://github.com/openai/codex/blob/main/.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
</p>
</br>
If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE.</a>
</br>If you want the desktop app experience, run <code>codex app</code> or visit <a href="https://chatgpt.com/codex?app-landing-page=true">the Codex App page</a>.
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a>.</p>

---

## About this fork (tri-wire-api)

This is a fork of `openai/codex` that restores and natively implements **three outbound wire protocols** on a single internal representation:

- **Responses** (`/v1/responses`) — upstream's only remaining wire.
- **Chat Completions** (`/v1/chat/completions`) — removed by upstream in Feb 2026 (PR #10157); reintroduced here as a first-class protocol.
- **Anthropic Messages** (`/v1/messages`) — Claude-native, including the extended-thinking chain (`thinking_delta` / `signature_delta` SSE frames, `budget_tokens` clamping, `cache_control` ephemeral breakpoints, and verbatim signature-preserving replay of `thinking` blocks).

**Mental model (why the fork is shaped this way):**

1. **One IR, three spokes.** All wires share `ResponseItem` as the single internal representation (hub-and-spoke, not pairwise translation). Complexity stays O(N) instead of O(N²).
2. **Minimal fork seam.** The divergence from upstream is contained to ~3 registration points: `WireApi`, `ModelProviderInfo`, and a per-wire dispatch in `core/src/client.rs`. Upstream still only defines `WireApi::Responses`, so our chat/messages variants never collide with upstream drift.
3. **goose-blueprint Anthropic implementation.** The Anthropic SSE state machine in `codex-api/src/sse/messages.rs` is modeled on goose's pattern (Block's Rust agent), not a hand-rolled parser.
4. **Real-gateway verified.** Every wire was tested end-to-end against a live combo gateway (H1 联调档案）：tool calls, parallel tool-call grouping, `max_tokens` hit signaling, and thinking-chain round trips. Any degradation is fail-loud, not silent — e.g., truncated `tool_use` JSON surfaces as `ApiError::Stream`, never a fabricated `{}`.
5. **Per-provider tuning.** Additional `anthropic_max_tokens`, `anthropic_thinking_budget`, `anthropic_prompt_caching` `Option<_>` fields let each provider opt in without touching global defaults.

Upstream is tracked as `upstream/main`. This fork's local agent docs (`docs/agents`, `docs/specs`, `PONYTAIL-DEBT.md`, etc.) are intentionally **not** committed here — they live in the working machine's sibling `D:/Aworker/codex/docs/` directory and are not pushed.

### Using the three wires (Responses / Chat / Anthropic Messages)

`wire_api` is **one protocol per provider** — there is no auto-negotiation and no failover. To use several protocols at once you declare multiple providers (they can even point at the same base URL) and pick one per run with `-p`:

```toml
# ~/.codex/config.toml

# default: upstream behaviour, one provider, one wire
model_provider = "openai"

# --- Responses wire (OpenAI native /v1/responses) ---
[model_providers.gw-resp]
name        = "gw-resp"
base_url    = "https://example.com/v1"   # note: path stays at /v1; the wire appends /responses, /chat/completions, or /messages itself
wire_api    = "responses"
env_key     = "MY_API_KEY"

# --- Chat Completions wire (OpenAI legacy, restored by this fork) ---
[model_providers.gw-chat]
name        = "gw-chat"
base_url    = "https://example.com/v1"
wire_api    = "chat"
env_key     = "MY_API_KEY"

# --- Anthropic Messages wire (/v1/messages) ---
[model_providers.gw-msg]
name        = "gw-msg"
base_url    = "https://example.com/v1"
wire_api    = "anthropic"
env_key     = "MY_API_KEY"

# optional anthropic-only knobs (per-provider; leave unset to use built-in defaults):
anthropic_max_tokens      = 128000   # output budget, otherwise a built-in default
anthropic_thinking_budget = 8192     # extended-thinking budget_tokens (clamped to 1024..max_tokens-1)
anthropic_prompt_caching  = true     # marks system prompt + last tool with cache_control: ephemeral
```

Then select a wire and model per invocation:

```shell
codex -p gw-resp -m some-openai-model    "…"
codex -p gw-chat -m deepseekpro          "…"
codex -p gw-msg  -m claude-…             "…"   # thinking chain streams natively (TUI shows reasoning deltas)
```

Behaviour notes:

- **`experimental_bearer_token = "PROXY_MANAGED"`** (or a real token) also works in place of `env_key` for gateways that inject auth downstream.
- The same physical gateway endpoints strike all three wires (`POST /v1/responses`, `POST /v1/chat/completions`, `POST /v1/messages`) — fork never strips or rewrites paths.
- On the Anthropic wire, replay of the model's `thinking` blocks is done verbatim with the SSE `signature` (anthropic's tool-use round contract); unsigned reasonings are **dropped** rather than altered. Non-data-URI images are dropped loudly. Truncated tool-call JSON errors out — never silently fabricated.
- A turn always runs on exactly one wire; switching wires mid-thread means starting a new turn with `-p`.

### Local dev vs. cloud CI

**Local machines only keep lightweight development — every heavy build, check, and release runs in GitHub Actions.** Locally you only need the Rust toolchain (pinned to `1.95.0` by `codex-rs/rust-toolchain.toml`) and `just` for quick `fmt`/`clippy`/`test` runs. The `fork-cli-test-release` workflow (triggered manually with `workflow_dispatch`) runs the `check` job (fmt + clippy + fork-scoped tests) and the release `build` job across Windows, macOS, and Linux, then publishes the rolling prerelease — so a local `codex-rs/target/` is just a disposable on-disk cache: delete it to reclaim ~110G of disk.

---

## Quickstart

### Installing and running Codex CLI

Run the following on Mac or Linux to install Codex CLI:

```shell
curl -fsSL https://chatgpt.com/codex/install.sh | sh
```

Run the following on Windows to install Codex CLI:

```shell
powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 | iex"
```

The standalone installers download from `https://releases.openai.com/codex` by default and fall back to GitHub Releases if a metadata or asset download is unavailable. To force GitHub Releases, set `CODEX_INSTALLER_USE_RELEASES_OPENAI_COM` to `false` (`0` and `no` are also accepted):

```shell
curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_INSTALLER_USE_RELEASES_OPENAI_COM=false sh
```

```powershell
$env:CODEX_INSTALLER_USE_RELEASES_OPENAI_COM='false'; irm https://chatgpt.com/codex/install.ps1 | iex
```

Codex CLI can also be installed via the following package managers:

```shell
# Install using npm
npm install -g @openai/codex
```

```shell
# Install using Homebrew
brew install --cask codex
```

Then simply run `codex` to get started.

<details>
<summary>You can also go to the <a href="https://github.com/openai/codex/releases/latest">latest GitHub Release</a> and download the appropriate binary for your platform.</summary>

Each GitHub Release contains many executables, but in practice, you likely want one of these:

- macOS
  - Apple Silicon/arm64: `codex-aarch64-apple-darwin.tar.gz`
  - x86_64 (older Mac hardware): `codex-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `codex-x86_64-unknown-linux-musl.tar.gz`
  - arm64: `codex-aarch64-unknown-linux-musl.tar.gz`

Each archive contains a single entry with the platform baked into the name (e.g., `codex-x86_64-unknown-linux-musl`), so you likely want to rename it to `codex` after extracting it.

</details>

### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Business, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
