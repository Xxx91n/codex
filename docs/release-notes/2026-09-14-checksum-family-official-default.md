# Release notes draft — checksum family regression (ticket 35)

Status: DRAFT for the next fork release (mirrored into the rolling prerelease body
in .github/workflows/fork-cli-test-release.yml). Required by A-023 / ADR-0011:
the default behavior change and the one-time migration command are stated up front.

## Migration checksum family: back to the official platform default

* Fork builds now embed the same migration-checksum family as official
  openai/codex builds of the same platform: **CRLF on Windows, LF on
  Linux/macOS**. The ticket 25 six-directory LF lock is retired. Switching
  between the fork and the official CLI on one `~/.codex` home no longer
  triggers the `VersionMismatch` family wall.
* New behavior: the default `[state] migration_checksum_family = "auto"`
  follows the binary's embedded family and **never rewrites the databases
  automatically**. If your databases were stamped with the legacy LF family,
  startup fails loudly and prints the exact one-time repair command:

  ```sh
  codex state fix-checksums --family crlf --apply
  ```

  (on Linux/macOS binaries the named family is `lf`). The command validates
  all six runtime databases behind the six-precondition gate, writes
  pre-flip backups, and is dry-run by default.
* The first detection of a drift fingerprint also shows a one-time notice
  (recorded in `~/.codex/.checksum-family-notices.json`, re-armed when the
  database family, binary family or platform changes;
  `CODEX_DISABLE_CHECKSUM_FAMILY_NOTICE=1` silences it).
* Explicit opt-ins unchanged from ticket 31: `[state]
  migration_checksum_family = "crlf" | "lf"` keep the databases in the named
  family across startups; `--family lf` remains the escape hatch.
* CI now asserts the family in three layers: per-platform checkout bytes
  (fork-health `platform-family-assert`), release-binary SHA-384 digest scan
  (fork-cli-test-release build jobs), and an end-to-end drill proving the
  official Windows release binary opens a fork-restored database on an
  isolated CODEX_HOME (fork-cli-test-release `e2e-official-coexistence`).
* Runbook / FAQ: [docs/fork-checksum-family.md](../fork-checksum-family.md).
  Decision record: ADR-0011.

## Upgrading from a ticket-31 era config

If you previously set `[state] migration_checksum_family = "crlf"` as a
bridge while the LF lock was in force, you can remove the setting after
upgrading: the new default keeps Windows databases in CRLF on its own. A
one-time `codex state fix-checksums --family crlf --apply` is still needed
for databases that are still stamped in the legacy LF family.