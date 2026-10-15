# ADR-0001: Fork baseline and upstream sync strategy

- Status: Accepted
- Date: 2026-09-02

## Status

Accepted.

## Context

This repository is a fork of `openai/codex` that restores and natively
implements three outbound wire protocols — Responses, Chat Completions, and
Anthropic Messages — on a single internal representation (`ResponseItem`).
Upstream ships only the Responses wire; the Chat wire was removed upstream in
Feb 2026 (PR #10157) and the Anthropic Messages wire is a fork addition
(goose-blueprint in-process transport).

The fork diverges from upstream by ~43 files (+4013/-9), concentrated in
`codex-api` (new endpoint + SSE, ~1400 lines), `core/client.rs` (+747),
`core/client_tests.rs` (+419), `tools/tool_spec`, `model-provider-info`, and the
wiremock test suites. Without a documented baseline, each upstream sync risks
re-deriving the same decisions, and each merge re-litigates the fork seam.

## Decision

- Track `upstream/main` (`openai/codex`) as the sync base; `origin` is this fork.
- Merge upstream, never rebase. Rebase rewrites the fork seam and destroys merge
  auditability; merging keeps the fork's divergence as a reviewable delta.
- Sync on a weekly cadence or per upstream release, whichever comes first.
- Confine fork divergence to three registration points — `WireApi`,
  `ModelProviderInfo.wire_api`, and the `client.rs` dispatch `match` — and put
  all wire-specific logic in new per-wire modules rather than growing `client.rs`.
- Record every upstream merge's rulings as a new Nygard ADR in `docs/adr/`,
  incrementing the counter (`NNNN-*.md`). ADRs live in the repository; heavy
  architecture notes stay in `D:/Aworker/codex/docs/` (outside the repo).

## Consequences

Positive:

- The fork's divergence stays reviewable and auditable across merges.
- Merge rulings become durable knowledge, not chat history that evaporates.
- The three-registration-point discipline keeps upstream drift from colliding
  with fork wire variants.

Negative:

- Merge commits accumulate; history is noisier than a rebased fork.
- The registration-point discipline is a permanent constraint that review must
  keep enforcing.
