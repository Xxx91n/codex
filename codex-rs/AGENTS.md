# codex-rs (Rust workspace)

Rust workspace of the `feat/tri-wire-api` fork of `openai/codex`. Crate names are
prefixed `codex-` (the `core` folder's crate is `codex-core`, etc.). This file
scopes everything under `codex-rs/`; it adds fork-specific rules on top of the
repo-root `AGENTS.md`.

## Scope & inheritance

- The repo-root `AGENTS.md` applies to every task here — read it first. Nothing
  in this file contradicts it; if something looks contradictory, the root wins
  and this file is wrong.
- Instruction files load root → leaf; the closest file to the edited file takes
  precedence. Keep this file to rules that hold for the whole Rust subtree.
  Per-module protocol red lines live in `core/src/client/wire/AGENTS.md`.
- Fork-wide red lines live in the root AGENTS.md "Fork delta: tri-wire-api"
  section (L324-393). This file restates the ones that bite inside codex-rs and
  points at the rest — it does not re-document them. Do not copy rules from the
  root here; fix the root instead (copied rules drift).

## Build & test (authoritative)

- Never run `cargo test` directly. Use `just` (in `codex-rs/`).
- `just test -p codex-api` — SSE unit tests for the Chat/Messages wires.
- `just test -p codex-core` — client_tests + the wiremock integration suites
  (`tests/suite/chat_wiremock.rs`, `tests/suite/anthropic_wiremock.rs`). This is
  the fast loop for wire changes.
- `just test` — full workspace suite; only after changes to shared crates, and
  ask before running it (root rule).
- `just fix -p <project>`, `just clippy -p <project>`, `just fmt`,
  `just fmt-check`.
- Release build (CI parity):
  `RUST_MIN_STACK=16777216 cargo build --release --locked -p codex-cli --bin codex`.
- `just` sets `RUST_MIN_STACK=8388608` and `NEXTEST_PROFILE=local` locally. No
  wire-specific env vars are required for the wiremock suites; live smoke tests
  need a real gateway and a provider `env_key` (README three-wire usage guide).
- Run `just fmt` after finishing code changes, without asking.

## Rust style (root rules, unchanged)

Crate API surface, clippy collapses, `argument_comment_lint`, RPITIT over
`#[async_trait]`, module LoC targets (500 soft / 800 hard), test authoring,
snapshot tests, and integration-test harness patterns are all defined in the
root AGENTS.md — they apply unchanged here.

## codex-core discipline

`codex-core` is the largest crate and is bloat-prone. Resist adding code to it;
prefer an existing crate or a new workspace crate. Push back on changes that
grow it unnecessarily. (Details: root AGENTS.md "The `codex-core` crate".)

## Fork delta map (ADR-0001 baseline)

The fork diverges from upstream by ~43 files (+4013/-9), concentrated in:

- `codex-api` — new endpoints + SSE files (~1400 new lines):
  `src/endpoint/messages.rs`, `src/sse/chat_completions.rs`,
  `src/sse/messages.rs`
- `core/src/client.rs` (+747 — the upstream conflict hotspot) and
  `core/src/client_tests.rs` (+419)
- `core/src/client/wire/` — per-wire request building + streaming
  (fork-only; has its own AGENTS.md — read it when working there)
- `tools/tool_spec`, `model-provider-info`, and the wiremock test clusters

Per-wire implementation lives in NEW files so upstream syncs touch them instead
of high-touch files. Do not move per-wire logic back into `client.rs` to
resolve a merge conflict.

## Fork red lines (fork-only, must hold)

1. Three registration points never move, and no wire is added without touching
   ALL three (root AGENTS.md "Registration points"):
   - `WireApi` enum — `codex-rs/model-provider-info/src/lib.rs`
   - `ModelProviderInfo.wire_api` field — same file
   - dispatch `match` on `wire_api` in `core/src/client.rs` (`stream()`)
2. Current wires: Responses (upstream's only), Chat (restored here; removed
   upstream Feb 2026, PR #10157 blueprint), Anthropic Messages (fork addition;
   goose-blueprint in-process transport, self-implemented — no third-party
   protocol dependencies).
3. Do NOT add a Gemini wire: rejected by ADR-0003 (Accepted). Reopen only on
   its triggers R1-R3 (`docs/adr/0003-gemini-wire-verdict.md`); even then the
   access path goes through the same three registration points.
4. Fork constants stay put (ADR-0002 Ruling 1): `CHAT_COMPLETIONS_ENDPOINT`,
   `ANTHROPIC_MESSAGES_ENDPOINT`, `DEFAULT_ANTHROPIC_MAX_TOKENS` (8192 — known
   PONYTAIL debt: truncates long Claude turns at `stop_reason=max_tokens`) in
   `core/src/client.rs`; `ANTHROPIC_VERSION` in
   `codex-api/src/endpoint/messages.rs`.
5. Chat/Anthropic synthesize the `Completed` event with `usage_metadata: None`;
   the Responses wire passes `usage_metadata` through unchanged (ADR-0002
   Ruling 2).
6. `handle_unauthorized` carries `event_sender` + `turn_id` params (upstream
   merge 94311d44); keep the fork's call sites passing them (ADR-0002 Ruling 3).

## Testing

- Wire changes: `just test -p codex-core` — the wiremock integration suites are
  the acceptance gate. Baseline trio (extend or mirror for new behavior):
  `chat_wire_tool_call_roundtrip`, `chat_wire_plain_text_turn`,
  `anthropic_wire_tool_call_roundtrip`.
- Any new wire behavior MUST add or extend a wiremock integration test — unit
  tests alone are not sufficient for protocol translation logic.
- SSE-frame unit tests for Chat/Messages live in codex-api:
  `just test -p codex-api`.
- Assert on outbound request bodies (wiremock request capture) — assert what
  the wire sends, not just that the stream completed.

## Sync & ADR process

- `upstream` = `openai/codex`; `origin` = this fork. Track `upstream/main`.
- Merge, never rebase. Rebase rewrites the fork seam and destroys auditability.
- After each upstream merge, record rulings as a new Nygard ADR in
  `docs/adr/NNNN-*.md`, incrementing the counter (ADR-0001 Decision).
- Heavy architecture notes stay OUT of the repo (workspace `docs/` directory).
- When upstream touches `client.rs` or codex-api near fork seams, expect a
  ruling; record it in `docs/adr/` before editing fork-only files.

## Out of scope here

- Per-wire protocol red lines → `core/src/client/wire/AGENTS.md`.
- TUI state-machine guidance → `tui/src/bottom_pane/AGENTS.md` (upstream leaf).
- CLI/TS/Python surface rules → repo-root AGENTS.md.
