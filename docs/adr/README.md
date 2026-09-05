# Architecture Decision Records — Index

Decision log for the `feat/tri-wire-api` fork (Nygard template:
Status / Context / Decision / Consequences). Append-only per ADR-0001:
accepted records are not edited; a changed decision writes a new record
that supersedes and links the old one, incrementing the `NNNN` counter.

| ADR | Status | Decision (one line) |
|---|---|---|
| [0001](0001-fork-baseline-and-sync.md) | Accepted | Fork baseline: merge-don't-rebase sync, three registration points, ADR-per-ruling discipline |
| [0002](0002-merge-94311d44-rulings.md) | Accepted | Upstream 94311d44 merge rulings: keep fork wire constants, `usage_metadata: None` on non-Responses wires, `handle_unauthorized` plumbing |
| [0003](0003-gemini-wire-verdict.md) | Accepted | No native Gemini wire (OpenAI-compatible endpoint covers it); reopen triggers R1–R3 |
| [0004](0004-chat-reasoning-content-passthrough.md) | Accepted | Chat `reasoning_content` passthrough reusing `ResponseItem::Reasoning` (no new IR variant) |
| [0005](0005-reasoning-effort-translation.md) | Accepted | `reasoning_effort` → chat top-level field / Anthropic adaptive-vs-manual thinking translation (pure, deterministic) |
| [0007](0007-major-upgrade-playbook.md) | Accepted (contingency playbook) | Upstream 1.0 major-upgrade playbook: break-surface preview, registration-point impact template, controlled full-CI verification window, frozen baseline & EOL clauses |

Term definitions for vocabulary used across ADRs live in the repo-root
[CONTEXT.md](../../CONTEXT.md) (single vocabulary authority). Agent-facing
constraints: root [AGENTS.md](../../AGENTS.md) ("Fork delta: tri-wire-api"),
then the nested chain `codex-rs/AGENTS.md` →
`core/src/client/wire/AGENTS.md`.
