//! Fork-only local state maintenance subcommands (tickets 31/35).
//!
//! The single sub-tree today is `codex state fix-checksums`, the only
//! sanctioned rewrite entry point under the ticket 35 default (`auto`):
//! it restores the migration checksum history to any line-ending family
//! so the fork runtime and the official openai/codex CLI of the same
//! platform family share one home (D-015, D-004). All write paths are
//! gated by the six preconditions documented in D-015; the explicit
//! `[state] migration_checksum_family` startup maintenance enforces the
//! same gates in-process.

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
    /// local state databases (e.g. restore a legacy LF-family database
    /// to the official platform family this binary embeds, per the
    /// startup fail-loud message). The six-precondition gate from D-015
    /// is enforced across every runtime database before anything is
    /// written; dry run is the default.
    FixChecksums(FixChecksumsCommand),
}

#[derive(Debug, Parser)]
pub(crate) struct FixChecksumsCommand {
    /// Target line-ending family. Required so the run never has to
    /// guess: on the official platform family, `crlf` matches every
    /// Windows build (fork and official) and `lf` every Linux/macOS
    /// build; the startup fail-loud message names the family to pass.
    /// `lf` on Windows is the ticket 35 escape hatch (simulate or
    /// restore the legacy family; also the pre-state used by the CI
    /// end-to-end coexistence case).
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
