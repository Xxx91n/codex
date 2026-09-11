use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use sqlx::Connection;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;

use super::ChecksumFamily;
use super::FixStatus;
use super::fix_migration_checksum_families;
use crate::SqliteConfig;
use crate::eol_checksum_repair::crlf_checksum;
use crate::migrations::STATE_MIGRATOR;
use crate::runtime::test_support::unique_temp_dir;

async fn stamp_family(pool: &SqlitePool, family: ChecksumFamily) {
    for migration in STATE_MIGRATOR.migrations.iter() {
        let checksum: Vec<u8> = match family {
            ChecksumFamily::Lf => migration.checksum.to_vec(),
            ChecksumFamily::Crlf => crlf_checksum(migration).expect("crlf image should exist"),
        };
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

async fn open_state_db_lf_stamped(sqlite: &SqliteConfig) -> SqlitePool {
    let pool = build_pool(sqlite).await;
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("migrations should apply");
    pool
}

async fn open_state_db_crlf_stamped(sqlite: &SqliteConfig) -> SqlitePool {
    let pool = open_state_db_lf_stamped(sqlite).await;
    stamp_family(&pool, ChecksumFamily::Crlf).await;
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
    let pool = open_state_db_lf_stamped(&sqlite).await;
    pool.close().await;

    let report = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, false)
        .await
        .expect("dry-run should succeed");
    assert!(!report.applied);
    assert_eq!(report.target_family, "crlf");
    let state = report
        .databases
        .iter()
        .find(|d| d.label == "state DB")
        .expect("state DB entry");
    assert_eq!(state.status, FixStatus::DryRun);
    assert!(
        !state.rewritten_versions.is_empty(),
        "LF to CRLF needs work"
    );
    assert_eq!(state.detected_family.as_deref(), Some("lf"));

    let verify = build_pool(&sqlite).await;
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(&verify)
            .await
            .expect("row 1 should load");
    let expected = STATE_MIGRATOR
        .migrations
        .iter()
        .find(|m| m.version == 1)
        .unwrap()
        .checksum
        .to_vec();
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
    let pool = open_state_db_lf_stamped(&sqlite).await;
    pool.close().await;

    let report = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, true)
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
        let image = crlf_checksum(migration).expect("image");
        assert_eq!(stored, image, "version {} must be CRLF", migration.version);
    }
    verify.close().await;
}

#[tokio::test]
async fn state_checksum_family_fix_rejects_true_drift() {
    // Precondition 2: a checksum that is neither the embedded nor the CRLF
    // image means real drift; the offline flip must refuse.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let pool = open_state_db_lf_stamped(&sqlite).await;
    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = 1")
        .bind([1_u8, 2, 3, 4].as_slice())
        .execute(&pool)
        .await
        .expect("corrupt should apply");
    pool.close().await;

    let report = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, true)
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

    let report = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, true)
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
    let pool = open_state_db_lf_stamped(&sqlite).await;
    sqlx::query("DROP TABLE thread_sections")
        .execute(&pool)
        .await
        .expect("drop");
    pool.close().await;

    let report = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, true)
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

    let report = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, true)
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
    let _pool = open_state_db_lf_stamped(&sqlite).await;
    let backup = sqlite
        .state_db_path()
        .with_file_name("state_5.sqlite.pre-checksum-flip-crlf.bak");
    std::fs::write(&backup, b"existing").expect("write backup");

    let err = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, true)
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
    let _pool = open_state_db_lf_stamped(&sqlite).await;

    let blocker = build_pool(&sqlite).await;
    let mut conn = blocker.acquire().await.expect("acquire");
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await.expect("begin");

    let started = std::time::Instant::now();
    let result = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, true).await;
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
    let _pool = open_state_db_crlf_stamped(&sqlite).await;

    let report = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, false)
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
    let _pool = open_state_db_lf_stamped(&sqlite).await;

    let first = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, true)
        .await
        .expect("first apply");
    assert!(first.applied);
    let second = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, true)
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
async fn state_checksum_family_startup_default_auto_heals_crlf_to_lf() {
    // Config default auto: the runtime self-heal rewrites a CRLF-stamped
    // database back to the embedded (LF) family on startup.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let _pool = open_state_db_crlf_stamped(&sqlite).await;

    let _runtime = crate::runtime::StateRuntime::init(sqlite, "test".to_string())
        .await
        .expect("init should succeed under default auto");

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
            migration.checksum.to_vec(),
            "default auto must land LF"
        );
    }
    verify.close().await;
}

#[tokio::test]
async fn state_checksum_family_startup_explicit_crlf_maintains_crlf_family() {
    // [state] migration_checksum_family = "crlf": the runtime keeps the
    // CRLF family at steady state.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs())
        .with_maintained_checksum_family(Some(ChecksumFamily::Crlf));
    let _pool = open_state_db_lf_stamped(&sqlite).await;

    let _runtime = crate::runtime::StateRuntime::init(sqlite, "test".to_string())
        .await
        .expect("init should succeed under explicit crlf");

    let verify_path = sqlite_home.as_path().abs();
    let verify = build_pool(&SqliteConfig::new_for_testing(verify_path)).await;
    for migration in STATE_MIGRATOR.migrations.iter() {
        let stored: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
                .bind(migration.version)
                .fetch_one(&verify)
                .await
                .expect("row");
        let image = crlf_checksum(migration).expect("image");
        assert_eq!(stored, image, "explicit crlf must land CRLF");
    }
    verify.close().await;
}

#[tokio::test]
async fn state_checksum_family_startup_explicit_crlf_heals_then_flips() {
    // The user's DB is currently CRLF (e.g. a fresh official-Windows
    // install). Explicit-crlf config must: (1) accept the CRLF history, (2)
    // bring it forward to the embedded LF family, (3) migrate, (4) flip
    // the whole history back to CRLF. Final state: every row is CRLF.
    let sqlite_home = unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home).await.expect("home");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |p| {
        let _ = std::fs::remove_dir_all(p);
    });
    let sqlite = SqliteConfig::new_for_testing(sqlite_home.as_path().abs())
        .with_maintained_checksum_family(Some(ChecksumFamily::Crlf));
    let _pool = open_state_db_crlf_stamped(&sqlite).await;

    let _runtime = crate::runtime::StateRuntime::init(sqlite, "test".to_string())
        .await
        .expect("init should succeed: heal then migrate then flip");

    let verify_path = sqlite_home.as_path().abs();
    let verify = build_pool(&SqliteConfig::new_for_testing(verify_path)).await;
    for migration in STATE_MIGRATOR.migrations.iter() {
        let stored: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
                .bind(migration.version)
                .fetch_one(&verify)
                .await
                .expect("row");
        let image = crlf_checksum(migration).expect("image");
        assert_eq!(
            stored, image,
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

    let report = fix_migration_checksum_families(&sqlite, ChecksumFamily::Crlf, false)
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
