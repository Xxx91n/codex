# Release notes - idempotent rolling-tag publishing (ticket 36)

Status: for the fork test-release pipeline (.github/workflows/fork-cli-test-release.yml,
`release` job). Required by A-029 / D-007: the rolling prerelease publish is now an
idempotent upsert, and the manual "delete tag, re-dispatch" recovery runbook is retired.

## The tri-wire-test tag is a ROLLING tag - do not pin it

* `tri-wire-test` (and its prerelease) is force-moved to the newest green full run by
  the refs API on every publish. The download entry points are stable and always serve
  the latest build:

  ```
  https://github.com/Xxx91n/codex/releases/download/tri-wire-test/codex-win-x86_64-pc-windows-msvc.zip
  https://github.com/Xxx91n/codex/releases/download/tri-wire-test/codex-linux-x86_64-unknown-linux-gnu.tar.gz
  https://github.com/Xxx91n/codex/releases/download/tri-wire-test/codex-mac-aarch64-apple-darwin.tar.gz
  ```

* Do not pin a commit SHA, a release id, or an asset id under this tag: any of them may
  move or be replaced by the next green run. Pin a SHA only if you need a frozen build,
  in which case download once and keep the file yourself.

## Publishing is idempotent (B1 root-cause fix)

* The publish step performs no destructive ref operations at all. It (a) force-moves
  the tag via a single refs-API PATCH (creating the ref when absent), (b) heals an
  existing release in place (`gh release edit --draft=false --prerelease`) or creates one
  when missing, (c) overwrites same-name assets via `gh release upload --clobber`, and
  (d) serializes concurrent publishes through a job-level `concurrency` group without
  cancel-in-progress.
* Why: the previous remove-and-recreate flow raced GitHub async tag GC - a late
  removal event could force the freshly created release back to draft, so a green run
  did not guarantee a downloadable asset (the "run green, download 404" symptom;
  cli/cli#5024 / #8458).
* **Retired procedure:** manually deleting the `tri-wire-test` tag (or release) before
  re-dispatching is NO LONGER needed and must not be performed. Re-dispatching
  `fork-cli-test-release` (platforms=all) at any state - tag present, release present,
  release drafted, or both absent - converges to a published release on the newest commit.
* Off-name leftovers: if an asset name ever changes across generations, old-named assets
  simply remain attached to the rolling release (never removed automatically); the live
  download contract is the fixed per-OS names listed above.
