# ADR-0002: Rulings from merging upstream main (94311d44)

- Status: Accepted
- Date: 2026-09-02
- Related: ADR-0001 (fork baseline and three-registration-point discipline; this ADR records that baseline's first merge rulings)

## Status

Accepted.

## Context

Merging `upstream/main` into `feat/tri-wire-api` brings upstream's event-surface
changes that landed after commit `94311d44` ("Forward history note images to the
model (#41292)"): `handle_unauthorized` in `core.rs` gained `event_sender` and
`turn_id` parameters for auth-recovery events, and the Responses `Completed`
event gained a `usage_metadata` field. Reconciling these with the fork's
tri-wire additions required three rulings.

## Decision

1. **Keep fork constants.** The fork's wire constants — `CHAT_COMPLETIONS_ENDPOINT`
   (`/chat/completions`), `ANTHROPIC_MESSAGES_ENDPOINT` (`/messages`), and
   `DEFAULT_ANTHROPIC_MAX_TOKENS` (`8192`) in `codex-rs/core/src/client.rs`, plus
   `ANTHROPIC_VERSION` (`2023-06-01`) in
   `codex-rs/codex-api/src/endpoint/messages.rs` — are preserved as-is; upstream
   has no Chat/Anthropic equivalents to adopt. `DEFAULT_ANTHROPIC_MAX_TOKENS`
   carries a known PONYTAIL: the fixed output budget truncates long Claude turns
   (`stop_reason=max_tokens`).

2. **`usage_metadata = None` on non-Responses wires.** Upstream added
   `usage_metadata` to the Responses `Completed` event. The fork's Chat
   (`codex-rs/codex-api/src/sse/chat_completions.rs`) and Anthropic Messages
   (`codex-rs/codex-api/src/sse/messages.rs`) wires synthesize `Completed` with
   `usage_metadata: None` because neither wire has an equivalent field; the
   Responses wire continues to pass `usage_metadata` through unchanged.

3. **`handle_unauthorized` gains `event_sender`/`turn_id`.** Upstream added two
   parameters to `handle_unauthorized` for emitting auth-recovery events. The
   fork's call sites in `codex-rs/core/src/client.rs` were updated to pass
   `self.client.event_sender.as_ref()` and `responses_metadata.turn_id.as_deref()`
   rather than dropping the new event plumbing.

## Consequences

Positive:

- The fork seam survives the merge without drift on the wire dispatch.
- The Chat/Messages `Completed` surface stays self-consistent: `usage_metadata`
  is genuinely absent on those wires, not silently wrong.

Negative:

- The 8192-token Anthropic output budget remains hardcoded; it is tracked as a
  PONYTAIL to be sourced from per-model config later.
- Passing the new `event_sender`/`turn_id` through every `handle_unauthorized`
  call site is a recurring merge cost on future upstream syncs.
