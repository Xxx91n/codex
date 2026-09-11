//! Local state database settings (fork-only; see the round-7 ticket 31
//! handoff for context).
//!
//! These settings are fork-only because the official openai/codex has no
//! public state fix-checksums subcommand and no need to keep the
//! migration checksum family in lockstep with a fork runtime. Both the
//! config item and the matching CLI subcommand are part of ticket 31's
//! free-switch capability (D-015). The migration_checksum_family
//! setting defaults to auto, which preserves the historical one-way LF
//! self-heal: the runtime normalizes any CRLF-stamped database to the
//! embedded (LF) family on VersionMismatch and never writes CRLF on
//! its own.

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
    /// Defaults to auto. The offline flip uses the matching
    /// codex state fix-checksums --family ... subcommand; this
    /// setting controls the in-process self-heal and the
    /// post-migration startup flip.
    pub migration_checksum_family: Option<MigrationChecksumFamily>,
}
