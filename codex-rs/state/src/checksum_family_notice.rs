//! One-time migration checksum family drift notice (ticket 35, A-023).
//!
//! Under the default `auto` behavior a family drift fails loudly on every
//! startup (see `eol_checksum_repair::detect_checksum_family_drift`); this
//! module adds the softer first-detection guidance block: printed once per
//! (database family, binary family, platform) fingerprint and recorded under
//! the SQLite home so repeated startups do not nag. The marker stores the
//! fingerprint that was acknowledged, not just a boolean: any fingerprint
//! component changing re-arms the notice (the conda notices #16500 lesson:
//! a bare "seen" flag permanently mutes what was really a different event).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

/// JSON marker file kept in the SQLite (CODEX) home.
pub(crate) const NOTICE_FILE_NAME: &str = ".checksum-family-notices.json";

/// Set to any value other than "0" to silence the one-time notice. The
/// fail-loud startup error keeps its help lines either way.
pub(crate) const NOTICE_DISABLE_ENV: &str = "CODEX_DISABLE_CHECKSUM_FAMILY_NOTICE";

const FAMILY_MISMATCH_KEY: &str = "family-mismatch";

/// What exactly was acknowledged: the drift the user saw, verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DriftFingerprint {
    pub db_family: String,
    pub binary_family: String,
    pub platform: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SeenNotice {
    seen_at_epoch_secs: u64,
    db_family: String,
    binary_family: String,
    platform: String,
}

impl SeenNotice {
    fn matches(&self, fingerprint: &DriftFingerprint) -> bool {
        self.db_family == fingerprint.db_family
            && self.binary_family == fingerprint.binary_family
            && self.platform == fingerprint.platform
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct NoticeFile {
    notices: BTreeMap<String, SeenNotice>,
}

/// Pure env gate so it stays unit-testable without mutating the process
/// environment: any value except absent/empty/"0" disables the notice.
fn is_notice_disabled(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| value != OsStr::new("0") && !value.is_empty())
}

/// Pure decision: show when no record exists or the acknowledged drift
/// differs from the current one in any fingerprint component.
fn should_show(stored: Option<&SeenNotice>, fingerprint: &DriftFingerprint) -> bool {
    stored.is_none_or(|seen| !seen.matches(fingerprint))
}

fn marker_path(sqlite_home: &Path) -> PathBuf {
    sqlite_home.join(NOTICE_FILE_NAME)
}

fn read_marker(sqlite_home: &Path) -> NoticeFile {
    std::fs::read(marker_path(sqlite_home))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Record the acknowledged drift; returns whether the notice should be
/// shown now (the first detection of this exact fingerprint). Write failures
/// are swallowed into "show it anyway": a home we cannot write to should
/// still tell the user how to repair their databases.
fn record_family_notice(sqlite_home: &Path, fingerprint: &DriftFingerprint) -> bool {
    let mut file = read_marker(sqlite_home);
    let stored = file.notices.get(FAMILY_MISMATCH_KEY);
    if !should_show(stored, fingerprint) {
        return false;
    }
    let seen_at_epoch_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    file.notices.insert(
        FAMILY_MISMATCH_KEY.to_string(),
        SeenNotice {
            seen_at_epoch_secs,
            db_family: fingerprint.db_family.clone(),
            binary_family: fingerprint.binary_family.clone(),
            platform: fingerprint.platform.clone(),
        },
    );
    if let Ok(json) = serde_json::to_vec_pretty(&file) {
        let _ = std::fs::write(marker_path(sqlite_home), json);
    }
    true
}

/// Print the one-time block when appropriate. The full repair guidance
/// rides on the startup error itself; this block frames it as the family
/// regression it is and points at the runbook.
pub(crate) fn maybe_show_family_notice(
    sqlite_home: &Path,
    fingerprint: &DriftFingerprint,
    notice_block: &str,
) {
    if is_notice_disabled(std::env::var_os(NOTICE_DISABLE_ENV)) {
        return;
    }
    if record_family_notice(sqlite_home, fingerprint) {
        eprintln!("{notice_block}");
        log::warn!("{notice_block}");
    }
}

#[cfg(test)]
#[path = "checksum_family_notice_tests.rs"]
mod tests;
