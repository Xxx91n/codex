use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use sqlx::Connection;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;

use super::ChecksumFamily;
use super::FixStatus;
use super::fix_migration_checksum_families;
use crate::SqliteConfig;
use crate::checksum_family_notice::NOTICE_FILE_NAME;
use crate::eol_checksum_repair::family_checksum;
use crate::eol_checksum_repair::migrator_embedded_family;
use crate::migrations::STATE_MIGRATOR;
use crate::runtime::test_support::unique_temp_dir;

// Ticket 35: the embedded checksum family follows the checkout of the build
// (machine) (official platform family), so every test here is phrased
// relative to `embedded()` / `flipped()` instead of hard-coding LF as the
// embedded side. The literal Crlf/Lf stamps still appear where a test pins
// an absolute family (explicit maintenance opt-ins).
fn embedded() -> ChecksumFamily {
    migrator_embedded_family(&STATE_MIGRATOR)
}

fn flipped() -> ChecksumFamily {
    embedded().flip()
}

fn image(migration: &sqlx::migrate::Migration, family: ChecksumFamily) -> Vec<u8> {
    family_checksum(migration, family).expect("line-ending image should be computable")
}

async fn stamp_family(pool: &SqlitePool, family: ChecksumFamily) {
    for migration in STATE_MIGRATOR.migrations.iter() {
        let checksum = image(migration, family);
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(checksum.as_slice())
            .bind(migration.version)
            .execute(pool)
            .await
            .expect("checksum update should succeed");
    }
}

async fn build_pool(sqlite: &SqliteConfig) -> SqlitePool {
    let path = sqlite.state_db_path();
    sqlite
        .open_read_write_pool(&path)
        .await
        .expect("state pool should open")
}

async fn open_state_db_embedded_stamped(sqlite: &SqliteConfig) -> SqlitePool {
    let pool = build_pool(sqlite).await;
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("migrations should apply");
    pool
}

async fn open_state_db_flipped_stamped(sqlite: &SqliteConfig) -> SqlitePool {
    let pool = open_state_db_embedded_stamped(sqlite).await;
    stamp_family(&pool, flipped()).await;
    pool
}

#[tokio::test]
async fn state_checksum_family_fix_dry_run_reports_changes_without_touching() {
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let pool = open_state_db_embedded_stamped(&sqlite).await;
    pool.close().await;

    let report = fix_migration_checksum_families(&sqlite, flipped(), false)
        .await
        .expect("dry-run should succeed");
    assert!(!report.applied);
    assert_eq!(report.target_family, flipped().as_str());
    let state = report
        .databases
        .iter()
        .find(|d| d.label == "state DB")
        .expect("state DB entry");
    assert_eq!(state.status, FixStatus::DryRun);
    assert!(
        !state.rewritten_versions.is_empty(),
        format!(
            "{} to {} needs work",
            embedded().as_str(),
            flipped().as_str()
        )
    );
    assert_eq!(state.detected_family.as_deref(), Some(embedded().as_str()));

    let verify = build_pool(&sqlite).await;
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(&verify)
            .await
            .expect("row 1 should load");
    let expected = image(&STATE_MIGRATOR.migrations[0], embedded());
    assert_eq!(stored, expected, "dry-run must not modify stored rows");
    verify.close().await;
}

#[tokio::test]
async fn state_checksum_family_fix_apply_rewrites_to_target() {
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let pool = open_state_db_embedded_stamped(&sqlite).await;
    pool.close().await;

    let report = fix_migration_checksum_families(&sqlite, flipped(), true)
        .await
        .expect("apply should succeed");
    assert!(report.applied);
    let state = report
        .databases
        .iter()
        .find(|d| d.label == "state DB")
        .expect("state DB entry");
    assert_eq!(state.status, FixStatus::Rewritten);
    assert!(!state.rewritten_versions.is_empty());

    let verify = build_pool(&sqlite).await;
    for migration in STATE_MIGRATOR.migrations.iter() {
        let stored: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
                .bind(migration.version)
                .fetch_one(&verify)
                .await
                .expect("row should load");
        let expected = image(migration, flipped());
        assert_eq!(
            stored, expected,
            "version {} must land the target",
            migration.version
        );
    }
    verify.close().await;
}

#[tokio::test]
async fn state_checksum_family_fix_rejects_true_drift() {
    // Precondition 2: a checksum that is neither line-ending image of the
    // embedded SQL means real drift; the offline flip must refuse.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let pool = open_state_db_embedded_stamped(&sqlite).await;
    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = 1")
        .bind([1_u8, 2, 3, 4].as_slice())
        .execute(&pool)
        .await
        .expect("corrupt should apply");
    pool.close().await;

    let report = fix_migration_checksum_families(&sqlite, flipped(), true)
        .await
        .expect("validation returns a report");
    let state = report
        .databases
        .iter()
        .find(|d| d.label == "state DB")
        .expect("state DB entry");
    assert_eq!(state.status, FixStatus::Rejected);
    let reason = state.reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("not_an_eol_only_difference_at_version_1"),
        "reason was: {reason}"
    );
    assert!(!report.applied, "rejected path must not write");
}

#[tokio::test]
async fn state_checksum_family_fix_rejects_unknown_migration() {
    // Precondition 1: an applied row whose version is not in the embedded
    // set is unknown to this binary; refuse.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let pool = build_pool(&sqlite).await;
    let partial = Migrator::with_migrations(
        STATE_MIGRATOR
            .migrations
            .iter()
            .filter(|m| m.version <= 1)
            .cloned()
            .collect(),
    );
    partial.run(&pool).await.expect("partial");
    sqlx::query("INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time) VALUES (?, '', '2026-09-11 00:00:00', 1, ?, 0)")
        .bind(9999_i64)
        .bind(STATE_MIGRATOR.migrations[0].checksum.as_ref())
        .execute(&pool)
        .await
        .expect("insert unknown");
    pool.close().await;

    let report = fix_migration_checksum_families(&sqlite, flipped(), true)
        .await
        .expect("validation returns a report");
    let state = report
        .databases
        .iter()
        .find(|d| d.label == "state DB")
        .expect("state DB entry");
    assert_eq!(state.status, FixStatus::Rejected);
    let reason = state.reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("unknown_migration_version_9999"),
        "reason was: {reason}"
    );
    assert!(!report.applied);
}

#[tokio::test]
async fn state_checksum_family_fix_rejects_schema_mismatch() {
    // Precondition 3: schema consistency gate.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let pool = open_state_db_embedded_stamped(&sqlite).await;
    sqlx::query("DROP TABLE thread_sections")
        .execute(&pool)
        .await
        .expect("drop");
    pool.close().await;

    let report = fix_migration_checksum_families(&sqlite, flipped(), true)
        .await
        .expect("validation returns a report");
    let state = report
        .databases
        .iter()
        .find(|d| d.label == "state DB")
        .expect("state DB entry");
    assert_eq!(state.status, FixStatus::Rejected);
    let reason = state.reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("schema_drift") || reason.contains("missing"),
        "reason was: {reason}"
    );
}

#[tokio::test]
async fn state_checksum_family_fix_rejects_version_set_mismatch() {
    // Precondition 4: version set match.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let pool = build_pool(&sqlite).await;
    let partial = Migrator::with_migrations(
        STATE_MIGRATOR
            .migrations
            .iter()
            .filter(|m| m.version <= 5)
            .cloned()
            .collect(),
    );
    partial.run(&pool).await.expect("partial");
    pool.close().await;

    let report = fix_migration_checksum_families(&sqlite, flipped(), true)
        .await
        .expect("validation returns a report");
    let state = report
        .databases
        .iter()
        .find(|d| d.label == "state DB")
        .expect("state DB entry");
    assert_eq!(state.status, FixStatus::Rejected);
    let reason = state.reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("version_set_mismatch"),
        "reason was: {reason}"
    );
}

#[tokio::test]
async fn state_checksum_family_fix_rejects_when_backup_already_exists() {
    // Precondition 5: atomic reversibility.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let _pool = open_state_db_embedded_stamped(&sqlite).await;
    let backup = sqlite.state_db_path().with_file_name(format!(
        "state_5.sqlite.pre-checksum-flip-{}.bak",
        flipped().as_str()
    ));
    std::fs::write(&backup, b"existing").expect("write backup");

    let err = fix_migration_checksum_families(&sqlite, flipped(), true)
        .await
        .expect_err("apply must refuse when backup exists");
    let msg = err.to_string();
    assert!(
        msg.contains("backup") && msg.contains("already exists"),
        "err was: {msg}"
    );
    let _ = std::fs::remove_file(&backup);
}

#[tokio::test]
async fn state_checksum_family_fix_rejects_when_concurrent_writer_holds_lock() {
    // Precondition 6: concurrency boundary.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let _pool = open_state_db_embedded_stamped(&sqlite).await;

    let blocker = build_pool(&sqlite).await;
    let mut conn = blocker.acquire().await.expect("acquire");
    let tx = conn.begin_with("BEGIN IMMEDIATE").await.expect("begin");

    let started = std::time::Instant::now();
    let result = fix_migration_checksum_families(&sqlite, flipped(), true).await;
    let elapsed = started.elapsed();
    tx.rollback().await.expect("rollback");
    // Return the checked-out connection before close(): Pool::close waits
    // for checked-out connections and would otherwise hang.
    drop(conn);
    blocker.close().await;

    assert!(
        result.is_err()
            || result.as_ref().map_or(false, |r| r
                .databases
                .iter()
                .any(|d| d.status == FixStatus::Rejected)),
        "expected busy-timeout rejection; got: {result:?}"
    );
    assert!(
        elapsed >= std::time::Duration::from_secs(1),
        "expected to wait for busy_timeout (elapsed {elapsed:?})"
    );
}

#[tokio::test]
async fn state_checksum_family_fix_dry_run_on_already_target_family() {
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let _pool = open_state_db_flipped_stamped(&sqlite).await;

    let report = fix_migration_checksum_families(&sqlite, flipped(), false)
        .await
        .expect("dry-run");
    let state = report
        .databases
        .iter()
        .find(|d| d.label == "state DB")
        .expect("state DB entry");
    assert_eq!(state.status, FixStatus::NoChange);
    assert!(state.rewritten_versions.is_empty());
}

#[tokio::test]
async fn state_checksum_family_fix_apply_idempotent() {
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let _pool = open_state_db_embedded_stamped(&sqlite).await;

    let first = fix_migration_checksum_families(&sqlite, flipped(), true)
        .await
        .expect("first apply");
    assert!(first.applied);
    let second = fix_migration_checksum_families(&sqlite, flipped(), true)
        .await
        .expect("second apply");
    assert!(second.applied);
    let state = second
        .databases
        .iter()
        .find(|d| d.label == "state DB")
        .expect("state DB entry");
    assert_eq!(state.status, FixStatus::NoChange);
    assert!(state.rewritten_versions.is_empty());
}

#[tokio::test]
async fn state_checksum_family_fix_lf_escape_hatch_flips_and_is_idempotent() {
    // Ticket 35 (A-025): `--family lf` stays a first-class escape hatch:
    // repeated applies are safe no-ops and the history lands the LF image
    // regardless of which family this build embeds.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let _pool = open_state_db_embedded_stamped(&sqlite).await;

    for round in 0..2 {
        let report = fix_migration_checksum_families(&sqlite, ChecksumFamily::Lf, true)
            .await
            .expect("lf escape hatch apply");
        assert!(report.applied, "round {round}");
        let state = report
            .databases
            .iter()
            .find(|d| d.label == "state DB")
            .expect("state DB entry");
        let stored = build_pool(&sqlite).await;
        for migration in STATE_MIGRATOR.migrations.iter() {
            let stored_checksum: Vec<u8> =
                sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
                    .bind(migration.version)
                    .fetch_one(&stored)
                    .await
                    .expect("row should load");
            assert_eq!(
                stored_checksum,
                image(migration, ChecksumFamily::Lf),
                "round {round}: {} must land the LF image",
                migration.version
            );
        }
        stored.close().await;
        if round == 0 {
            assert_ne!(state.status, FixStatus::Rejected);
        }
    }
    let report = fix_migration_checksum_families(&sqlite, ChecksumFamily::Lf, false)
        .await
        .expect("dry-run after flip");
    let state = report
        .databases
        .iter()
        .find(|d| d.label == "state DB")
        .expect("state DB entry");
    assert_eq!(state.status, FixStatus::NoChange);
}

#[tokio::test]
async fn state_checksum_family_startup_default_auto_fails_loud_on_drift() {
    // Ticket 35 (A-021/A-022/A-023): default auto never rewrites. A drifted
    // database yields a loud error naming both families plus the exact
    // repair command, records the one-time marker, and leaves the rows
    // untouched.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let _pool = open_state_db_flipped_stamped(&sqlite).await;

    let err = crate::runtime::StateRuntime::init(sqlite, "test".to_string())
        .await
        .expect_err("auto must fail loud on family drift");
    let msg = err.to_string();
    assert!(msg.contains("checksum family mismatch"), "msg was: {msg}");
    assert!(
        msg.contains(&format!(
            "stored migrations are in the {} family",
            flipped().as_str()
        )),
        "msg was: {msg}"
    );
    assert!(
        msg.contains(&format!(
            "this binary embeds the {} family",
            embedded().as_str()
        )),
        "msg was: {msg}"
    );
    assert!(
        msg.contains(&format!(
            "codex state fix-checksums --family {} --apply",
            embedded().as_str()
        )),
        "msg was: {msg}"
    );
    // One-time marker recorded under the sqlite home (A-023).
    let marker = sqlite_home.as_path().join(NOTICE_FILE_NAME);
    assert!(marker.exists(), "one-time notice marker must be persisted");
    // Zero automatic rewrite: rows still carry the flipped family.
    let verify = build_pool(&SqliteConfig::new_for_testing(sqlite_home.as_path().abs())).await;
    for migration in STATE_MIGRATOR.migrations.iter() {
        let stored: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
                .bind(migration.version)
                .fetch_one(&verify)
                .await
                .expect("row");
        assert_eq!(
            stored,
            image(migration, flipped()),
            "auto must not rewrite version {}",
            migration.version
        );
    }
    verify.close().await;
}

#[tokio::test]
async fn state_checksum_family_startup_default_auto_keeps_tamper_error_verbatim() {
    // A checksum outside both families is real drift/tampering: no family
    // guidance, no repair command offered, no marker (A-022 negative arm).
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let pool = open_state_db_embedded_stamped(&sqlite).await;
    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = 1")
        .bind([9_u8, 8, 7].as_slice())
        .execute(&pool)
        .await
        .expect("corrupt should apply");
    pool.close().await;

    let err = crate::runtime::StateRuntime::init(sqlite, "test".to_string())
        .await
        .expect_err("tampered history must keep failing loudly");
    let msg = err.to_string();
    assert!(msg.contains("previously applied"), "msg was: {msg}");
    assert!(!msg.contains("checksum family mismatch"), "msg was: {msg}");
    assert!(!msg.contains("fix-checksums"), "msg was: {msg}");
    assert!(
        !sqlite_home.as_path().join(NOTICE_FILE_NAME).exists(),
        "no one-time notice for a non-family failure"
    );
}

#[tokio::test]
async fn state_checksum_family_startup_explicit_crlf_maintains_crlf_family() {
    // [state] migration_checksum_family = "crlf" (ticket 31, unchanged):
    // the runtime keeps the CRLF family at steady state.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs())
        .with_maintained_checksum_family(Some(ChecksumFamily::Crlf));
    let _pool = open_state_db_embedded_stamped(&sqlite).await;

    let _runtime = crate::runtime::StateRuntime::init(sqlite, "test".to_string())
        .await
        .expect("init should succeed under explicit crlf");

    let verify = build_pool(&SqliteConfig::new_for_testing(sqlite_home.as_path().abs())).await;
    for migration in STATE_MIGRATOR.migrations.iter() {
        let stored: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
                .bind(migration.version)
                .fetch_one(&verify)
                .await
                .expect("row");
        assert_eq!(
            stored,
            image(migration, ChecksumFamily::Crlf),
            "explicit crlf must land CRLF"
        );
    }
    verify.close().await;
}

#[tokio::test]
async fn state_checksum_family_startup_explicit_lf_maintains_lf_family() {
    // [state] migration_checksum_family = "lf" (ticket 35): the explicit
    // LF opt-in lands the history in the LF image on every startup,
    // including builds whose embedded family is CRLF.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs())
        .with_maintained_checksum_family(Some(ChecksumFamily::Lf));
    let _pool = open_state_db_embedded_stamped(&sqlite).await;

    let _runtime = crate::runtime::StateRuntime::init(sqlite, "test".to_string())
        .await
        .expect("init should succeed under explicit lf");

    let verify = build_pool(&SqliteConfig::new_for_testing(sqlite_home.as_path().abs())).await;
    for migration in STATE_MIGRATOR.migrations.iter() {
        let stored: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
                .bind(migration.version)
                .fetch_one(&verify)
                .await
                .expect("row");
        assert_eq!(
            stored,
            image(migration, ChecksumFamily::Lf),
            "explicit lf must land LF"
        );
    }
    verify.close().await;
}

#[tokio::test]
async fn state_checksum_family_startup_explicit_crlf_heals_then_flips() {
    // The user's DB is currently stamped with the CRLF image (e.g. a fresh
    // official-Windows install). Explicit-crlf config must: (1) accept the
    // CRLF history, (2) land it in the embedded family to validate,
    // (3) migrate, (4) flip the whole history to CRLF. Final state: every
    // row is CRLF.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs())
        .with_maintained_checksum_family(Some(ChecksumFamily::Crlf));
    let pool = open_state_db_embedded_stamped(&sqlite).await;
    stamp_family(&pool, ChecksumFamily::Crlf).await;
    pool.close().await;

    let _runtime = crate::runtime::StateRuntime::init(sqlite, "test".to_string())
        .await
        .expect("init should succeed: heal then migrate then flip");

    let verify = build_pool(&SqliteConfig::new_for_testing(sqlite_home.as_path().abs())).await;
    for migration in STATE_MIGRATOR.migrations.iter() {
        let stored: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
                .bind(migration.version)
                .fetch_one(&verify)
                .await
                .expect("row");
        assert_eq!(
            stored,
            image(migration, ChecksumFamily::Crlf),
            "after heal then migrate then flip every row is CRLF"
        );
    }
    verify.close().await;
}

#[tokio::test]
async fn state_checksum_family_fix_dry_run_on_missing_database() {
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());

    let report = fix_migration_checksum_families(&sqlite, flipped(), false)
        .await
        .expect("missing DBs are not errors");
    assert!(
        report
            .databases
            .iter()
            .all(|d| d.status == FixStatus::NotFound)
    );
    assert!(!report.applied);
}
