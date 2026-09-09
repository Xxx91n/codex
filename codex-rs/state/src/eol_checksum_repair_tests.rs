use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use sqlx::migrate::MigrateError;
use sqlx::migrate::Migration;
use sqlx::migrate::Migrator;
use sqlx::AssertSqlSafe;
use sqlx::Row;
use sqlx::SqlSafeStr;

use super::ObjectKind;
use super::SchemaReplay;
use super::actual_schema_inventory;
use super::repair_eol_checksum_family;
use crate::migrations::GOALS_MIGRATOR;
use crate::migrations::LOGS_MIGRATOR;
use crate::migrations::MEMORIES_MIGRATOR;
use crate::migrations::QUEUE_MIGRATOR;
use crate::migrations::STATE_MIGRATOR;
use crate::migrations::THREAD_HISTORY_MIGRATOR;
use crate::migrations::runtime_state_migrator;

/// The checksum sqlx would embed for the CRLF image of a migration: the byte
/// form every pre-ticket-18 Windows checkout fed to `sqlx::migrate!`.
fn crlf_image_checksum(migration: &Migration) -> Vec<u8> {
    let image = Migration::new(
        migration.version,
        migration.description.clone(),
        migration.migration_type,
        AssertSqlSafe(migration.sql.as_str().replace('\n', "\r\n")).into_sql_str(),
        migration.no_tx,
    );
    image.checksum.into_owned()
}

fn crlf_image_checksum_for(migrator: &Migrator, version: i64) -> Vec<u8> {
    let migration = migrator
        .migrations
        .iter()
        .find(|migration| migration.version == version)
        .expect("migration should exist");
    crlf_image_checksum(migration)
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

fn stored_history(pool: &sqlx::SqlitePool) -> Vec<(i64, Vec<u8>)> {
    sqlx::query("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
        .fetch_all(pool)
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

    // Stamp the stored history with the CRLF images of the embedded SQL: the
    // byte form pre-ticket-18 Windows builds fed to sqlx::migrate!.
    for migration in STATE_MIGRATOR.migrations.iter() {
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(crlf_image_checksum(migration).as_slice())
            .bind(migration.version)
            .execute(&pool)
            .await
            .expect("CRLF-family checksum should update");
    }

    // The LF-family migrator must reject that history before the heal.
    let strict_error = STATE_MIGRATOR
        .run(&pool)
        .await
        .expect_err("LF-family migrator should reject a CRLF-family history");
    assert!(matches!(strict_error, MigrateError::VersionMismatch(_)));

    repair_eol_checksum_family(&pool, &runtime_state_migrator())
        .await
        .expect("EOL-only checksum flip should be healed");

    assert_eq!(stored_history(&pool), embedded_history(&STATE_MIGRATOR));

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
            .bind(crlf_image_checksum(migration).as_slice())
            .bind(migration.version)
            .execute(&pool)
            .await
            .expect("CRLF-family checksum should update");
    }
    sqlx::query("DROP TABLE thread_sections")
        .execute(&pool)
        .await
        .expect("schema drift should apply");

    let heal_error = repair_eol_checksum_family(&pool, &runtime_state_migrator())
        .await
        .expect_err("schema drift must hard-fail instead of rewriting checksums");
    assert!(heal_error.to_string().contains("schema"));

    // Nothing was rewritten: the history still carries the CRLF family.
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(&pool)
            .await
            .expect("version 1 row should load");
    assert_eq!(stored, crlf_image_checksum_for(&STATE_MIGRATOR, 1));

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

    let heal_error = repair_eol_checksum_family(&pool, &runtime_state_migrator())
        .await
        .expect_err("checksums outside both line-ending families must hard-fail");
    assert!(heal_error.to_string().contains("not an EOL-only difference"));

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

    // A pre-rename CRLF-family binary: migrations 1..37 plus the recency
    // migration recorded as version 38.
    let pre_recency = migrator_through(&STATE_MIGRATOR, /*max_version*/ 37);
    let recency = STATE_MIGRATOR
        .migrations
        .iter()
        .find(|migration| migration.version == 39)
        .expect("recency migration should exist");
    let mut legacy_migrations = pre_recency.migrations.iter().cloned().collect::<Vec<_>>();
    // The recency migration as the pre-rename binary recorded it: version 38.
    legacy_migrations.push(Migration::new(
        38,
        recency.description.clone(),
        recency.migration_type,
        AssertSqlSafe(recency.sql.as_str().replace('\n', "\r\n")).into_sql_str(),
        recency.no_tx,
    ));
    Migrator::with_migrations(legacy_migrations)
        .run(&pool)
        .await
        .expect("legacy recency migrations should apply");

    repair_eol_checksum_family(&pool, &runtime_state_migrator())
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

    repair_eol_checksum_family(&pool, &runtime_state_migrator())
        .await
        .expect("a matching history should not error the heal");

    assert_eq!(stored_history(&pool), embedded_history(&STATE_MIGRATOR));

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
        migrator
            .run(&pool)
            .await
            .expect("migrations should apply");

        // The DDL replay must reproduce the exact catalog of a database the
        // migrator itself built, for every runtime migration set: the gate in
        // repair_eol_checksum_family compares against exactly this replay.
        let mut replay = SchemaReplay::default();
        for migration in migrator.migrations.iter() {
            replay.apply_migration(migration.sql.as_str());
        }
        replay.objects.insert("_sqlx_migrations".to_string(), ObjectKind::Table);
        let actual = actual_schema_inventory(&pool)
            .await
            .expect("catalog inventory should load");
        assert_eq!(actual, replay.objects);

        pool.close().await;
    }
}
