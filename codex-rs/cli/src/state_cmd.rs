//! Fork-only local state maintenance subcommands (ticket 31).
//!
//! The single sub-tree today is `codex state fix-checksums`, which
//! mirrors the in-process self-heal in codex-state for users who want
//! to switch between the fork runtime and the official Windows CLI of
//! the same commit (D-015). All write paths are gated by the six
//! preconditions documented in D-015 and the same gates are enforced
//! by the runtime startup flip.

use anyhow::Context;
use clap::Parser;
use codex_core::config::ConfigBuilder;
use codex_state::ChecksumFamily;
use codex_state::FixStatus;
use codex_state::fix_migration_checksum_families;
use codex_utils_cli::CliConfigOverrides;
use serde::Serialize;

#[derive(Debug, Parser)]
pub(crate) struct StateCommand {
    #[command(subcommand)]
    pub subcommand: StateSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub(crate) enum StateSubcommand {
    /// Repair or flip the EOL sqlx migration checksum family of the
    /// local state databases so the official openai/codex CLI of the
    /// same commit can open them without its own self-heal (and
    /// vice versa). The six-precondition gate from D-015 is enforced
    /// across every runtime database before anything is written; dry
    /// run is the default.
    FixChecksums(FixChecksumsCommand),
}

#[derive(Debug, Parser)]
pub(crate) struct FixChecksumsCommand {
    /// Target line-ending family. Required so the run never has to
    /// guess: `crlf` rewrites every row to the CRLF image of the
    /// embedded SQL (so the official Windows CLI of the same commit
    /// can open the database without ceremony); `lf` rewrites every
    /// row to the embedded (LF) checksum (so the fork runtime can
    /// open a database last touched by the official Windows CLI).
    #[arg(long, value_enum)]
    pub family: FixChecksumsFamily,

    /// Apply the rewrite. Without this flag the command runs in dry
    /// run mode, prints a JSON report of what would change, and does
    /// not touch any database.
    #[arg(long)]
    pub apply: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum, Serialize)]
#[clap(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub(crate) enum FixChecksumsFamily {
    Lf,
    Crlf,
}

impl From<FixChecksumsFamily> for ChecksumFamily {
    fn from(family: FixChecksumsFamily) -> Self {
        match family {
            FixChecksumsFamily::Lf => ChecksumFamily::Lf,
            FixChecksumsFamily::Crlf => ChecksumFamily::Crlf,
        }
    }
}

pub(crate) async fn run_fix_checksums(
    cmd: FixChecksumsCommand,
    config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let overrides = config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let config = ConfigBuilder::default()
        .cli_overrides(overrides)
        .build()
        .await
        .context("failed to build config for state fix-checksums")?;
    let target: ChecksumFamily = cmd.family.into();
    let report = fix_migration_checksum_families(&config.sqlite, target, cmd.apply)
        .await
        .context("state fix-checksums failed")?;
    let json = serde_json::to_string_pretty(&report)
        .context("failed to serialize state fix-checksums report")?;
    println!("{json}");
    let any_rejected = report
        .databases
        .iter()
        .any(|d| matches!(d.status, FixStatus::Rejected));
    if any_rejected {
        anyhow::bail!(
            "one or more databases failed the precondition gate; see the JSON report above for the per-database reason"
        );
    }
    Ok(())
}
