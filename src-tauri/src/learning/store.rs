//! SQLite rule store (`learning.db` in the app data directory).

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use rusqlite_migration::{Migrations, M};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use super::core::learner::NewRule;
use super::core::rules::{Rule, RuleKind};
use super::core::text::normalize_phrase;

static MIGRATIONS: &[M] = &[M::up(
    "CREATE TABLE rules (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        from_text TEXT NOT NULL,
        to_text TEXT NOT NULL,
        from_norm TEXT NOT NULL,
        to_norm TEXT NOT NULL,
        kind TEXT NOT NULL,
        hits INTEGER NOT NULL DEFAULT 1,
        misses INTEGER NOT NULL DEFAULT 0,
        applied INTEGER NOT NULL DEFAULT 0,
        paused BOOLEAN NOT NULL DEFAULT 0,
        source_sentence TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        UNIQUE(from_norm, to_norm)
    );",
)];

const COLUMNS: &str = "id, from_text, to_text, kind, hits, misses, applied, paused, \
                       source_sentence, created_at, updated_at";

pub struct RuleStore {
    conn: Mutex<Connection>,
}

impl RuleStore {
    /// Opens (and creates, and migrates) the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::from_connection(Connection::open(path)?)
    }

    /// A store that lives in memory, for tests.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut conn: Connection) -> Result<Self> {
        Migrations::new(MIGRATIONS.to_vec()).to_latest(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// A panic while holding the lock cannot leave the connection half-written
    /// (SQLite is transactional), so a poisoned lock is safe to reuse.
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Every rule, newest first.
    pub fn all(&self) -> Result<Vec<Rule>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM rules ORDER BY id DESC"))?;
        let rows = stmt.query_map([], row_to_rule)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Stores a new rule with one hit. When a rule with the same normalised
    /// `from` and `to` exists, that rule gets one more hit instead.
    pub fn insert(&self, r: &NewRule, now: i64) -> Result<Rule> {
        let conn = self.lock();
        let (from_norm, to_norm) = (normalize_phrase(&r.from), normalize_phrase(&r.to));
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM rules WHERE from_norm = ?1 AND to_norm = ?2",
                params![from_norm, to_norm],
                |row| row.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => {
                conn.execute(
                    "UPDATE rules SET hits = hits + 1, updated_at = ?2 WHERE id = ?1",
                    params![id, now],
                )?;
                id
            }
            None => {
                conn.execute(
                    "INSERT INTO rules (from_text, to_text, from_norm, to_norm, kind, hits, \
                     source_sentence, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?7)",
                    params![
                        r.from,
                        r.to,
                        from_norm,
                        to_norm,
                        r.kind.as_str(),
                        r.source_sentence,
                        now
                    ],
                )?;
                conn.last_insert_rowid()
            }
        };
        get(&conn, id)
    }

    pub fn hit(&self, id: i64, now: i64) -> Result<()> {
        self.bump("hits", id, Some(now))
    }

    pub fn miss(&self, id: i64, now: i64) -> Result<()> {
        self.bump("misses", id, Some(now))
    }

    pub fn applied(&self, id: i64) -> Result<()> {
        self.bump("applied", id, None)
    }

    fn bump(&self, column: &str, id: i64, now: Option<i64>) -> Result<()> {
        // `column` is one of three constants above, never user input.
        let conn = self.lock();
        match now {
            Some(now) => conn.execute(
                &format!("UPDATE rules SET {column} = {column} + 1, updated_at = ?2 WHERE id = ?1"),
                params![id, now],
            )?,
            None => conn.execute(
                &format!("UPDATE rules SET {column} = {column} + 1 WHERE id = ?1"),
                params![id],
            )?,
        };
        Ok(())
    }

    /// Edits a rule's text and pause flag. Fails when no rule has this id, or
    /// when another rule already has the same normalised `from` and `to`.
    pub fn update(&self, id: i64, from: &str, to: &str, paused: bool, now: i64) -> Result<Rule> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE rules SET from_text = ?2, to_text = ?3, from_norm = ?4, to_norm = ?5, \
             paused = ?6, updated_at = ?7 WHERE id = ?1",
            params![
                id,
                from,
                to,
                normalize_phrase(from),
                normalize_phrase(to),
                paused,
                now
            ],
        )?;
        if changed == 0 {
            return Err(anyhow!("no learning rule with id {id}"));
        }
        get(&conn, id)
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.lock()
            .execute("DELETE FROM rules WHERE id = ?1", params![id])?;
        Ok(())
    }
}

fn get(conn: &Connection, id: i64) -> Result<Rule> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM rules WHERE id = ?1"),
        params![id],
        row_to_rule,
    )
    .map_err(Into::into)
}

fn row_to_rule(row: &Row<'_>) -> rusqlite::Result<Rule> {
    let kind: String = row.get(3)?;
    Ok(Rule {
        id: row.get(0)?,
        from: row.get(1)?,
        to: row.get(2)?,
        kind: RuleKind::parse(&kind).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                format!("unknown rule kind {kind:?}").into(),
            )
        })?,
        hits: row.get(4)?,
        misses: row.get(5)?,
        applied: row.get(6)?,
        paused: row.get(7)?,
        source_sentence: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_rule(from: &str, to: &str) -> NewRule {
        NewRule {
            from: from.into(),
            to: to.into(),
            kind: RuleKind::Word,
            source_sentence: format!("sentence with {to}"),
        }
    }

    fn store() -> RuleStore {
        RuleStore::open_in_memory().unwrap()
    }

    #[test]
    fn insert_then_all_returns_newest_first() {
        let s = store();
        let a = s.insert(&new_rule("Calvin", "Kelvin"), 10).unwrap();
        let b = s.insert(&new_rule("heiress", "Eres"), 20).unwrap();
        assert_eq!((a.hits, a.misses, a.applied, a.paused), (1, 0, 0, false));
        assert_eq!((a.created_at, a.updated_at), (10, 10));
        assert_eq!(a.source_sentence, "sentence with Kelvin");
        assert_eq!(s.all().unwrap(), vec![b, a]);
    }

    #[test]
    fn duplicate_insert_increments_hits() {
        let s = store();
        let a = s.insert(&new_rule("Calvin", "Kelvin"), 10).unwrap();
        let b = s.insert(&new_rule("  calvin ", "Kelvin."), 30).unwrap();
        assert_eq!(b.id, a.id);
        assert_eq!((b.hits, b.updated_at, b.created_at), (2, 30, 10));
        assert_eq!(s.all().unwrap().len(), 1);
    }

    #[test]
    fn counters_move() {
        let s = store();
        let id = s.insert(&new_rule("Calvin", "Kelvin"), 10).unwrap().id;
        s.hit(id, 11).unwrap();
        s.miss(id, 12).unwrap();
        s.miss(id, 13).unwrap();
        s.applied(id).unwrap();
        let r = &s.all().unwrap()[0];
        assert_eq!((r.hits, r.misses, r.applied, r.updated_at), (2, 2, 1, 13));
    }

    #[test]
    fn update_changes_text_norms_and_pause() {
        let s = store();
        let id = s.insert(&new_rule("Calvin", "Kelvin"), 10).unwrap().id;
        let r = s.update(id, "Calvyn", "Kelvin K", true, 40).unwrap();
        assert_eq!(
            (r.from.as_str(), r.to.as_str(), r.paused, r.updated_at),
            ("Calvyn", "Kelvin K", true, 40)
        );
        // The new norms drive duplicate detection.
        let again = s.insert(&new_rule("calvyn", "kelvin k"), 50).unwrap();
        assert_eq!((again.id, again.hits), (id, 2));
        assert!(s.update(999, "a", "b", false, 1).is_err());
    }

    #[test]
    fn update_to_an_existing_pair_is_an_error() {
        let s = store();
        s.insert(&new_rule("a", "b"), 1).unwrap();
        let id = s.insert(&new_rule("c", "d"), 1).unwrap().id;
        assert!(s.update(id, "a", "b", false, 2).is_err());
    }

    #[test]
    fn delete_removes() {
        let s = store();
        let id = s.insert(&new_rule("Calvin", "Kelvin"), 10).unwrap().id;
        s.delete(id).unwrap();
        assert!(s.all().unwrap().is_empty());
        s.delete(id).unwrap(); // deleting a missing rule is not an error
    }

    #[test]
    fn open_on_disk_persists_and_reopens() {
        let dir = std::env::temp_dir().join(format!("handy-learning-{}", std::process::id()));
        let path = dir.join("nested").join("learning.db");
        {
            let s = RuleStore::open(&path).unwrap();
            s.insert(&new_rule("Calvin", "Kelvin"), 10).unwrap();
        }
        let s = RuleStore::open(&path).unwrap();
        assert_eq!(s.all().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
