# State migration checksum family: official default + recovery runbook

This document is the authoritative runbook for the `codex-rs/state`
migration checksum family mechanism: what the fork embeds by default,
how the official openai/codex CLI and the fork runtime coexist on one
`~/.codex` home, and how to recover a database stranded in the wrong
family. History: round 7 ticket 31 added the bidirectional switch
(D-015); round 8 ticket 35 (ADR-0011) retired the ticket 25 LF lock and
re-turned the default to the official platform family (D-001/D-004).

## Why a family exists at all

`sqlx::migrate!` embeds the migration `.sql` bytes and their SHA-384
checksums at compile time, so the embedded family depends on the line
endings of the build checkout. A CRLF checkout and an LF checkout
produce two incompatible checksum families, and sqlx rejects a database
written by the other family with `MigrateError::VersionMismatch` before
applying anything.

The official openai/codex Windows CLI is built from a Windows checkout
whose default `core.autocrlf=true` stores the migration sources with
CRLF: official Windows builds embed the CRLF family, official Linux/
macOS builds embed LF. Since ticket 35 the fork matches that exactly:
.gitattributes keeps the six migration directories at plain `text
!eol` (checkout follows the platform git config), and the fork embeds
the same platform family as an official build of the same commit. The
ticket 25 arrangement (six directories pinned to LF via `eol=lf` plus
an LF self-heal at startup) retired with ADR-0011: it was what locked
users out of the official CLI on a shared home (the D-012 incident).

CI enforces the defense in two layers:

* `fork-health` job `platform-family-assert` checks the per-platform
  checkout bytes of the six directories (Windows runner CRLF, Linux
  runner LF), fail-loud, daily.
* `fork-cli-test-release` build jobs scan the release binary for the
  SHA-384 digests of the checked-out `.sql` bytes (the authoritative
  artifact layer), and the `e2e-official-coexistence` job runs the full
  drift/fail-loud/restore drill against the real official Windows
  release binary on a throwaway `CODEX_HOME`.

## Default behavior (`[state] migration_checksum_family = "auto"`)

`auto` follows the family this binary embeds and never rewrites
anything automatically. New databases are created in the platform
family; switching between the fork and an official binary of the same
platform and commit costs nothing.

If the databases under the home carry the other family (typically a
legacy database from the LF-lock era, or a home moved across
platforms), startup fails loudly with a read-only diagnosis:

```text
checksum family mismatch: stored migrations are in the lf family, this
binary embeds the crlf family (line-ending-only difference; SQL
contents verified identical).
note: under the default auto behavior the runtime never rewrites
migration checksums automatically, because the same databases are
shared with the official openai/codex CLI.
help: re-stamp the databases to this binary's embedded family:
help:   codex state fix-checksums --family crlf --apply
help: or switch to a binary from the database's family: Windows builds
embed crlf, Linux/macOS builds embed lf; the restore runbook is
docs/fork-checksum-family.md.
```

The diagnosis is strictly read-only and strictly EOL-shaped: when a
stored checksum matches neither line-ending image (real drift or
tampering) or an applied version is unknown to the binary, sqlx's own
`VersionMismatch` error stands and no repair command is offered.

The first time a given drift fingerprint (database family, binary
family, platform) is detected, a one-time notice block is also printed
and recorded in `$CODEX_HOME/.checksum-family-notices.json`. Any
fingerprint change re-arms the notice; set
`CODEX_DISABLE_CHECKSUM_FAMILY_NOTICE=1` to silence it (the startup
error keeps its help lines either way).

## Explicit opt-in maintenance (`"crlf"` / `"lf"`)

Ticket 31's maintenance behavior is unchanged: with an explicit family,
on startup `VersionMismatch` the runtime first re-validates the history
into this binary's embedded family, runs the migration, then lands the
whole history in the configured family (same six gates as the offline
subcommand, one transaction; the error phase reads
`maintain_crlf_checksum_family` / `maintain_lf_checksum_family`). This
is the documented maintenance loop and only ever runs on an explicit
opt-in.

`"lf"` is also the escape hatch (A-025): it keeps a database in the
legacy family if the official project ever moves to LF canonical
(openai/codex#38528 direction), and the CI e2e drill uses it to
simulate the legacy drift.

## The `codex state fix-checksums` subcommand

    codex state fix-checksums --family <lf|crlf> [--apply]

* `--family` is required (no implicit guess); pass the family the
  startup fail-loud message names.
* Without `--apply`: dry-run. Validates every runtime database, prints
  a JSON report, touches nothing. Exit code 0 when every database is
  `not_found`, `empty`, `no_change`, or `dry_run`; non-zero when any
  database lands in `rejected`.
* With `--apply`: every database that passed the precondition gate
  gets a backup written next to it as
  `<dbname>.pre-checksum-flip-<lf|crlf>.bak` (after a WAL checkpoint,
  so the copy is consistent) and the row-update transaction runs.
* `--apply --json`-style scripting: the report is the stdout JSON, so
  CI and multi-machine fleets can pipe it into `jq`.

### Six preconditions (any one missing -> reject)

For every runtime database (state, logs, goals, memories, queue, thread
history) the flip refuses to write if any of the following fails. The
same gates apply to the explicit opt-in startup maintenance.

1. Row-level family criterion. Every applied row's checksum must equal
   one of the two line-ending images of the embedded SQL. An applied
   row whose version is unknown to this binary is refused outright.
2. Normalized SHA-384 fingerprint equality. A row whose checksum
   matches neither image means real drift; the command refuses. By
   construction, matching either image means the underlying SQL is
   byte-identical modulo line endings.
3. Schema consistency gate. The SchemaReplay DDL walker covers all six
   runtime databases; the actual sqlite_master inventory must match the
   replayed expectations (missing or mistyped objects fail). The replay
   walks the history recorded as applied - not the whole embedded set -
   so a database with pending migrations is judged against its own
   subset's expectations (ticket 37 / A-030).
4. Version set inclusion (relaxed from equality by ticket 37 / A-030).
   The applied version set must be a subset of the embedded set. Three
   states, kept verbatim in sync with the P4 gate comment in
   `codex-rs/state/src/fix_checksums.rs`:
   * stored a proper subset of embedded - pending migrations, the
     normal upgrade state (the R1 field shape) - allowed. The flip
     restores the rows that exist, the JSON report names the
     embedded-only versions as `pending_version_count` /
     `pending_versions`, and the next normal startup absorbs them,
     stamping them in its own family - converging on the same steady
     state as an official binary.
   * stored equals embedded - fully migrated: behavior unchanged.
   * stored a strict superset - the database is ahead of this binary:
     refused whole, with the precise reason `db_ahead_unknown_versions`
     listing every unknown version, because a row with no embedded
     mirror can never prove its line-ending fingerprint. (The startup
     migrator tolerates a database ahead via `ignore_missing` -
     upstream codex PR #16924 - while an offline bookkeeping rewrite,
     like Flyway/Alembic/Atlas repair, aligns the ledger only to
     migrations it actually has.)
5. Atomic and reversible. Apply creates a backup file next to the
   database; an existing backup file at that name is refused (never
   overwriting a previous pre-flip snapshot). The row updates run in a
   single transaction. Re-running the command is a no-op when the
   database is already in the target family (idempotency, tested).
6. Concurrency boundary. Apply takes a `BEGIN IMMEDIATE` writer
   transaction under the pool's busy timeout. If another Codex process
   is using the database, the BEGIN blocks until the busy timeout and
   the command refuses rather than queueing.

## Recovery SOP (legacy LF database -> official family)

The DBA-style flow below is the sanctioned order of operations, modeled
on the official incident report openai/codex#23777 and the Flyway
repair discipline: explicit command, backups first, prove it on a
copy, only then touch the real database. The six runtime databases
(state/logs/goals/memories/queue/thread history) are ALL in scope —
family drift is never just `state_5.sqlite`.

0. Quiet window: no Codex/app-server process may be running (the
   concurrency-boundary precondition doubles as the SOP gate).
1. Full backup of every runtime database plus sidecars:
   copy `<db>.sqlite`, `<db>.sqlite-wal` and `<db>.sqlite-shm`
   together into a timestamped folder outside the home.
2. Pre-verify on a throwaway copy: copy the backup into a scratch dir,
   set `CODEX_HOME` to it (the directory must exist before the first
   launch), and run the flip there first: dry-run, review the JSON
   report, then `--apply`. The real databases stay untouched until
   step 4.
3. Verify the copy item by item:
   * `PRAGMA integrity_check` returns `ok` on every database;
   * user-data row counts (sessions, logs, goals, memories, queue,
     thread history) are unchanged;
   * every `_sqlx_migrations.checksum` byte-equals the CRLF image of
     the embedded migration (a `codex state fix-checksums --family
     crlf` dry-run reporting `no_change` per database proves this
     row-by-row);
   * the restored copy cold-boots under the official Windows binary
     AND under the fork without the state-runtime init error.
4. Apply to the real databases: `codex state fix-checksums --family
   crlf` (dry-run) -> review -> `codex state fix-checksums --family
   crlf --apply`. The subcommand itself writes the
   `pre-checksum-flip` backups as its reversibility artifact.
5. Official cold start + fork restart: launch the official CLI once,
   then the fork once. Both must pass the state-runtime init.

Historical note (pre-ticket-35 kernels): during the window between
restoring the databases and upgrading to a ticket-35 package, an old
fork kernel with `auto` = LF self-heal could flip the family back; set
`[state] migration_checksum_family = "crlf"` in that kernel's config
as the bridge. Since ticket 35, `auto` keeps the platform family on
its own and no bridge config is needed.

## Emergency FAQ

* The official CLI refuses to start after a fork session?
  Run `codex state fix-checksums --family crlf --apply` (fork CLI,
  Windows) once — this is the exact inverse of the incident that
  motivated ADR-0011, and the bidirectional flip is verified in CI.
* A fork startup prints `checksum family mismatch`?
  Follow the `help:` line verbatim — it names the family this binary
  embeds. It appears on legacy databases and after moving a home
  across platforms; it never appears for a freshly created database on
  a CI-verified build.
* Can the runtime just fix it at startup for me?
  No — that is a deliberate decision (ADR-0011): auto-rewriting a
  shared bookkeeping table has no published industrial precedent
  (#38528 is unmerged), hides who touched the database, and puts the
  failure surface on every startup. The explicit command is the
  supported path (Flyway repair model).
* Two binaries at once on one home?
  Never. WAL contention aside, keep the same-instant-one-kernel rule;
  the family regression made it less dangerous, not optional.
* The flip refuses with `not_an_eol_only_difference_at_version_N` or
  `db_ahead_unknown_versions unknown=[...]`?
  That is the tamper/real-drift gate protecting you (the second one
  means the database was migrated by a newer binary than the one
  running the flip), not a bug to route around: restore from the
  pre-flip backup and investigate before re-running.

## What is out of scope

* Running migrations: the flip only rewrites checksums. A database
  behind the binary is restorable as-is (ticket 37); its pending
  versions stay unapplied and the next normal startup of any Codex
  binary absorbs them (see precondition 4).
* Moving the schema: a fork-only schema migration not in the official
  set fails the `schema_drift` gate by design.
* Automatic family rewrites under `auto`: retired by ADR-0011; drift
  fails loud, the explicit command repairs.