//! Startup self-heal for the EOL-only sqlx migration checksum family flip.
//!
//! `sqlx::migrate!` embeds the migration `.sql` bytes and their SHA-384
//! checksums into the binary at compile time, so those bytes depend on the
//! checkout's line endings: the same commit checked out with CRLF and with LF
//! produces two incompatible checksum families, and sqlx rejects a database
//! written by the other family with `MigrateError::VersionMismatch` before it
//! applies anything. Ticket 18 pinned the migration files to LF via
//! `.gitattributes`, which flipped the family for every Windows build; the
//! databases written by historical CRLF-family Windows binaries (verified
//! byte-for-byte during diagnosis: the stored v1 checksum is the SHA-384 of
//! the CRLF image of the embedded SQL) stopped opening at startup.
//!
//! The heal is deliberately narrow. For every applied migration row, the
//! stored checksum must equal the embedded checksum or the checksum of the
//! CRLF image of the embedded SQL (the exact byte transform Windows runners
//! with `core.autocrlf=true` applied: every `\n` became `\r\n`, and the
//! sources contain no lone CR bytes). The one documented exception is the
//! legacy recency row recorded as version 38 that
//! `repair_legacy_recency_migration_version` repairs for LF-family databases.
//! The database schema must still match the DDL the effective migrations
//! build (replayed statement by statement), and every rewrite happens in one
//! transaction. Anything else hard-fails, so a database that was not produced
//! by this migration lineage is never re-stamped.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use sqlx::AssertSqlSafe;
use sqlx::Connection;
use sqlx::SqlSafeStr;
use sqlx::SqlitePool;
use sqlx::migrate::Migration;
use sqlx::migrate::Migrator;

/// The line-ending family sqlx's embedded checksums are stamped in. The
/// runtime ships with the LF family (ticket 18 locked the SQL files to LF
/// via `.gitattributes`); the CRLF family is the byte image every
/// pre-ticket-18 Windows build fed to `sqlx::migrate!`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChecksumFamily {
    /// The embedded (LF) family: checksums `sqlx::migrate!` embeds from
    /// the on-disk SQL bytes.
    Lf,
    /// The CRLF image family: checksums a checkout with `core.autocrlf=true`
    /// would have produced before ticket 18 normalized the sources.
    Crlf,
}

impl ChecksumFamily {
    /// Stable lowercase spelling used in config, CLI flags, and JSON reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "lf",
            Self::Crlf => "crlf",
        }
    }
}

/// Identify which line-ending family a stored row's checksum belongs to.
/// Returns `None` when the stored value matches neither the embedded (LF)
/// checksum nor the CRLF image of the embedded SQL: the row is not a pure
/// EOL difference and the self-heal must refuse to rewrite it.
pub(crate) fn checksum_family(migration: &Migration, stored: &[u8]) -> Option<ChecksumFamily> {
    if stored == migration.checksum.as_ref() {
        Some(ChecksumFamily::Lf)
    } else if crlf_checksum(migration).is_some_and(|image| stored == image.as_slice()) {
        Some(ChecksumFamily::Crlf)
    } else {
        None
    }
}

/// The checksum bytes a row must carry for the target family. `None` when
/// the target family has no representable checksum for this migration (the
/// CRLF image is uncomputable for any migration whose SQL already contains
/// a CR byte).
pub(crate) fn target_checksum(family: ChecksumFamily, migration: &Migration) -> Option<Vec<u8>> {
    match family {
        ChecksumFamily::Lf => Some(migration.checksum.to_vec()),
        ChecksumFamily::Crlf => crlf_checksum(migration),
    }
}

const LEGACY_RECENCY_APPLIED_VERSION: i64 = 38;
const RECENCY_VERSION: i64 = 39;

/// Heal a line-ending-only sqlx migration checksum family flip into the
/// requested target family; the caller retries `Migrator::run` against the
/// repaired history afterwards. `target` = `ChecksumFamily::Lf` rewrites
/// rows that carry the CRLF image back to the embedded (LF) checksum;
/// `target` = `ChecksumFamily::Crlf` rewrites LF-stamped rows to the
/// CRLF image so an official CLI of the same commit can open the database
/// without its own self-heal. Returns the list of migration versions the
/// transaction rewrote (empty when the history already matches the target).
pub(crate) async fn repair_eol_checksum_family(
    pool: &SqlitePool,
    migrator: &Migrator,
    target: ChecksumFamily,
) -> anyhow::Result<Vec<i64>> {
    let migrations_table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?
    .is_some();
    if !migrations_table_exists {
        anyhow::bail!(
            "EOL checksum self-heal: _sqlx_migrations is missing though migration validation \
             reported a version mismatch"
        );
    }
    let applied = sqlx::query_as::<_, (i64, Vec<u8>)>(
        "SELECT version, checksum FROM _sqlx_migrations ORDER BY version",
    )
    .fetch_all(pool)
    .await?;
    if applied.is_empty() {
        anyhow::bail!(
            "EOL checksum self-heal: no applied migrations though migration validation reported \
             a version mismatch"
        );
    }
    let embedded: BTreeMap<i64, &Migration> = migrator
        .migrations
        .iter()
        .map(|migration| (migration.version, migration))
        .collect();
    let applied_versions: BTreeSet<i64> = applied.iter().map(|(version, _)| *version).collect();

    let mut updates: Vec<(i64, Vec<u8>)> = Vec::new();
    let mut legacy_rename: Option<(String, Vec<u8>)> = None;
    // The migrations whose DDL the schema gate replays, in applied order.
    let mut effective: Vec<&Migration> = Vec::new();
    for (version, stored) in &applied {
        let Some(migration) = embedded.get(version).copied() else {
            anyhow::bail!(
                "EOL checksum self-heal: applied migration {version} is unknown to this binary; \
                 refusing the EOL-only checksum rewrite"
            );
        };
        match checksum_family(migration, stored.as_slice()) {
            Some(family) if family == target => {
                // Row is already in the target family; nothing to rewrite.
                effective.push(migration);
            }
            Some(_) => {
                // Row is in the other line-ending family: rewrite it to the
                // target family in the same transaction.
                let Some(new) = target_checksum(target, migration) else {
                    anyhow::bail!(
                        "EOL checksum self-heal: target family has no representable \
                         checksum for migration {version}; refusing the rewrite"
                    );
                };
                updates.push((*version, new));
                effective.push(migration);
            }
            None => {
                // Legacy recency row: a pre-rename binary recorded the
                // recency migration as version 38 with the recency SQL's
                // checksum. The stored value can be the LF or CRLF image of
                // the recency SQL; the heal rewrites the row to version 39
                // in the target family in the same transaction.
                let is_legacy_recency_row = *version == LEGACY_RECENCY_APPLIED_VERSION
                    && !applied_versions.contains(&RECENCY_VERSION);
                if !is_legacy_recency_row {
                    anyhow::bail!(
                        "EOL checksum self-heal: migration {version} checksum matches neither the \
                         embedded checksum nor the CRLF image of the embedded SQL; not an EOL-only \
                         difference"
                    );
                }
                let Some(recency) = embedded.get(&RECENCY_VERSION).copied() else {
                    anyhow::bail!(
                        "EOL checksum self-heal: recency migration {RECENCY_VERSION} is missing from \
                         the embedded set"
                    );
                };
                if checksum_family(recency, stored.as_slice()).is_none() {
                    anyhow::bail!(
                        "EOL checksum self-heal: legacy recency row {version} checksum matches neither \
                         the embedded recency checksum nor its CRLF image"
                    );
                }
                let Some(new) = target_checksum(target, recency) else {
                    anyhow::bail!(
                        "EOL checksum self-heal: target family has no representable checksum \
                         for the recency row; refusing the rewrite"
                    );
                };
                legacy_rename = Some((recency.description.to_string(), new));
                effective.push(recency);
            }
        }
    }
    if updates.is_empty() && legacy_rename.is_none() {
        return Ok(Vec::new());
    }

    let mut replay = SchemaReplay::default();
    for migration in &effective {
        replay.apply_migration(migration.sql.as_str());
    }
    replay
        .objects
        .insert("_sqlx_migrations".to_string(), ObjectKind::Table);
    let actual = actual_schema_inventory(pool).await?;
    verify_schema_matches(&replay.objects, &actual)?;

    let mut connection = pool.acquire().await?;
    let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;
    for (version, checksum) in &updates {
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(checksum.as_slice())
            .bind(*version)
            .execute(&mut *transaction)
            .await?;
    }
    if let Some((description, checksum)) = &legacy_rename {
        sqlx::query(
            "UPDATE _sqlx_migrations SET version = ?, description = ?, checksum = ? WHERE \
             version = ?",
        )
        .bind(RECENCY_VERSION)
        .bind(description.as_str())
        .bind(checksum.as_slice())
        .bind(LEGACY_RECENCY_APPLIED_VERSION)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;

    let rewritten: Vec<i64> = updates
        .iter()
        .map(|(version, _)| *version)
        .chain(legacy_rename.as_ref().map(|_| RECENCY_VERSION))
        .collect();
    let rewritten_len = rewritten.len();
    log::info!("rewrote {rewritten_len} EOL-only sqlx migration checksum(s)");
    Ok(rewritten)
}

/// The gate before any rewrite: every catalog object the effective migrations
/// build must exist in the database with the right kind. Additive drift (for
/// example objects from a newer binary) is tolerated, matching the runtime
/// migrator's `ignore_missing` semantics; missing or mistyped objects are
/// not, because they mean the stored history does not describe this schema.
fn verify_schema_matches(
    expected: &BTreeMap<String, ObjectKind>,
    actual: &BTreeMap<String, ObjectKind>,
) -> anyhow::Result<()> {
    let mut problems: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for (name, kind) in expected {
        match actual.get(name) {
            Some(actual_kind) if actual_kind != kind => {
                problems.push(format!(
                    "{name} exists as {actual_kind:?} but the migrations build a {kind:?}"
                ));
            }
            None => missing.push(name.clone()),
            Some(_) => {}
        }
    }
    if !missing.is_empty() {
        problems.push(format!("missing {}", preview(&missing)));
    }
    if !problems.is_empty() {
        anyhow::bail!(
            "EOL checksum self-heal: schema does not match the migrations recorded as applied \
             ({}); refusing the checksum rewrite",
            problems.join("; ")
        );
    }
    Ok(())
}

fn preview(names: &[String]) -> String {
    const LIMIT: usize = 8;
    let shown = names
        .iter()
        .take(LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() <= LIMIT {
        shown
    } else {
        let rest = names.len() - LIMIT;
        format!("{shown}, and {rest} more")
    }
}

async fn actual_schema_inventory(
    pool: &SqlitePool,
) -> anyhow::Result<BTreeMap<String, ObjectKind>> {
    let rows = sqlx::query_as::<_, (String, String)>("SELECT name, type FROM sqlite_master")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .filter(|(name, _)| !name.starts_with("sqlite_"))
        .filter_map(|(name, kind)| ObjectKind::from_sqlite_type(&kind).map(|kind| (name, kind)))
        .collect())
}

/// The checksum the CRLF image of the embedded SQL would produce: the byte
/// form every `core.autocrlf=true` Windows checkout fed to `sqlx::migrate!`
/// before ticket 18. `None` when the SQL already contains CR bytes and a
/// line-ending image is therefore not computable.
pub(crate) fn crlf_checksum(migration: &Migration) -> Option<Vec<u8>> {
    let sql = migration.sql.as_str();
    if sql.contains('\r') {
        return None;
    }
    let image = Migration::new(
        migration.version,
        migration.description.clone(),
        migration.migration_type,
        AssertSqlSafe(sql.replace('\n', "\r\n")).into_sql_str(),
        migration.no_tx,
    );
    Some(image.checksum.into_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObjectKind {
    Table,
    Index,
    Trigger,
    View,
}

impl ObjectKind {
    pub(crate) fn from_sqlite_type(kind: &str) -> Option<Self> {
        match kind {
            "table" => Some(Self::Table),
            "index" => Some(Self::Index),
            "trigger" => Some(Self::Trigger),
            "view" => Some(Self::View),
            _ => None,
        }
    }
}

/// Replays migration DDL into the catalog inventory a migrated database must
/// contain. Deliberately conservative: statements the scanner cannot parse
/// are skipped, so the replay can only under-approximate expectations
/// (missing objects still fail loudly); it never invents them.
#[derive(Default)]
pub(crate) struct SchemaReplay {
    pub(crate) objects: BTreeMap<String, ObjectKind>,
    children: BTreeMap<String, BTreeSet<String>>,
}

impl SchemaReplay {
    pub(crate) fn apply_migration(&mut self, sql: &str) {
        let lowered = scrub_sql(sql).to_ascii_lowercase();
        for effect in scan_effects(&lowered) {
            match effect {
                Effect::CreateTable(name) => {
                    self.objects.insert(name.clone(), ObjectKind::Table);
                    self.children.entry(name).or_default();
                }
                Effect::CreateChild { name, table, kind } => {
                    self.objects.insert(name.clone(), kind);
                    if let Some(children) = self.children.get_mut(&table) {
                        children.insert(name);
                    }
                }
                Effect::CreateView(name) => {
                    self.objects.insert(name, ObjectKind::View);
                }
                Effect::DropTable(name) => {
                    self.objects.remove(&name);
                    if let Some(children) = self.children.remove(&name) {
                        for child in children {
                            self.objects.remove(&child);
                        }
                    }
                }
                Effect::DropObject(name) => {
                    self.objects.remove(&name);
                    for children in self.children.values_mut() {
                        children.remove(&name);
                    }
                }
                Effect::RenameTable { from, to } => {
                    if let Some(children) = self.children.remove(&from) {
                        self.children.insert(to.clone(), children);
                    }
                    if let Some(kind) = self.objects.remove(&from) {
                        self.objects.insert(to, kind);
                    }
                }
            }
        }
    }
}

enum Effect {
    CreateTable(String),
    CreateChild {
        name: String,
        table: String,
        kind: ObjectKind,
    },
    CreateView(String),
    DropTable(String),
    DropObject(String),
    RenameTable {
        from: String,
        to: String,
    },
}

fn scan_effects(lowered: &str) -> Vec<Effect> {
    let mut hits: Vec<(usize, &'static str)> = Vec::new();
    for keyword in ["create", "drop", "alter"] {
        let mut from = 0;
        while let Some(offset) = lowered[from..].find(keyword) {
            let position = from + offset;
            from = position + keyword.len();
            if !starts_at_word_boundary(lowered, position)
                || is_word_byte_at(lowered, position + keyword.len())
            {
                continue;
            }
            hits.push((position, keyword));
        }
    }
    hits.sort_by_key(|(position, _)| *position);
    let mut effects = Vec::new();
    for (position, keyword) in hits {
        match keyword {
            "create" => parse_create(lowered, position, &mut effects),
            "drop" => parse_drop(lowered, position, &mut effects),
            _ => parse_alter(lowered, position, &mut effects),
        }
    }
    effects
}

fn parse_create(lowered: &str, start: usize, effects: &mut Vec<Effect>) {
    let mut i = skip_ws(lowered, start + "create".len());
    let Some((mut kind, mut end)) = read_word(lowered, i) else {
        return;
    };
    if kind == "unique" {
        i = skip_ws(lowered, end);
        let Some((next, next_end)) = read_word(lowered, i) else {
            return;
        };
        kind = next;
        end = next_end;
    }
    if matches!(kind, "temp" | "temporary" | "virtual") {
        // TEMP objects live in the temp catalog and VIRTUAL tables do not
        // occur in this migration set.
        return;
    }
    match kind {
        "table" => {
            let Some((name, _)) = parse_name(lowered, end) else {
                return;
            };
            effects.push(Effect::CreateTable(name));
        }
        "index" | "trigger" => {
            let Some((name, after_name)) = parse_name(lowered, end) else {
                return;
            };
            let Some(table) = parse_on_target(lowered, after_name) else {
                return;
            };
            let object_kind = if kind == "index" {
                ObjectKind::Index
            } else {
                ObjectKind::Trigger
            };
            effects.push(Effect::CreateChild {
                name,
                table,
                kind: object_kind,
            });
        }
        "view" => {
            let Some((name, _)) = parse_name(lowered, end) else {
                return;
            };
            effects.push(Effect::CreateView(name));
        }
        _ => {}
    }
}

fn parse_drop(lowered: &str, start: usize, effects: &mut Vec<Effect>) {
    let after_drop = skip_ws(lowered, start + "drop".len());
    let Some((kind, end)) = read_word(lowered, after_drop) else {
        return;
    };
    if !matches!(kind, "table" | "index" | "trigger") {
        return;
    }
    let Some((name, _)) = parse_name(lowered, end) else {
        return;
    };
    if kind == "table" {
        effects.push(Effect::DropTable(name));
    } else {
        effects.push(Effect::DropObject(name));
    }
}

fn parse_alter(lowered: &str, start: usize, effects: &mut Vec<Effect>) {
    let after_alter = skip_ws(lowered, start + "alter".len());
    let Some((table_word, end)) = read_word(lowered, after_alter) else {
        return;
    };
    if table_word != "table" {
        return;
    }
    let after_name = skip_ws(lowered, end);
    let Some((from, from_end)) = read_word(lowered, after_name) else {
        return;
    };
    let after_from = skip_ws(lowered, from_end);
    let Some((next, next_end)) = read_word(lowered, after_from) else {
        return;
    };
    if next != "rename" {
        return;
    }
    let after_rename = skip_ws(lowered, next_end);
    let Some((to_keyword, to_keyword_end)) = read_word(lowered, after_rename) else {
        return;
    };
    if to_keyword == "column" {
        // Column renames never change the object inventory.
        return;
    }
    if to_keyword != "to" {
        return;
    }
    let after_to = skip_ws(lowered, to_keyword_end);
    let Some((to, _)) = read_word(lowered, after_to) else {
        return;
    };
    effects.push(Effect::RenameTable {
        from: from.to_string(),
        to: to.to_string(),
    });
}

/// Skip `IF [NOT] EXISTS` after a CREATE/DROP keyword and return the object
/// name plus the offset just past it.
fn parse_name(lowered: &str, start: usize) -> Option<(String, usize)> {
    let mut i = skip_ws(lowered, start);
    if let Some((word, end)) = read_word(lowered, i)
        && word == "if"
    {
        i = skip_ws(lowered, end);
        if let Some((maybe_not, end_not)) = read_word(lowered, i)
            && maybe_not == "not"
        {
            i = skip_ws(lowered, end_not);
        }
        let (exists, exists_end) = read_word(lowered, i)?;
        if exists != "exists" {
            return None;
        }
        i = skip_ws(lowered, exists_end);
    }
    let (name, end) = read_word(lowered, i)?;
    Some((name.to_string(), end))
}

/// Walk the header of a CREATE INDEX/TRIGGER statement to the token after the
/// first standalone `ON` (the target table).
fn parse_on_target(lowered: &str, mut i: usize) -> Option<String> {
    loop {
        i = skip_ws(lowered, i);
        if i >= lowered.len() || lowered.as_bytes()[i] == b';' {
            return None;
        }
        let Some((word, end)) = read_word(lowered, i) else {
            i += 1;
            continue;
        };
        if word == "on" {
            let (table, _) = read_word(lowered, skip_ws(lowered, end))?;
            return Some(table.to_string());
        }
        i = end;
    }
}

/// Blank out SQL comments and quoted regions (one output char per input char)
/// so keyword scanning cannot be fooled by text inside them.
fn scrub_sql(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut rest = sql;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut quote: Option<char> = None;
    while let Some(c) = rest.chars().next() {
        rest = &rest[c.len_utf8()..];
        if in_line_comment {
            out.push(' ');
            if c == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if in_block_comment {
            out.push(' ');
            if c == '*' && rest.starts_with('/') {
                rest = &rest[1..];
                out.push(' ');
                in_block_comment = false;
            }
            continue;
        }
        if let Some(quote_close) = quote {
            out.push(' ');
            if c == quote_close {
                if rest.starts_with(quote_close) {
                    rest = &rest[quote_close.len_utf8()..];
                    out.push(' ');
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match c {
            '-' if rest.starts_with('-') => {
                rest = &rest[1..];
                out.push_str("  ");
                in_line_comment = true;
            }
            '/' if rest.starts_with('*') => {
                rest = &rest[1..];
                out.push_str("  ");
                in_block_comment = true;
            }
            '\'' | '"' | '`' | '[' => {
                out.push(' ');
                quote = Some(if c == '[' { ']' } else { c });
            }
            _ => out.push(c),
        }
    }
    out
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_word_byte_at(lowered: &str, i: usize) -> bool {
    lowered
        .as_bytes()
        .get(i)
        .is_some_and(|byte| is_word_byte(*byte))
}

fn starts_at_word_boundary(lowered: &str, position: usize) -> bool {
    position == 0 || !is_word_byte_at(lowered, position - 1)
}

fn skip_ws(lowered: &str, mut i: usize) -> usize {
    let bytes = lowered.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn read_word(lowered: &str, i: usize) -> Option<(&str, usize)> {
    let bytes = lowered.as_bytes();
    if i >= bytes.len() || !is_word_byte(bytes[i]) {
        return None;
    }
    let mut end = i;
    while end < bytes.len() && is_word_byte(bytes[end]) {
        end += 1;
    }
    Some((&lowered[i..end], end))
}

#[cfg(test)]
#[path = "eol_checksum_repair_tests.rs"]
mod tests;
