use anyhow::{Result, ensure};
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, params};
use std::{collections::HashMap, path::Path, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Bucket {
    pub date: NaiveDate,
    pub hour: u32,
    pub app: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    pub ms: u64,
    pub switches: u64,
}

impl Count {
    pub fn merge(&mut self, other: Self) {
        self.ms += other.ms;
        self.switches += other.switches;
    }
}

pub type Totals = HashMap<Bucket, Count>;

pub struct Store(Connection);

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::setup(Connection::open(path)?)
    }
    pub fn memory() -> Result<Self> {
        Self::setup(Connection::open_in_memory()?)
    }

    fn setup(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_millis(400))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            version <= 1,
            "this database was created by a newer Irminsul version"
        );
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        if version == 0 {
            conn.execute_batch("BEGIN;
                CREATE TABLE IF NOT EXISTS usage (
                    day TEXT NOT NULL,
                    hour INTEGER NOT NULL CHECK(hour BETWEEN 0 AND 23),
                    app TEXT NOT NULL,
                    ms INTEGER NOT NULL CHECK(ms >= 0),
                    switches INTEGER NOT NULL CHECK(switches >= 0),
                    PRIMARY KEY(day, hour, app)
                ) WITHOUT ROWID;
                CREATE TABLE IF NOT EXISTS preferences (name TEXT PRIMARY KEY, value INTEGER NOT NULL) WITHOUT ROWID;
                PRAGMA user_version = 1;
                COMMIT;")?;
        }
        Ok(Self(conn))
    }

    pub fn save(&mut self, changes: &Totals, keep_from: Option<NaiveDate>) -> Result<()> {
        if changes.is_empty() && keep_from.is_none() {
            return Ok(());
        }
        let tx = self.0.transaction()?;
        if !changes.is_empty() {
            let mut upsert = tx.prepare_cached("INSERT INTO usage(day, hour, app, ms, switches) VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(day,hour,app) DO UPDATE SET ms=ms+excluded.ms, switches=switches+excluded.switches")?;
            for (key, val) in changes {
                if val.ms != 0 || val.switches != 0 {
                    upsert.execute(params![
                        key.date.to_string(),
                        key.hour,
                        key.app,
                        val.ms,
                        val.switches
                    ])?;
                }
            }
        }
        if let Some(cutoff) = keep_from {
            tx.execute("DELETE FROM usage WHERE day < ?1", [cutoff.to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn range(&self, start: NaiveDate, end: NaiveDate) -> Result<Totals> {
        let mut q = self.0.prepare_cached(
            "SELECT day,hour,app,ms,switches FROM usage WHERE day BETWEEN ?1 AND ?2",
        )?;
        let mut rows = q.query(params![start.to_string(), end.to_string()])?;
        let mut totals = Totals::new();
        while let Some(r) = rows.next()? {
            let day: String = r.get(0)?;
            totals.insert(
                Bucket {
                    date: day.parse()?,
                    hour: r.get(1)?,
                    app: r.get(2)?,
                },
                Count {
                    ms: r.get(3)?,
                    switches: r.get(4)?,
                },
            );
        }
        Ok(totals)
    }

    pub fn preference(&self, key: &str) -> Result<Option<u16>> {
        Ok(self
            .0
            .query_row("SELECT value FROM preferences WHERE name=?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_preference(&self, key: &str, value: u16) -> Result<()> {
        self.0.execute("INSERT INTO preferences(name,value) VALUES (?1,?2) ON CONFLICT(name) DO UPDATE SET value=excluded.value", params![key,value])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upserts_and_prunes_at_calendar_cutoff() {
        let mut db = Store::memory().unwrap();
        let a = "2026-11-17".parse().unwrap();
        let b = "2026-11-18".parse().unwrap();
        let key = |date| Bucket {
            date,
            hour: 10,
            app: "code".into(),
        };
        let changes = Totals::from([
            (
                key(a),
                Count {
                    ms: 20,
                    switches: 1,
                },
            ),
            (
                key(b),
                Count {
                    ms: 50,
                    switches: 2,
                },
            ),
        ]);
        db.save(&changes, None).unwrap();
        db.save(&changes, Some(b)).unwrap();
        assert_eq!(
            db.range(a, b).unwrap(),
            Totals::from([(
                key(b),
                Count {
                    ms: 100,
                    switches: 4
                }
            )])
        );
        db.set_preference("paused", 1).unwrap();
        assert_eq!(db.preference("paused").unwrap(), Some(1));
    }
}
