//! Offline maintenance for the EOL sqlx migration checksum family.
//!
//! Mirrors the in-process self-heal in `eol_checksum_repair`, but applies
//! the full six-precondition gate documented in D-015 before touching any
//! database, runs every gate across every runtime database before deciding
//! to write, and produces a machine-readable JSON report.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;
use sqlx::Connection;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;

use crate::SqliteConfig;
use crate::eol_checksum_repair::ChecksumFamily;
use crate::eol_checksum_repair::ObjectKind;
use crate::eol_checksum_repair::SchemaReplay;
use crate::eol_checksum_repair::crlf_checksum;
use crate::eol_checksum_repair::repair_eol_checksum_family;
use crate::migrations::GOALS_MIGRATOR;
use crate::migrations::LOGS_MIGRATOR;
use crate::migrations::MEMORIES_MIGRATOR;
use crate::migrations::QUEUE_MIGRATOR;
use crate::migrations::STATE_MIGRATOR;
use crate::migrations::THREAD_HISTORY_MIGRATOR;

/// Per-database outcome of the maintenance run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixStatus {
    /// The database file does not exist on disk; nothing to flip.
    NotFound,
    /// The database exists but has no `_sqlx_migrations` rows.
    Empty,
    /// Dry-run: a rewrite would be required, but the database was not touched.
    DryRun,
    /// The history is already in the target family; no rewrite was required.
    NoChange,
    /// Apply: rows were rewritten into the target family.
    Rewritten,
    /// One of the six preconditions failed; the database was not modified.
    Rejected,
}

impl fmt::Display for FixStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::NotFound => "not_found",
            Self::Empty => "empty",
            Self::DryRun => "dry_run",
            Self::NoChange => "no_change",
            Self::Rewritten => "rewritten",
            Self::Rejected => "rejected",
        };
        f.write_str(s)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FixChecksumsDb {
    pub label: String,
    pub path: String,
    pub status: FixStatus,
    /// Line-ending family across the applied rows:
    ///   `"lf"` — every row carries the embedded (LF) checksum.
    ///   `"crlf"` — every row carries the CRLF image of the embedded SQL.
    ///   `"mixed"` — both families appear (e.g. the fork ran its migrator
    ///   on an LF-stamped database and a pre-ticket-18 official binary
    ///   appended CRLF-stamped rows).
    ///   `None` — the database has no applied rows to characterize.
    pub detected_family: Option<String>,
    pub applied_version_count: usize,
    pub embedded_version_count: usize,
    /// Versions that would be / were rewritten into the target family.
    pub rewritten_versions: Vec<i64>,
    /// Stable reason code for `FixStatus::Rejected`. `None` for other
    /// statuses.
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FixChecksumsReport {
    pub target_family: String,
    pub applied: bool,
    pub databases: Vec<FixChecksumsDb>,
}

struct DatabaseSpec {
    label: &'static str,
    path: PathBuf,
    migrator: &'static Migrator,
}

fn collect_database_specs(sqlite: &SqliteConfig) -> Vec<DatabaseSpec> {
    vec![
        DatabaseSpec {
            label: "state DB",
            path: sqlite.state_db_path(),
            migrator: &STATE_MIGRATOR,
        },
        DatabaseSpec {
            label: "log DB",
            path: sqlite.logs_db_path(),
            migrator: &LOGS_MIGRATOR,
        },
        DatabaseSpec {
            label: "goals DB",
            path: sqlite.goals_db_path(),
            migrator: &GOALS_MIGRATOR,
        },
        DatabaseSpec {
            label: "memories DB",
            path: sqlite.memories_db_path(),
            migrator: &MEMORIES_MIGRATOR,
        },
        DatabaseSpec {
            label: "queue DB",
            path: sqlite.queue_db_path(),
            migrator: &QUEUE_MIGRATOR,
        },
        DatabaseSpec {
            label: "thread history DB",
            path: sqlite.thread_history_db_path(),
            migrator: &THREAD_HISTORY_MIGRATOR,
        },
    ]
}

fn backup_path_for(path: &Path, target: ChecksumFamily) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(format!(".pre-checksum-flip-{}.bak", target.as_str()));
    path.with_file_name(name)
}

async fn read_applied(pool: &SqlitePool) -> anyhow::Result<Option<Vec<(i64, Vec<u8>)>>> {
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    if exists.is_none() {
        return Ok(None);
    }
    let rows: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await?;
    Ok(Some(rows))
}

fn detect_family(migrator: &Migrator, rows: &[(i64, Vec<u8>)]) -> Option<String> {
    let mut lf_count = 0usize;
    let mut crlf_count = 0usize;
    for (version, stored) in rows {
        let Some(migration) = migrator.migrations.iter().find(|m| m.version == *version) else {
            continue;
        };
        if stored.as_slice() == migration.checksum.as_ref() {
            lf_count += 1;
        } else if crlf_checksum(migration)
            .is_some_and(|image| stored.as_slice() == image.as_slice())
        {
            crlf_count += 1;
        }
    }
    match (lf_count, crlf_count) {
        (0, 0) => None,
        (l, 0) if l > 0 => Some("lf".to_string()),
        (0, c) if c > 0 => Some("crlf".to_string()),
        _ => Some("mixed".to_string()),
    }
}

fn reject_reason_version_set(migrator: &Migrator, applied: &[(i64, Vec<u8>)]) -> Option<String> {
    let mut embedded: Vec<i64> = migrator.migrations.iter().map(|m| m.version).collect();
    embedded.sort_unstable();
    let mut applied_versions: Vec<i64> = applied.iter().map(|(v, _)| *v).collect();
    applied_versions.sort_unstable();
    if applied_versions == embedded {
        return None;
    }
    let embedded_set: BTreeSet<i64> = embedded.iter().copied().collect();
    let applied_set: BTreeSet<i64> = applied_versions.iter().copied().collect();
    let missing: Vec<i64> = embedded_set.difference(&applied_set).copied().collect();
    let extra: Vec<i64> = applied_set.difference(&embedded_set).copied().collect();
    let mut reason = String::from("version_set_mismatch");
    if !missing.is_empty() {
        reason.push_str(&format!(" missing={missing:?}"));
    }
    if !extra.is_empty() {
        reason.push_str(&format!(" extra={extra:?}"));
    }
    Some(reason)
}

fn reject_reason_schema(
    migrator: &Migrator,
    actual: &BTreeMap<String, ObjectKind>,
) -> Option<String> {
    let mut replay = SchemaReplay::default();
    for migration in migrator.migrations.iter() {
        replay.apply_migration(migration.sql.as_str());
    }
    replay
        .objects
        .insert("_sqlx_migrations".to_string(), ObjectKind::Table);
    let mut problems: Vec<String> = Vec::new();
    for (name, expected_kind) in &replay.objects {
        match actual.get(name) {
            Some(actual_kind) if actual_kind != expected_kind => {
                problems.push(format!(
                    "{name}_as_{actual_kind:?}_but_migrations_build_{expected_kind:?}"
                ));
            }
            None => problems.push(format!("missing_{name}")),
            Some(_) => {}
        }
    }
    if problems.is_empty() {
        None
    } else {
        Some(format!("schema_drift: {}", problems.join(";")))
    }
}

async fn actual_schema_inventory(
    pool: &SqlitePool,
) -> anyhow::Result<BTreeMap<String, ObjectKind>> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT name, type FROM sqlite_master")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .filter(|(name, _)| !name.starts_with("sqlite_"))
        .filter_map(|(name, kind)| ObjectKind::from_sqlite_type(&kind).map(|k| (name, k)))
        .collect())
}

async fn open_pool_for(sqlite: &SqliteConfig, path: &Path) -> anyhow::Result<SqlitePool> {
    Ok(sqlite.open_read_write_pool(path).await?)
}

async fn validate_database(
    sqlite: &SqliteConfig,
    spec: &DatabaseSpec,
    target: ChecksumFamily,
) -> FixChecksumsDb {
    if !spec.path.exists() {
        return FixChecksumsDb {
            label: spec.label.to_string(),
            path: spec.path.to_string_lossy().into_owned(),
            status: FixStatus::NotFound,
            detected_family: None,
            applied_version_count: 0,
            embedded_version_count: spec.migrator.migrations.len(),
            rewritten_versions: Vec::new(),
            reason: None,
        };
    }
    let pool = match open_pool_for(sqlite, &spec.path).await {
        Ok(p) => p,
        Err(error) => {
            return FixChecksumsDb {
                label: spec.label.to_string(),
                path: spec.path.to_string_lossy().into_owned(),
                status: FixStatus::Rejected,
                detected_family: None,
                applied_version_count: 0,
                embedded_version_count: spec.migrator.migrations.len(),
                rewritten_versions: Vec::new(),
                reason: Some(format!("open_failed: {error}")),
            };
        }
    };
    let applied = match read_applied(&pool).await {
        Ok(None) => {
            pool.close().await;
            return FixChecksumsDb {
                label: spec.label.to_string(),
                path: spec.path.to_string_lossy().into_owned(),
                status: FixStatus::Empty,
                detected_family: None,
                applied_version_count: 0,
                embedded_version_count: spec.migrator.migrations.len(),
                rewritten_versions: Vec::new(),
                reason: None,
            };
        }
        Ok(Some(rows)) if rows.is_empty() => {
            pool.close().await;
            return FixChecksumsDb {
                label: spec.label.to_string(),
                path: spec.path.to_string_lossy().into_owned(),
                status: FixStatus::Empty,
                detected_family: None,
                applied_version_count: 0,
                embedded_version_count: spec.migrator.migrations.len(),
                rewritten_versions: Vec::new(),
                reason: None,
            };
        }
        Ok(Some(rows)) => rows,
        Err(error) => {
            pool.close().await;
            return FixChecksumsDb {
                label: spec.label.to_string(),
                path: spec.path.to_string_lossy().into_owned(),
                status: FixStatus::Rejected,
                detected_family: None,
                applied_version_count: 0,
                embedded_version_count: spec.migrator.migrations.len(),
                rewritten_versions: Vec::new(),
                reason: Some(format!("read_applied_failed: {error}")),
            };
        }
    };
    if applied.is_empty() {
        pool.close().await;
        return FixChecksumsDb {
            label: spec.label.to_string(),
            path: spec.path.to_string_lossy().into_owned(),
            status: FixStatus::Empty,
            detected_family: None,
            applied_version_count: 0,
            embedded_version_count: spec.migrator.migrations.len(),
            rewritten_versions: Vec::new(),
            reason: None,
        };
    }
    // P1: applied migration unknown to this binary -> reject.
    for (version, _) in &applied {
        if !spec
            .migrator
            .migrations
            .iter()
            .any(|m| m.version == *version)
        {
            pool.close().await;
            return FixChecksumsDb {
                label: spec.label.to_string(),
                path: spec.path.to_string_lossy().into_owned(),
                status: FixStatus::Rejected,
                detected_family: None,
                applied_version_count: applied.len(),
                embedded_version_count: spec.migrator.migrations.len(),
                rewritten_versions: Vec::new(),
                reason: Some(format!("unknown_migration_version_{version}")),
            };
        }
    }
    // P2: checksum outside both line-ending families -> reject.
    for (version, stored) in &applied {
        let Some(migration) = spec
            .migrator
            .migrations
            .iter()
            .find(|m| m.version == *version)
        else {
            continue;
        };
        let in_embedded = stored.as_slice() == migration.checksum.as_ref();
        let in_crlf =
            crlf_checksum(migration).is_some_and(|image| stored.as_slice() == image.as_slice());
        if !in_embedded && !in_crlf {
            pool.close().await;
            return FixChecksumsDb {
                label: spec.label.to_string(),
                path: spec.path.to_string_lossy().into_owned(),
                status: FixStatus::Rejected,
                detected_family: None,
                applied_version_count: applied.len(),
                embedded_version_count: spec.migrator.migrations.len(),
                rewritten_versions: Vec::new(),
                reason: Some(format!("not_an_eol_only_difference_at_version_{version}")),
            };
        }
    }
    // P4: version set match (checked before the schema gate so a partial
    // history reports the version-set reason, not the full-replay drift).
    if let Some(reason) = reject_reason_version_set(spec.migrator, &applied) {
        pool.close().await;
        return FixChecksumsDb {
            label: spec.label.to_string(),
            path: spec.path.to_string_lossy().into_owned(),
            status: FixStatus::Rejected,
            detected_family: None,
            applied_version_count: applied.len(),
            embedded_version_count: spec.migrator.migrations.len(),
            rewritten_versions: Vec::new(),
            reason: Some(reason),
        };
    }
    // P3: schema gate (SchemaReplay vs actual inventory).
    let actual = match actual_schema_inventory(&pool).await {
        Ok(actual) => actual,
        Err(error) => {
            pool.close().await;
            return FixChecksumsDb {
                label: spec.label.to_string(),
                path: spec.path.to_string_lossy().into_owned(),
                status: FixStatus::Rejected,
                detected_family: None,
                applied_version_count: applied.len(),
                embedded_version_count: spec.migrator.migrations.len(),
                rewritten_versions: Vec::new(),
                reason: Some(format!("schema_inventory_failed: {error}")),
            };
        }
    };
    if let Some(reason) = reject_reason_schema(spec.migrator, &actual) {
        pool.close().await;
        return FixChecksumsDb {
            label: spec.label.to_string(),
            path: spec.path.to_string_lossy().into_owned(),
            status: FixStatus::Rejected,
            detected_family: None,
            applied_version_count: applied.len(),
            embedded_version_count: spec.migrator.migrations.len(),
            rewritten_versions: Vec::new(),
            reason: Some(reason),
        };
    }
    let detected = detect_family(spec.migrator, &applied);
    let rewritten_versions: Vec<i64> = applied
        .iter()
        .filter_map(|(version, stored)| {
            let migration = spec
                .migrator
                .migrations
                .iter()
                .find(|m| m.version == *version)?;
            let in_embedded = stored.as_slice() == migration.checksum.as_ref();
            let in_crlf =
                crlf_checksum(migration).is_some_and(|image| stored.as_slice() == image.as_slice());
            match (target, in_embedded, in_crlf) {
                (ChecksumFamily::Lf, true, _) | (ChecksumFamily::Crlf, _, true) => None,
                (ChecksumFamily::Lf, _, true) | (ChecksumFamily::Crlf, true, _) => Some(*version),
                _ => None,
            }
        })
        .collect();
    let status = if rewritten_versions.is_empty() {
        FixStatus::NoChange
    } else {
        FixStatus::DryRun
    };
    let result = FixChecksumsDb {
        label: spec.label.to_string(),
        path: spec.path.to_string_lossy().into_owned(),
        status,
        detected_family: detected,
        applied_version_count: applied.len(),
        embedded_version_count: spec.migrator.migrations.len(),
        rewritten_versions,
        reason: None,
    };
    pool.close().await;
    result
}

async fn apply_to_database(
    sqlite: &SqliteConfig,
    spec: &DatabaseSpec,
    target: ChecksumFamily,
) -> anyhow::Result<FixChecksumsDb> {
    if !spec.path.exists() {
        return Ok(FixChecksumsDb {
            label: spec.label.to_string(),
            path: spec.path.to_string_lossy().into_owned(),
            status: FixStatus::NotFound,
            detected_family: None,
            applied_version_count: 0,
            embedded_version_count: spec.migrator.migrations.len(),
            rewritten_versions: Vec::new(),
            reason: None,
        });
    }
    let pool = open_pool_for(sqlite, &spec.path).await?;
    // P5: backup before any rewrite; reject if a backup already exists.
    let backup = backup_path_for(&spec.path, target);
    if backup.exists() {
        pool.close().await;
        return Err(anyhow::anyhow!(
            "backup {} already exists; refusing to overwrite a previous pre-checksum-flip snapshot",
            backup.display()
        ));
    }
    // Checkpoint the WAL while no transaction is held: a TRUNCATE checkpoint
    // inside our own writer transaction self-deadlocks (SQLITE_LOCKED), and
    // the copy must see a complete main database file.
    let checkpoint = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&pool),
    )
    .await;
    if checkpoint.is_err() {
        // A contended TRUNCATE checkpoint can block without honoring the
        // busy timeout; report rejected and let the locals drop instead of
        // calling the blocking close().
        return Ok(rejected_db(
            spec,
            "database_busy_another_codex_process_may_be_running",
        ));
    }
    if let Some(parent) = backup.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&spec.path, &backup).map_err(|error| {
        anyhow::anyhow!(
            "failed to copy {} to {}: {error}",
            spec.path.display(),
            backup.display()
        )
    })?;
    // P6: BEGIN IMMEDIATE writer gate, bounded by the pool's busy timeout
    // and an outer timeout so contention with another live Codex process
    // surfaces as a rejected database instead of a hang. The gate
    // transaction commits empty and releases the writer lock immediately;
    // repair_eol_checksum_family takes its own writer transaction.
    // The gate connection lives in a scoped block: Pool::close() waits for
    // checked-out connections to be returned, so it must not still be held
    // (explicitly or in a variable) when any close() below runs.
    {
        let mut conn = pool.acquire().await?;
        let begun = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            conn.begin_with("BEGIN IMMEDIATE"),
        )
        .await;
        let gate = match begun {
            Ok(Ok(tx)) => tx,
            // Busy or timed out: the locals drop here without a blocking
            // Pool::close (close waits for checked-out connections, which
            // `conn` still is) and the database is reported rejected.
            _ => {
                return Ok(rejected_db(
                    spec,
                    "database_busy_another_codex_process_may_be_running",
                ));
            }
        };
        gate.commit().await?;
    }
    let rewritten = repair_eol_checksum_family(&pool, spec.migrator, target).await?;
    let detected = if rewritten.is_empty() {
        Some(target.as_str().to_string())
    } else {
        let other = match target {
            ChecksumFamily::Lf => "crlf",
            ChecksumFamily::Crlf => "lf",
        };
        Some(other.to_string())
    };
    let status = if rewritten.is_empty() {
        FixStatus::NoChange
    } else {
        FixStatus::Rewritten
    };
    pool.close().await;
    Ok(FixChecksumsDb {
        label: spec.label.to_string(),
        path: spec.path.to_string_lossy().into_owned(),
        status,
        detected_family: detected,
        applied_version_count: spec.migrator.migrations.len(),
        embedded_version_count: spec.migrator.migrations.len(),
        rewritten_versions: rewritten,
        reason: None,
    })
}

/// Build a rejected per-database entry for an apply-phase gate failure.
fn rejected_db(spec: &DatabaseSpec, reason: &str) -> FixChecksumsDb {
    FixChecksumsDb {
        label: spec.label.to_string(),
        path: spec.path.to_string_lossy().into_owned(),
        status: FixStatus::Rejected,
        detected_family: None,
        applied_version_count: 0,
        embedded_version_count: spec.migrator.migrations.len(),
        rewritten_versions: Vec::new(),
        reason: Some(reason.to_string()),
    }
}

/// Validate, then optionally apply the target family to every runtime
/// database. The report is always returned in full; when any database
/// rejects a precondition, nothing is written.
pub async fn fix_migration_checksum_families(
    sqlite: &SqliteConfig,
    target: ChecksumFamily,
    apply: bool,
) -> anyhow::Result<FixChecksumsReport> {
    let specs = collect_database_specs(sqlite);
    let mut results: Vec<FixChecksumsDb> = Vec::with_capacity(specs.len());
    for spec in &specs {
        results.push(validate_database(sqlite, spec, target).await);
    }
    let any_rejected = results
        .iter()
        .any(|r| matches!(r.status, FixStatus::Rejected));
    if !apply || any_rejected {
        return Ok(FixChecksumsReport {
            target_family: target.as_str().to_string(),
            applied: false,
            databases: results,
        });
    }
    for (spec, prior) in specs.iter().zip(results.iter_mut()) {
        if prior.status != FixStatus::DryRun {
            // Nothing to rewrite (already in the target family, missing, or
            // empty): applying would only create a redundant backup, so the
            // run stays idempotent.
            continue;
        }
        let outcome = apply_to_database(sqlite, spec, target).await?;
        *prior = outcome;
    }
    Ok(FixChecksumsReport {
        target_family: target.as_str().to_string(),
        applied: true,
        databases: results,
    })
}

#[cfg(test)]
#[path = "fix_checksums_tests.rs"]
mod tests;
