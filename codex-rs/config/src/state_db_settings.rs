//! Local state database settings (fork-only; see the round-7 ticket 31
//! handoff and docs/fork-checksum-family.md for context).
//!
//! These settings are fork-only because the official openai/codex has no
//! public state fix-checksums subcommand and no need to keep the
//! migration checksum family in lockstep with a fork runtime. Both the
//! config item and the matching CLI subcommand are part of ticket 31's
//! free-switch capability (D-015). Since ticket 35 the migration_checksum_
//! family setting defaults to auto, which follows the family this binary
//! embeds (the official platform family: CRLF on Windows builds, LF on
//! Linux/macOS builds): a family mismatch fails loudly with the repair
//! command and the runtime rewrites nothing on its own. crlf and lf are
//! explicit opt-in maintenance targets.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MigrationChecksumFamily {
    #[default]
    Auto,
    Lf,
    Crlf,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct StateDbSettingsToml {
    /// Maintenance target for the _sqlx_migrations checksum family.
    /// Defaults to auto: follow the family this binary embeds, which is
    /// the official platform family (CRLF on Windows builds, LF on
    /// Linux/macOS). Under auto a family mismatch fails loudly with the
    /// repair command and nothing is rewritten automatically. crlf and lf
    /// are explicit opt-in maintenance targets applied by the startup
    /// flip; the offline flip uses the matching codex state fix-checksums
    /// --family ... subcommand.
    pub migration_checksum_family: Option<MigrationChecksumFamily>,
}
