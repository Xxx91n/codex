# core/src/client/wire — per-wire transport (fork seam)

Fork-only modules: build outbound requests and run streaming loops for the
non-Responses wires on one internal representation (`ResponseItem`).

## Scope & inheritance

- Repo-root `AGENTS.md` and `codex-rs/AGENTS.md` both apply above this file —
  read them first. This file adds ONLY what is true for tasks touching this
  directory's protocol surface.
- Root → codex-rs → here is the designed progressive-disclosure chain: wire
  red lines live HERE so work in other subtrees never pays their token cost.
- Line numbers drift; where this file cites `client.rs`/`lib.rs` lines, treat
  them as "at time of writing" and grep for the symbol.

## Boundary

- These modules own request construction + streaming for Chat and Anthropic
  Messages. The three registration points stay OUT of this directory:
  1. `WireApi` enum — `codex-rs/model-provider-info/src/lib.rs`
  2. `ModelProviderInfo.wire_api` field — same file
  3. dispatch `match` on `wire_api` — `core/src/client.rs` (`stream()`)
- This directory is reached only through the dispatch. Never add a fourth
  registration point here, and never move a registration point in.
- The API-crate side of the seam lives in codex-api
  (`endpoint/messages.rs`, `sse/chat_completions.rs`, `sse/messages.rs`);
  changing wire/ behavior often requires changing it too — check both sides.

## File map

- `mod.rs` — module docs + shared helpers (`content_items_to_text`).
- `chat.rs` — `stream_chat_completions`, `build_chat_request`,
  `build_chat_messages`, `map_chat_role`.
- `anthropic.rs` — `stream_anthropic_messages`, `build_messages_request`,
  `build_messages_messages`, `anthropic_tool_use_block`,
  `push_anthropic_tool_result`, `content_items_to_image_blocks`.
- `reasoning_effort.rs` — pure effort→wire-parameter translation:
  `chat_reasoning_effort`, `budget_tokens_for_effort`, `adaptive_effort_value`,
  `clamp_thinking_budget`, `anthropic_thinking_from_effort` (ticket 11).

## Red lines

### Adding a wire

1. A new protocol = a new module file in this directory + registration at all
   three points (WireApi variant, provider `wire_api` config, dispatch branch)
   + the config/schema surface. A match arm alone is NOT a wire.
2. Follow the anthropic.rs/chat.rs shape: request builder + streaming loop in
   the module; keep `client.rs` holding only the dispatch.

### Thinking signature (Anthropic)

3. Thinking blocks replay verbatim — text AND `signature` — inside tool-use
   rounds; without the signature the server 400s the whole turn. A reasoning
   item WITHOUT a signature is dropped (validation relaxed for non-tool turns).
4. Never merge thinking content into text content blocks, and never truncate
   or rewrite a signature.

### cache_control (Anthropic only)

5. Prompt-cache `cache_control` injection (`{type: ephemeral}`) happens ONLY
   on the anthropic outbound path. Chat and Responses have no equivalent — do
   not inject there, and do not strip what providers send on ingest.

### Images

6. `content_items_to_image_blocks` accepts ONLY base64 data-URIs. Plain
   http(s) URLs and malformed data-URIs are explicitly dropped (debug log +
   the text sibling already carries the fallback marker). The Messages API has
   no URL-fetch variant; silently "upgrading" a dropped image to text is a
   semantic change — don't.

### Tool-use replay (Anthropic)

7. `anthropic_tool_use_block`: a no-argument tool call MUST serialize
   `"input": {}` — `null` makes the Messages API reject the replayed
   tool_use block with a 400.
8. `push_anthropic_tool_result`: consecutive tool results coalesce into ONE
   user message — the Messages API requires strict user/assistant alternation.

### Usage metadata & events

9. Chat and Anthropic have no Responses-style event framing: streams are
   parsed per-wire and framework events (Added/Done/Completed) are synthesized
   on the internal `ResponseItem` surface. Keep synthesis stateful and
   block-level; never pass raw SSE bytes through (NDJSON/half-frame hazards
   are per-wire bugs).
10. The synthesized `Completed` event on Chat/Anthropic carries
    `usage_metadata: None` (ADR-0002 Ruling 2; codex-api
    `sse/chat_completions.rs`, `sse/messages.rs`); the Responses wire passes
    usage through unchanged.

### Degrade surface (fork semantics)

11. Non-Responses wires degrade or localize explicitly: auto-compact is
    disabled/localized; `store` / `previous_response_id` and
    `/responses/compact` semantics have no equivalent. Never silently pretend
    a capability exists on Chat/Anthropic.
12. Treat finish conditions per wire (Anthropic `stop_reason`, Chat finish
    flags): an early stop is data, not a stream-end shortcut — let the
    protocol's own termination drive the loop.
13. Effort translation (`reasoning_effort.rs`) is pure and deterministic:
    the same session effort must produce byte-identical `reasoning_effort`/
    `thinking`/`output_config` payloads — effort changes are prompt-cache-
    key material on Anthropic- and Kimi-family upstreams. Never thread
    timestamps, randomness, or per-request state through it.

## How to add a wire (4th-wire recipe)

Agent-executable checklist for adding a 4th wire (e.g. Gemini) on this seam.
The human-facing narrative and the why live in
[README.md — How to add a 4th wire](../../../../../README.md#how-to-add-a-4th-wire);
this section only adds what an agent must DO and VERIFY inside this directory.

**Preflight (ADR-0006 decision 3 ③, 清单先于实现).** Before the first commit,
open the three FORK_DIVERGENCE entries for the new wire (per ADR-0006 decision 4
— seam attribution) and draft its CONTEXT.md vocabulary entry (usage +
prohibited-usage pairs). Implementation does not start until the queue entries
and vocabulary exist.

1. **Build the module pair from the existing spokes.** Copy the
   `anthropic.rs` / `chat.rs` shape into `wire/<new>.rs` (request builder +
   streaming loop) and the inbound state machine into
   `codex-api/src/sse/<new>.rs`; extend the File map and the `mod.rs`
   module doc to declare the new spoke. Verify: the new module compiles in
   isolation; README step 1 has the blueprint and link stays unbroken.
   → Red line 2 — shape: builder + streaming loop live in the module,
     dispatch stays in `client.rs`.

2. **Register at the three seam anchors — in their homes, not here.** Add the
   `WireApi` variant, the `wire_api` field mapping, and the dispatch
   branch (in `model-provider-info/src/lib.rs` and `core/src/client.rs`).
   This directory stays untouched by registration (see Boundary above).
   Verify: `grep -nE 'WireApi::|wire_api:|wire_api =>' <three files>` returns
   exactly one hit per file; no wire-specific logic landed in `client.rs`.
   → Red line 1 — a match arm alone is NOT a wire; all three points change
     together.

3. **Prove the spoke on the internal IR.** Add wiremock replay fixtures and a
   round-trip test for the new spoke to the seam suite, register it in the
   cross-wire table, and assert on captured outbound request bodies (see
   Testing & verification below). Verify: new-wire events synthesize on
   `ResponseItem`; `Completed` carries `usage_metadata: None` unless the
   protocol actually delivers usage.
   → Red line 9 — streams are parsed per-wire; events are synthesized on
     the internal surface, never raw SSE bytes (+ Red line 10 for the
     `usage_metadata` contract).

4. **Declare the vocabulary and the degrade surface.** Add CONTEXT.md entries
   (usage + prohibited-usage pairs) for the new wire and list explicitly what
   it does NOT support. No silent pretending a capability exists on this
   wire; auto-compact and other cross-wire features are localized per wire.
   Verify: every wire-specific term is in CONTEXT.md before any code that
   uses it lands.
   → Red line 11 — degrade or localize explicitly; never silently pretend
     a capability exists on this wire.

5. **Land the decision and close the sync loop.** Write the Nygard ADR
   (increments the `NNNN-` counter from ADR-0001), refresh FORK_DIVERGENCE
   (three new entries per ADR-0006 decision 4, with merge-base anchor), and
   wait for fork-health (wiremock trio + check job) to go green. Verify: the
   new loop terminates on the protocol's own stop condition, not on a
   stream-end shortcut.
   → Red line 12 — an early stop is data, not a stream-end shortcut (+ Red
     line 13 if the wire touches `reasoning_effort` translation).

**Does NOT carry over.** Red lines 3–8 are wire-specific invariants
(Anthropic thinking signature, Anthropic-only `cache_control`, base64-only
images, Anthropic tool-use replay). Do NOT transplant them onto a new wire —
write only the CONTEXT.md vocabulary and red lines the new protocol
actually needs.

### Stable cross-references (link, don't copy)

| What | Lives in | Cite as |
|---|---|---|
| Three registration points (seam anchors) | `Boundary` above + `## Red lines` §1 | boundary + Red line 1 |
| Five-step blueprint narrative & why | [README.md](../../../../../README.md#how-to-add-a-4th-wire) | README §How to add a 4th wire |
| Per-wire vocabulary (ResponseItem, three registration points, apply-adapt-ignore) | [CONTEXT.md](../../../../../CONTEXT.md) | CONTEXT.md → core vocabulary |
| Fork baseline + merge strategy | [ADR-0001](../../../../../docs/adr/0001-fork-baseline-and-sync.md) | ADR-0001 §Decision |
| Seam attribution + queue entries | [ADR-0006](../../../../../docs/adr/0006-fork-divergence-patch-queue.md) | ADR-0006 §4 (seam anchors) |
| Inbound SSE state machine pair | `codex-api/src/sse/<new>.rs` + Testing & verification below | Testing & verification |

## Module discipline

- Root AGENTS.md Rust rules apply (500/800 LoC targets, exhaustive matches, no
  single-use helpers). If a file here grows past target, add a module — do not
  widen it.
- Doc comments on the tricky invariants (alternation, `{}`-not-null, image
  fallback, signature replay) are load-bearing — keep them in sync with
  behavior changes.

## Testing & verification

- Fast loop: `just test -p codex-core` (wiremock suites:
  `core/tests/suite/chat_wiremock.rs`, `core/tests/suite/anthropic_wiremock.rs`).
  SSE-frame unit tests: `just test -p codex-api`.
- Baseline acceptance trio — new wire behavior extends or mirrors these:
  - `chat_wire_tool_call_roundtrip`
  - `chat_wire_plain_text_turn`
  - `anthropic_wire_tool_call_roundtrip`
- Assert on outbound request bodies captured by wiremock — assert what the
  wire sends, not just that the stream completed.
- No wire-specific env vars locally; live smoke tests need a real gateway +
  provider `env_key` (README three-wire usage guide).

## Sync-time rules (upstream merge)

- These files are fork-only: upstream never modifies them; they are the
  designed conflict absorber for the fork seam.
- After a sync: grep the three registration points; confirm no wire logic was
  pulled back into `client.rs` to resolve a conflict.
- Re-check `handle_unauthorized` call-site plumbing if upstream changes its
  signature (ADR-0002 Ruling 3 pattern); record the merge's rulings in a new
  `docs/adr/NNNN-*.md` (merge, never rebase).

## Out of scope here

- Fork map / commands / sync cadence → `codex-rs/AGENTS.md`.
- Rust style rules / TUI conventions → repo-root AGENTS.md.
- Protocol type definitions → codex-api models; the `WireApi` enum and
  provider config → model-provider-info. Edit those in their homes, not here.
