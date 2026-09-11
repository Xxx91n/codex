# State migration checksum family: bidirectional switch

This document describes the fork-only maintenance path that lets a user
freely switch between the openai/codex Windows CLI and the fork
runtime on the same ~/.codex directory. The mechanism was added in
round 7 (ticket 31) per grill ledger D-015.

## Why this exists

sqlx::migrate! embeds the migration .sql bytes and their SHA-384
checksums at compile time. Those bytes depend on the line endings of
the checkout, so a CRLF checkout and an LF checkout produce two
incompatible checksum families. A database written by one family is
rejected by the other with MigrateError::VersionMismatch.

* Before ticket 18: pre-normalization Windows binaries used
  core.autocrlf=true and produced CRLF-family databases.
* After ticket 18: the repository's .gitattributes pinned every
  migration file to LF, so every Windows build from this checkout
  produces LF-family databases.
* The official openai/codex Windows CLI of the same commit ships a
  CRLF family; the fork runtime ships an LF family. A user who
  pointed CODEX_CLI_PATH at the fork runtime and then back at the
  official Windows CLI hit "failed to initialize sqlite state
  runtime under ...\\.codex" (D-012).

Ticket 25's startup self-heal rewrote the family in one direction
only (CRLF to LF). Ticket 31 adds the reverse direction and the
offline maintenance path that lets the user re-stamp the family
on demand, with a JSON report and a dry-run default.

## The codex state fix-checksums subcommand

The subcommand lives in codex-rs/cli/src/state_cmd.rs and is
registered in codex-rs/cli/src/main.rs under the top-level "state"
subcommand.

    codex state fix-checksums --family <lf|crlf> [--apply]

* --family is required (no implicit guess).
* Without --apply, the command runs in dry-run mode: it validates
  every runtime database, prints a JSON report, and does not touch
  anything. The exit code is 0 when every database is either
  not_found, empty, no_change, or dry_run, and non-zero when any
  database lands in rejected.
* With --apply, every database that passed the precondition gate
  has a backup written next to it as
  <dbname>.pre-checksum-flip-<lf|crlf>.bak (after a WAL checkpoint
  to make the copy consistent) and the row update transaction
  runs. The backup is the reversibility artifact: if the
  transaction fails, the database is unchanged and the backup file
  contains the prior state.

### Six preconditions (any one missing -> reject)

For every runtime database (state, logs, goals, memories, queue,
thread history) the offline flip refuses to write if any of the
following fails. The same six gates apply to the in-process
self-heal and the post-migration startup flip.

1. Row-level family criterion. Every applied row's checksum must
   equal the embedded (LF) checksum or the CRLF image of the
   embedded SQL. An applied row whose version is unknown to this
   binary is refused outright.
2. Normalized SHA-384 fingerprint equality. A row whose checksum
   matches neither family means real drift; the command refuses.
   By construction, matching either family means the underlying SQL
   is byte-identical modulo line endings.
3. Schema consistency gate. The SchemaReplay DDL walker covers all
   six runtime databases; the actual sqlite_master inventory must
   match the replayed expectations (missing or mistyped objects
   fail).
4. Version set match. The applied version set must equal the
   embedded version set: a database behind the binary or ahead of
   it is refused. (The startup self-heal tolerates subset; the
   offline flip is stricter because the goal is to make a paired
   official binary of the same commit open the database.)
5. Atomic and reversible. Apply creates a backup file next to the
   database; an existing backup file at that name is refused (we
   never overwrite a previous pre-flip snapshot). The row updates
   run in a single transaction. Re-running the command is a no-op
   when the database is already in the target family (idempotency).
6. Concurrency boundary. Apply takes a BEGIN IMMEDIATE writer
   transaction under the pool's busy timeout. If another Codex
   process is using the database, the BEGIN blocks until busy
   timeout and then errors with SQLITE_BUSY; the command refuses
   rather than queueing.

## The [state] migration_checksum_family config item

    [state]
    migration_checksum_family = "auto"  # default

* "auto" (default) preserves the historical behavior: the
  in-process self-heal rewrites any CRLF-stamped database to the
  embedded (LF) family on VersionMismatch, and no startup rewrite
  is performed. Zero behavior change vs. the pre-ticket-31
  baseline.
* "lf" is an explicit alias for "auto". Named for symmetry with
  the CLI flag and the JSON report.
* "crlf" keeps the local databases in the CRLF family across
  startups. After every successful LF-embedded migration run the
  runtime rewrites every row in the same transaction to the CRLF
  image, so the official Windows CLI of the same commit can open
  the database without its own self-heal. The next fork startup
  will see VersionMismatch and run the LF self-heal before
  flipping back to CRLF, which is the documented maintenance loop.

The startup path uses the same six gates as the offline
subcommand; the runtime refuses to write if any precondition
fails (the resulting RuntimeDbInitError mentions the
maintain_crlf_checksum_family phase so the failure is
distinguishable from a migration failure).

## Operational recipe (bidirectional switch)

1. User runs the fork runtime; the in-process self-heal normalizes
   the database to LF.
2. User wants to try the official Windows CLI of the same commit:
   * codex state fix-checksums --family crlf --apply
   * The subcommand runs dry-run by default; re-run with --apply
     after reviewing the JSON report.
3. After the official CLI session, the user wants to come back to
   the fork runtime:
   * codex state fix-checksums --family lf --apply
4. To make the round-trip seamless across every restart with the
   official Windows CLI as the default, set
   [state] migration_checksum_family = "crlf" in the fork
   runtime's config.toml.

## JSON report shape

The subcommand prints a FixChecksumsReport on stdout
(serde_json::to_string_pretty). Top-level fields:

    {
      "target_family": "crlf",
      "applied": true,
      "databases": [
        {
          "label": "state DB",
          "path": "/home/user/.codex/state_5.sqlite",
          "status": "rewritten",
          "detected_family": "lf",
          "applied_version_count": 52,
          "embedded_version_count": 52,
          "rewritten_versions": [1, 2, 3, "..."],
          "reason": null
        }
      ]
    }

Per-database status values:

* not_found - the database file does not exist (not an error).
* empty - the database has no _sqlx_migrations rows.
* no_change - the history is already in the target family.
* dry_run - a rewrite would be required; nothing was written.
* rewritten - rows were rewritten in --apply mode.
* rejected - one of the six preconditions failed; the reason
  field carries a stable code
  (unknown_migration_version_N, schema_drift: ...,
  version_set_mismatch ..., not_an_eol_only_difference_...,
  backup ... already exists, or the SQLITE_BUSY translation).

## What is out of scope

* The mechanism does not run a migration. It only rewrites
  checksums. codex state fix-checksums --family crlf --apply on
  a database that is one migration behind the embedded set
  refuses with version_set_mismatch; the user should run Codex
  normally (any binary) first to bring the database up to date,
  then re-run the flip.
* The mechanism does not move the schema. The schema is verified
  against the embedded migrations; if a fork-only schema
  migration has been added (e.g. a new table not in the official
  migration set), the schema_drift reason will refuse the flip.
* The mechanism does not auto-flip on startup. auto / lf keep
  the historical one-way self-heal; crlf requires an explicit
  config setting or an explicit codex state fix-checksums
  --family crlf --apply invocation.
