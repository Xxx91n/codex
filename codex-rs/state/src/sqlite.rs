//! Shared SQLite connection configuration.

#![expect(
    clippy::disallowed_methods,
    reason = "this is the centralized SQLite connection shim"
)]

use crate::DbTelemetry;
use crate::checksum_family_notice::maybe_show_family_notice;
use crate::eol_checksum_repair::ChecksumFamily;
use crate::eol_checksum_repair::detect_checksum_family_drift;
use crate::eol_checksum_repair::migrator_embedded_family;
use crate::eol_checksum_repair::repair_eol_checksum_family;
use crate::migrations::repair_legacy_recency_migration_version;
use crate::runtime::RuntimeDbInitError;
use crate::telemetry;
use crate::telemetry::DbKind;
use codex_utils_absolute_path::AbsolutePathBuf;
use log::LevelFilter;
use sqlx::ConnectOptions;
use sqlx::Error;
use sqlx::SqlitePool;
use sqlx::migrate::MigrateError;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteAutoVacuum;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqliteJournalMode;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::sqlite::SqliteSynchronous;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

const LOGS_DB_FILENAME: &str = "logs_2.sqlite";
const GOALS_DB_FILENAME: &str = "goals_1.sqlite";
const MEMORIES_DB_FILENAME: &str = "memories_1.sqlite";
const QUEUE_DB_FILENAME: &str = "queue_1.sqlite";
const STATE_DB_FILENAME: &str = "state_5.sqlite";
const THREAD_HISTORY_DB_FILENAME: &str = "thread_history_1.sqlite";

#[derive(Clone, Copy)]
struct RuntimeDbSpec {
    label: &'static str,
    filename: &'static str,
    kind: DbKind,
    open_phase: &'static str,
    migrate_phase: &'static str,
}

impl RuntimeDbSpec {
    fn path(self, codex_home: &Path) -> PathBuf {
        codex_home.join(self.filename)
    }
}

const STATE_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "state DB",
    filename: STATE_DB_FILENAME,
    kind: DbKind::State,
    open_phase: "open_state",
    migrate_phase: "migrate_state",
};

const LOGS_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "log DB",
    filename: LOGS_DB_FILENAME,
    kind: DbKind::Logs,
    open_phase: "open_logs",
    migrate_phase: "migrate_logs",
};

const GOALS_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "goals DB",
    filename: GOALS_DB_FILENAME,
    kind: DbKind::Goals,
    open_phase: "open_goals",
    migrate_phase: "migrate_goals",
};

const MEMORIES_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "memories DB",
    filename: MEMORIES_DB_FILENAME,
    kind: DbKind::Memories,
    open_phase: "open_memories",
    migrate_phase: "migrate_memories",
};

const MEMORIES_V2_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "memories v2 DB",
    filename: "memories_v2_1.sqlite",
    ..MEMORIES_DB
};

const QUEUE_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "queue DB",
    filename: QUEUE_DB_FILENAME,
    kind: DbKind::Queue,
    open_phase: "open_queue",
    migrate_phase: "migrate_queue",
};

const THREAD_HISTORY_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "thread history DB",
    filename: THREAD_HISTORY_DB_FILENAME,
    kind: DbKind::ThreadHistory,
    open_phase: "open_thread_history",
    migrate_phase: "migrate_thread_history",
};

const RUNTIME_DBS: [RuntimeDbSpec; 7] = [
    STATE_DB,
    LOGS_DB,
    GOALS_DB,
    MEMORIES_DB,
    MEMORIES_V2_DB,
    QUEUE_DB,
    THREAD_HISTORY_DB,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDbPath {
    pub label: &'static str,
    pub path: PathBuf,
}

/// Resolved configuration shared by all Codex SQLite connections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteConfig {
    sqlite_home: AbsolutePathBuf,
    /// Explicit checksum-family maintenance opt-in (ticket 31, re-targeted
    /// by ticket 35). `Some(Crlf)` keeps every database in the CRLF family
    /// and `Some(Lf)` in the LF family across startups: on a startup
    /// `VersionMismatch` the runtime first rewrites the stored history to
    /// this binary's embedded family so sqlx can validate and migrate, then
    /// rewrites it again to the configured family after the migration run.
    /// `None` is the default `auto` behavior since ticket 35: follow the
    /// family this binary embeds (the official platform family), and a
    /// family drift fails loudly with the repair command instead of
    /// rewriting anything. The user-facing knob is
    /// `[state] migration_checksum_family` in `config.toml` (see the docs).
    maintained_checksum_family: Option<ChecksumFamily>,
}

impl SqliteConfig {
    pub fn from_sqlite_home(sqlite_home: AbsolutePathBuf) -> Self {
        Self {
            sqlite_home,
            maintained_checksum_family: None,
        }
    }

    pub fn new_for_testing(sqlite_home: AbsolutePathBuf) -> Self {
        Self::from_sqlite_home(sqlite_home)
    }

    /// Set the maintained checksum family (see the field doc). `None` is the
    /// default `auto` behavior; `Some(..)` marks the explicit ticket-31
    /// opt-in maintenance families (D-015, re-targeted by ticket 35).
    pub fn with_maintained_checksum_family(mut self, family: Option<ChecksumFamily>) -> Self {
        self.maintained_checksum_family = family;
        self
    }

    /// The checksum family the runtime maintains across startups.
    pub fn maintained_checksum_family(&self) -> Option<ChecksumFamily> {
        self.maintained_checksum_family
    }

    pub fn home(&self) -> &Path {
        self.sqlite_home.as_path()
    }

    /// Return the path to the primary state database.
    pub fn state_db_path(&self) -> PathBuf {
        STATE_DB.path(self.home())
    }

    /// Return the path to the logs database.
    pub fn logs_db_path(&self) -> PathBuf {
        LOGS_DB.path(self.home())
    }

    /// Return the path to the goals database.
    pub fn goals_db_path(&self) -> PathBuf {
        GOALS_DB.path(self.home())
    }

    /// Return the path to the memories database.
    pub fn memories_db_path(&self) -> PathBuf {
        MEMORIES_DB.path(self.home())
    }

    pub(crate) fn memories_v2_db_path(&self) -> PathBuf {
        MEMORIES_V2_DB.path(self.home())
    }

    pub(crate) async fn open_memories_v2_db(&self) -> anyhow::Result<SqlitePool> {
        self.open_runtime_db(
            MEMORIES_V2_DB,
            &crate::migrations::runtime_memories_migrator(),
            /*telemetry_override*/ None,
        )
        .await
    }

    /// Return the path to the durable user-message queue database.
    pub fn queue_db_path(&self) -> PathBuf {
        QUEUE_DB.path(self.home())
    }

    /// Return the path to the paginated thread-history database.
    pub fn thread_history_db_path(&self) -> PathBuf {
        THREAD_HISTORY_DB.path(self.home())
    }

    /// Return the paths to every database managed by the state runtime.
    pub fn runtime_db_paths(&self) -> Vec<RuntimeDbPath> {
        RUNTIME_DBS
            .iter()
            .map(|spec| RuntimeDbPath {
                label: spec.label,
                path: spec.path(self.home()),
            })
            .collect()
    }

    pub(super) async fn open_state_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        // New state DBs should use incremental auto-vacuum, but retrofitting an
        // existing DB requires a full VACUUM. Do not attempt that during process
        // startup: it is maintenance work that can contend with foreground writers.
        self.open_runtime_db(STATE_DB, migrator, telemetry_override)
            .await
    }

    pub(super) async fn open_logs_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        self.open_runtime_db(LOGS_DB, migrator, telemetry_override)
            .await
    }

    pub(super) async fn open_goals_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        self.open_runtime_db(GOALS_DB, migrator, telemetry_override)
            .await
    }

    pub(super) async fn open_memories_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        self.open_runtime_db(MEMORIES_DB, migrator, telemetry_override)
            .await
    }

    pub(super) async fn open_queue_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        self.open_runtime_db(QUEUE_DB, migrator, telemetry_override)
            .await
    }

    pub(super) async fn open_thread_history_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        self.open_runtime_db(THREAD_HISTORY_DB, migrator, telemetry_override)
            .await
    }

    async fn open_runtime_db(
        &self,
        spec: RuntimeDbSpec,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        let path = spec.path(self.home());
        let started = Instant::now();
        let pool_result = self
            .open_read_write_pool(&path)
            .await
            .map_err(anyhow::Error::from);
        telemetry::record_init_result(
            telemetry_override,
            spec.kind,
            spec.open_phase,
            started.elapsed(),
            &pool_result,
        );
        let pool = pool_result.map_err(|source| {
            RuntimeDbInitError::new(spec.label, "open", path.as_path(), source)
        })?;
        let started = Instant::now();
        let migrate_result = async {
            if matches!(spec.kind, DbKind::State) {
                repair_legacy_recency_migration_version(&pool, migrator).await?;
            }
            if let Err(error) = migrator.run(&pool).await.map_err(anyhow::Error::from) {
                let version_mismatch = matches!(
                    error.downcast_ref::<MigrateError>(),
                    Some(MigrateError::VersionMismatch(_))
                );
                if !version_mismatch {
                    return Err(error);
                }
                let binary_family = embedded_binary_family(migrator);
                if self.maintained_checksum_family.is_none() {
                    // Ticket 35 (A-021/A-022/A-023): default `auto` follows
                    // the embedded (official platform) family and rewrites
                    // nothing. A pure family drift fails loudly with both
                    // families and the explicit repair command; anything the
                    // read-only probe cannot certify as EOL-only keeps
                    // sqlx's own VersionMismatch error verbatim.
                    let drift = detect_checksum_family_drift(&pool, migrator, binary_family)
                        .await
                        .unwrap_or(None);
                    return match drift {
                        Some(drift) => {
                            let fingerprint = drift.fingerprint();
                            maybe_show_family_notice(
                                self.home(),
                                &fingerprint,
                                &drift.notice_text(),
                            );
                            Err(anyhow::anyhow!(drift.guidance()))
                        }
                        None => Err(error),
                    };
                };
                // Ticket 31 explicit opt-in (crlf/lf): when every applied
                // checksum is a pure line-ending image of the embedded SQL
                // and the schema still matches, rewrite the stored checksums
                // into the embedded family in one transaction, retry the
                // migration, then land the history in the configured family
                // below. Anything else keeps failing loudly.
                repair_eol_checksum_family(&pool, migrator, binary_family).await?;
                migrator.run(&pool).await.map_err(anyhow::Error::from)?;
            }
            Ok(())
        }
        .await;
        telemetry::record_init_result(
            telemetry_override,
            spec.kind,
            spec.migrate_phase,
            started.elapsed(),
            &migrate_result,
        );
        if let Err(source) = migrate_result {
            pool.close().await;
            return Err(
                RuntimeDbInitError::new(spec.label, "migrate", path.as_path(), source).into(),
            );
        }
        // Land the history in the configured maintenance family when the user
        // opted in via `[state] migration_checksum_family = "crlf" | "lf"`.
        // The embedded-family migrator just stamped the new rows, so on a
        // different maintenance target every row needs one rewrite: the same
        // gates as the offline `codex state fix-checksums` subcommand apply,
        // and at steady state the schema matches by construction. The
        // transaction is atomic; a startup on the next run sees
        // VersionMismatch, re-validates into the embedded family, and flips
        // back, which is the documented maintenance loop.
        if let Some(target) = self.maintained_checksum_family
            && target != embedded_binary_family(migrator)
            && let Err(source) = repair_eol_checksum_family(&pool, migrator, target).await
        {
            let operation = match target {
                ChecksumFamily::Crlf => "maintain_crlf_checksum_family",
                ChecksumFamily::Lf => "maintain_lf_checksum_family",
            };
            pool.close().await;
            return Err(RuntimeDbInitError::new(spec.label, operation, path.as_path(), source)
                .into());
        }
        Ok(pool)
    }

    /// Open a writable Codex SQLite database, creating it if necessary.
    pub async fn open_read_write_pool(&self, path: &Path) -> Result<SqlitePool, Error> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .auto_vacuum(SqliteAutoVacuum::Incremental)
            .busy_timeout(Duration::from_secs(5))
            .log_statements(LevelFilter::Off);
        SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
    }

    /// Open an existing Codex SQLite database without creating or modifying it.
    pub async fn open_read_only_pool(
        &self,
        path: &Path,
        busy_timeout: Option<Duration>,
    ) -> Result<SqlitePool, Error> {
        let mut options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .read_only(true)
            .log_statements(LevelFilter::Off);
        if let Some(busy_timeout) = busy_timeout {
            options = options.busy_timeout(busy_timeout);
        }
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
    }
}

/// The checksum family this binary embeds for a runtime migrator: the
/// official platform default family since ticket 35 (CRLF on Windows
/// checkouts, LF on Linux/macOS checkouts).
fn embedded_binary_family(migrator: &Migrator) -> ChecksumFamily {
    migrator_embedded_family(migrator)
}
