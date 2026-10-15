use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use sqlx::AssertSqlSafe;
use sqlx::Row;
use sqlx::SqlSafeStr;
use sqlx::migrate::MigrateError;
use sqlx::migrate::Migration;
use sqlx::migrate::Migrator;

use super::ChecksumFamily;
use super::FamilyDrift;
use super::ObjectKind;
use super::SchemaReplay;
use super::actual_schema_inventory;
use super::detect_checksum_family_drift;
use super::family_checksum;
use super::migrator_embedded_family;
use super::repair_eol_checksum_family;
use crate::migrations::GOALS_MIGRATOR;
use crate::migrations::LOGS_MIGRATOR;
use crate::migrations::MEMORIES_MIGRATOR;
use crate::migrations::QUEUE_MIGRATOR;
use crate::migrations::STATE_MIGRATOR;
use crate::migrations::THREAD_HISTORY_MIGRATOR;
use crate::migrations::runtime_state_migrator;

// Ticket 35 made the embedded checksum family platform-relative (official
// platform default), so these tests phrase every flip relative to the
// `embedded()`/`flipped()` pair of the build running them.
fn embedded() -> ChecksumFamily {
    migrator_embedded_family(&STATE_MIGRATOR)
}

fn flipped() -> ChecksumFamily {
    embedded().flip()
}

/// The checksum sqlx would embed for the given line-ending image of a
/// migration (`embedded()` collapses to the precomputed embedded checksum).
fn image_checksum(migration: &Migration, family: ChecksumFamily) -> Vec<u8> {
    family_checksum(migration, family).expect("line-ending image should be computable")
}

fn image_checksum_for(migrator: &Migrator, version: i64, family: ChecksumFamily) -> Vec<u8> {
    let migration = migrator
        .migrations
        .iter()
        .find(|migration| migration.version == version)
        .expect("migration should exist");
    image_checksum(migration, family)
}

/// The embedded SQL of a migration rewritten into the flipped family.
fn flipped_sql(sql: &str) -> String {
    let lf = sql.replace("\r\n", "\n");
    if flipped() == ChecksumFamily::Crlf {
        lf.replace("\n", "\r\n")
    } else {
        lf
    }
}

fn embedded_checksum(migrator: &Migrator, version: i64) -> Vec<u8> {
    let migration = migrator
        .migrations
        .iter()
        .find(|migration| migration.version == version)
        .expect("migration should exist");
    migration.checksum.to_vec()
}

fn migrator_through(migrator: &Migrator, max_version: i64) -> Migrator {
    Migrator::with_migrations(
        migrator
            .migrations
            .iter()
            .filter(|migration| migration.version <= max_version)
            .cloned()
            .collect(),
    )
}

async fn stored_history(pool: &sqlx::SqlitePool) -> Vec<(i64, Vec<u8>)> {
    sqlx::query("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
        .fetch_all(pool)
        .await
        .expect("migration history should load")
        .into_iter()
        .map(|row| {
            (
                row.get::<i64, _>("version"),
                row.get::<Vec<u8>, _>("checksum"),
            )
        })
        .collect()
}

fn embedded_history(migrator: &Migrator) -> Vec<(i64, Vec<u8>)> {
    migrator
        .migrations
        .iter()
        .map(|migration| (migration.version, migration.checksum.to_vec()))
        .collect()
}

#[tokio::test]
async fn repair_eol_checksum_family_heals_eol_only_checksums() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply");

    // Stamp the stored history with the opposite line-ending family: the
    // byte form a build from the other checkout fed to sqlx::migrate!.
    for migration in STATE_MIGRATOR.migrations.iter() {
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(image_checksum(migration, flipped()).as_slice())
            .bind(migration.version)
            .execute(&pool)
            .await
            .expect("flipped-family checksum should update");
    }

    // The embedded-family migrator must reject that history before the heal.
    let strict_error = STATE_MIGRATOR
        .run(&pool)
        .await
        .expect_err("embedded-family migrator should reject a flipped-family history");
    assert!(matches!(strict_error, MigrateError::VersionMismatch(_)));

    repair_eol_checksum_family(&pool, &runtime_state_migrator(), embedded())
        .await
        .expect("EOL-only checksum flip should be healed");

    assert_eq!(
        stored_history(&pool).await,
        embedded_history(&STATE_MIGRATOR)
    );

    // The heal is only complete when sqlx itself accepts the database again.
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply after the heal");

    pool.close().await;
}

#[tokio::test]
async fn repair_eol_checksum_family_rejects_schema_mismatch() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    let partial = migrator_through(&STATE_MIGRATOR, /*max_version*/ 45);
    partial
        .run(&pool)
        .await
        .expect("partial migrations should apply");

    for migration in partial.migrations.iter() {
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(image_checksum(migration, flipped()).as_slice())
            .bind(migration.version)
            .execute(&pool)
            .await
            .expect("flipped-family checksum should update");
    }
    sqlx::query("DROP TABLE thread_sections")
        .execute(&pool)
        .await
        .expect("schema drift should apply");

    let heal_error = repair_eol_checksum_family(&pool, &runtime_state_migrator(), embedded())
        .await
        .expect_err("schema drift must hard-fail instead of rewriting checksums");
    assert!(heal_error.to_string().contains("schema"));

    // Nothing was rewritten: the history still carries the flipped family.
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(&pool)
            .await
            .expect("version 1 row should load");
    assert_eq!(stored, image_checksum_for(&STATE_MIGRATOR, 1, flipped()));

    pool.close().await;
}

#[tokio::test]
async fn repair_eol_checksum_family_rejects_non_eol_checksum() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply");

    // Corrupt one version beyond either line-ending family.
    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = 1")
        .bind([1_u8, 2, 3, 4].as_slice())
        .execute(&pool)
        .await
        .expect("corrupted checksum should update");

    let heal_error = repair_eol_checksum_family(&pool, &runtime_state_migrator(), embedded())
        .await
        .expect_err("checksums outside both line-ending families must hard-fail");
    assert!(
        heal_error
            .to_string()
            .contains("not an EOL-only difference")
    );

    // Nothing was rewritten: version 1 stays corrupted and the rest of the
    // history keeps the embedded family.
    let corrupted: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(&pool)
            .await
            .expect("version 1 row should load");
    assert_eq!(corrupted, vec![1, 2, 3, 4]);
    let untouched: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 2")
            .fetch_one(&pool)
            .await
            .expect("version 2 row should load");
    assert_eq!(untouched, embedded_checksum(&STATE_MIGRATOR, 2));

    pool.close().await;
}

#[tokio::test]
async fn repair_eol_checksum_family_rewrites_legacy_recency_row() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");

    // A pre-rename binary of the OTHER family: migrations 1..37 plus the
    // recency migration recorded as version 38 with the flipped image of
    // the recency SQL.
    let pre_recency = migrator_through(&STATE_MIGRATOR, /*max_version*/ 37);
    let recency = STATE_MIGRATOR
        .migrations
        .iter()
        .find(|migration| migration.version == 39)
        .expect("recency migration should exist");
    let mut legacy_migrations = pre_recency.migrations.iter().cloned().collect::<Vec<_>>();
    legacy_migrations.push(Migration::new(
        38,
        recency.description.clone(),
        recency.migration_type,
        AssertSqlSafe(flipped_sql(recency.sql.as_str())).into_sql_str(),
        recency.no_tx,
    ));
    Migrator::with_migrations(legacy_migrations)
        .run(&pool)
        .await
        .expect("legacy recency migrations should apply");

    repair_eol_checksum_family(&pool, &runtime_state_migrator(), embedded())
        .await
        .expect("legacy recency row should be healed");

    // The row is now version 39 with the embedded checksum and description.
    let renamed = sqlx::query(
        "SELECT version, checksum FROM _sqlx_migrations WHERE version >= 38 ORDER BY version",
    )
    .fetch_all(&pool)
    .await
    .expect("migration history should load")
    .into_iter()
    .map(|row| {
        (
            row.get::<i64, _>("version"),
            row.get::<Vec<u8>, _>("checksum"),
        )
    })
    .collect::<Vec<_>>();
    // Only the renamed recency row exists at version >= 38; later
    // migrations are applied by the STATE_MIGRATOR.run call below.
    let expected = STATE_MIGRATOR
        .migrations
        .iter()
        .filter(|migration| migration.version == 39)
        .map(|migration| (migration.version, migration.checksum.to_vec()))
        .collect::<Vec<_>>();
    assert_eq!(renamed, expected);

    // sqlx accepts the healed database and applies the missing migrations.
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply after the heal");

    pool.close().await;
}

#[tokio::test]
async fn repair_eol_checksum_family_is_noop_without_mismatches() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply");

    repair_eol_checksum_family(&pool, &runtime_state_migrator(), embedded())
        .await
        .expect("a matching history should not error the heal");

    assert_eq!(
        stored_history(&pool).await,
        embedded_history(&STATE_MIGRATOR)
    );

    pool.close().await;
}

#[tokio::test]
async fn repair_eol_checksum_family_schema_replay_covers_all_runtime_databases() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let cases = [
        (&STATE_MIGRATOR, sqlite.state_db_path()),
        (&LOGS_MIGRATOR, sqlite.logs_db_path()),
        (&GOALS_MIGRATOR, sqlite.goals_db_path()),
        (&MEMORIES_MIGRATOR, sqlite.memories_db_path()),
        (&QUEUE_MIGRATOR, sqlite.queue_db_path()),
        (&THREAD_HISTORY_MIGRATOR, sqlite.thread_history_db_path()),
    ];
    for (migrator, path) in &cases {
        let pool = sqlite
            .open_read_write_pool(path)
            .await
            .expect("runtime database should open");
        migrator.run(&pool).await.expect("migrations should apply");

        // The DDL replay must reproduce the exact catalog of a database the
        // migrator itself built, for every runtime migration set: the gate
        // in repair_eol_checksum_family compares against exactly this
        // replay.
        let mut replay = SchemaReplay::default();
        for migration in migrator.migrations.iter() {
            replay.apply_migration(migration.sql.as_str());
        }
        replay
            .objects
            .insert("_sqlx_migrations".to_string(), ObjectKind::Table);
        let actual = actual_schema_inventory(&pool)
            .await
            .expect("catalog inventory should load");
        assert_eq!(actual, replay.objects);

        pool.close().await;
    }
}

#[tokio::test]
async fn repair_eol_checksum_family_with_flipped_target_rewrites_embedded_history() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply");

    // The embedded-family migrator accepts the database as-is; flipping to
    // the opposite family must rewrite every row.
    let rewritten = repair_eol_checksum_family(&pool, &runtime_state_migrator(), flipped())
        .await
        .expect("flipping to the opposite target should succeed");
    assert!(
        !rewritten.is_empty(),
        "every embedded-stamped row must be rewritten"
    );

    // Every row now carries the flipped line-ending image.
    for migration in STATE_MIGRATOR.migrations.iter() {
        let stored: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
                .bind(migration.version)
                .fetch_one(&pool)
                .await
                .expect("stored checksum should load");
        let image = image_checksum(migration, flipped());
        assert_eq!(
            stored, image,
            "version {} must carry the flipped image",
            migration.version
        );
    }

    pool.close().await;
}

#[tokio::test]
async fn repair_eol_checksum_family_with_flipped_target_rejects_schema_mismatch() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    let partial = migrator_through(&STATE_MIGRATOR, /*max_version*/ 45);
    partial
        .run(&pool)
        .await
        .expect("partial migrations should apply");
    sqlx::query("DROP TABLE thread_sections")
        .execute(&pool)
        .await
        .expect("schema drift should apply");

    let heal_error = repair_eol_checksum_family(&pool, &runtime_state_migrator(), flipped())
        .await
        .expect_err("schema drift must hard-fail for the flipped target too");
    assert!(heal_error.to_string().contains("schema"));

    // Nothing was rewritten: rows still carry the embedded checksum.
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(&pool)
            .await
            .expect("version 1 row should load");
    assert_eq!(stored, embedded_checksum(&STATE_MIGRATOR, 1));

    pool.close().await;
}

#[tokio::test]
async fn repair_eol_checksum_family_with_flipped_target_rewrites_recency_row() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");

    // Migrations 1..37 plus the recency migration at version 39, all in the
    // embedded family: flipping to the opposite family must carry the
    // recency row across too.
    let pre_recency = migrator_through(&STATE_MIGRATOR, /*max_version*/ 37);
    let recency = STATE_MIGRATOR
        .migrations
        .iter()
        .find(|migration| migration.version == 39)
        .expect("recency migration should exist");
    let legacy_migrations = pre_recency
        .migrations
        .iter()
        .cloned()
        .chain(std::iter::once(recency.clone()))
        .collect::<Vec<_>>();
    Migrator::with_migrations(legacy_migrations)
        .run(&pool)
        .await
        .expect("recency migrations should apply");

    repair_eol_checksum_family(&pool, &runtime_state_migrator(), flipped())
        .await
        .expect("recency row should be healed to the flipped target");

    // The row is version 39 with the flipped image of the recency SQL.
    let renamed: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 39")
            .fetch_one(&pool)
            .await
            .expect("recency row should exist at version 39");
    assert_eq!(renamed, image_checksum(recency, flipped()));

    pool.close().await;
}

#[tokio::test]
async fn repair_eol_checksum_family_with_flipped_target_is_noop_when_already_flipped() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply");
    // Pre-flip every row to the opposite family.
    for migration in STATE_MIGRATOR.migrations.iter() {
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(image_checksum(migration, flipped()).as_slice())
            .bind(migration.version)
            .execute(&pool)
            .await
            .expect("flipped image should update");
    }

    let rewritten = repair_eol_checksum_family(&pool, &runtime_state_migrator(), flipped())
        .await
        .expect("already-flipped history is a no-op for the flipped target");
    assert!(
        rewritten.is_empty(),
        "no rows should be rewritten when already in target"
    );

    pool.close().await;
}

#[tokio::test]
async fn detect_checksum_family_drift_reports_pure_flip() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let pool = sqlite
        .open_read_write_pool(&sqlite.state_db_path())
        .await
        .expect("sqlite database should open");
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply");

    // No drift while the history sits in the embedded family.
    assert_eq!(
        detect_checksum_family_drift(&pool, &runtime_state_migrator(), embedded())
            .await
            .expect("probe should succeed"),
        None
    );

    for migration in STATE_MIGRATOR.migrations.iter() {
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(image_checksum(migration, flipped()).as_slice())
            .bind(migration.version)
            .execute(&pool)
            .await
            .expect("flip should update");
    }
    let drift = detect_checksum_family_drift(&pool, &runtime_state_migrator(), embedded())
        .await
        .expect("probe should succeed")
        .expect("flipped history is a pure family drift");
    assert_eq!(drift.db_family, Some(flipped()));
    assert_eq!(drift.binary_family, embedded());

    // One row dragged back -> mixed database, still drift but unlabelable.
    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = 1")
        .bind(embedded_checksum(&STATE_MIGRATOR, 1).as_slice())
        .execute(&pool)
        .await
        .expect("mix should update");
    let drift = detect_checksum_family_drift(&pool, &runtime_state_migrator(), embedded())
        .await
        .expect("probe should succeed")
        .expect("mixed history is still drift");
    assert_eq!(drift.db_family, None);

    // A checksum outside both families -> no guidance (sqlx error stands).
    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = 1")
        .bind([5_u8, 6].as_slice())
        .execute(&pool)
        .await
        .expect("tamper should update");
    assert_eq!(
        detect_checksum_family_drift(&pool, &runtime_state_migrator(), embedded())
            .await
            .expect("probe should succeed"),
        None
    );

    pool.close().await;
}

#[test]
fn drift_guidance_names_both_families_and_the_command() {
    let drift = FamilyDrift {
        db_family: Some(ChecksumFamily::Lf),
        binary_family: ChecksumFamily::Crlf,
    };
    let guidance = drift.guidance();
    assert!(
        guidance.contains("checksum family mismatch"),
        "guidance: {guidance}"
    );
    assert!(
        guidance.contains("stored migrations are in the lf family"),
        "guidance: {guidance}"
    );
    assert!(
        guidance.contains("this binary embeds the crlf family"),
        "guidance: {guidance}"
    );
    assert_eq!(
        drift.repair_command(),
        "codex state fix-checksums --family crlf --apply"
    );
    assert!(
        guidance.contains(&drift.repair_command()),
        "guidance: {guidance}"
    );
    let notice = drift.notice_text();
    assert!(notice.contains("one-time notice"), "notice: {notice}");
    assert!(
        notice.contains("docs/fork-checksum-family.md"),
        "notice: {notice}"
    );
    let fingerprint = drift.fingerprint();
    assert_eq!(fingerprint.db_family, "lf");
    assert_eq!(fingerprint.binary_family, "crlf");
    assert_eq!(fingerprint.platform, std::env::consts::OS);

    // Mixed databases label as "mixed" and keep pointing at the binary
    // family (the repair is the same command).
    let mixed = FamilyDrift {
        db_family: None,
        binary_family: ChecksumFamily::Lf,
    };
    assert!(
        mixed
            .guidance()
            .contains("stored migrations are in the mixed family"),
        "guidance: {}",
        mixed.guidance()
    );
}

#[test]
fn drift_guidance_prints_each_help_line_once() {
    // Ticket 37: the fail-loud block is composed exactly once here (the
    // app-server renders the error chain with {:#}; the wrapper Display
    // fix keeps the whole block - help lines included - single).
    let drift = FamilyDrift {
        db_family: Some(ChecksumFamily::Lf),
        binary_family: ChecksumFamily::Crlf,
    };
    let guidance = drift.guidance();
    assert_eq!(guidance.matches("help:").count(), 3, "guidance: {guidance}");
    let command = drift.repair_command();
    assert_eq!(
        guidance.matches(command.as_str()).count(),
        1,
        "guidance: {guidance}"
    );
    assert_eq!(
        drift.notice_text().matches(command.as_str()).count(),
        1,
        "notice: {}",
        drift.notice_text()
    );
}

#[tokio::test]
async fn repair_eol_checksum_family_repairs_behind_database_and_startup_absorbs() {
    // Ticket 37 (A-030), engine arm of the tri-state matrix: stored is a
    // proper subset of embedded (pending migrations, the R1 field shape)
    // heals row by row like any other EOL drift, the repair itself does
    // NOT apply the missing migrations, and the startup migrator absorbs
    // them into the embedded (target) family afterwards.
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    let keep = STATE_MIGRATOR.migrations.len() - 3;
    let behind = Migrator::with_migrations(
        STATE_MIGRATOR
            .migrations
            .iter()
            .take(keep)
            .cloned()
            .collect(),
    );
    behind
        .run(&pool)
        .await
        .expect("behind migrations should apply");
    for migration in behind.migrations.iter() {
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(image_checksum(migration, flipped()).as_slice())
            .bind(migration.version)
            .execute(&pool)
            .await
            .expect("flipped stamp should apply");
    }

    // The embedded-family startup rejects the drifted subset database -
    // this is the VersionMismatch whose fail-loud text pointed at the
    // repair command in the R1 rehearsal.
    let strict_error = STATE_MIGRATOR
        .run(&pool)
        .await
        .expect_err("embedded-family migrator must reject the flipped history");
    assert!(matches!(strict_error, MigrateError::VersionMismatch(_)));

    repair_eol_checksum_family(&pool, &runtime_state_migrator(), embedded())
        .await
        .expect("a behind database must be repairable");

    // Checksums only: the pending versions are still unapplied.
    assert_eq!(stored_history(&pool).await.len(), keep);

    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("startup must absorb the pending migrations");
    assert_eq!(
        stored_history(&pool).await,
        embedded_history(&STATE_MIGRATOR)
    );

    pool.close().await;
}
