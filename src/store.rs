//! SQLite persistence. One file, WAL mode, short synchronous queries.

use std::{path::Path, sync::Mutex};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// A valid answer, scored against the school.
    Counted,
    /// Jev judged it invalid: doesn't fit, just the category, or more than one thing.
    Rejected,
    /// Nothing (or nothing in time).
    Timeout,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Counted => "counted",
            Outcome::Rejected => "rejected",
            Outcome::Timeout => "timeout",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "counted" => Some(Outcome::Counted),
            "rejected" => Some(Outcome::Rejected),
            "timeout" => Some(Outcome::Timeout),
            _ => None,
        }
    }
}

/// One prompt in one player's dive. `outcome` is `None` while the clock is running.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub idx: usize,
    pub revealed_at: i64,
    pub outcome: Option<Outcome>,
    pub answer: Option<String>,
    pub canonical: Option<String>,
    pub reason: Option<String>,
    pub points: i64,
    pub share: Option<f64>,
}

/// A group of answers the school counts as the same thing.
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    pub canonical: String,
    /// How the first diver to say it wrote it.
    pub label: String,
    pub count: i64,
    /// Jev's estimate of how common it is, used until the school is big enough.
    pub prior: f64,
}

/// Cached judgement for a normalized answer, so repeat answers skip the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Counted { canonical: String, prior: f64 },
    Rejected { reason: String },
}

pub struct Store {
    conn: Mutex<Connection>,
}

type Result<T> = rusqlite::Result<T>;

impl Store {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS days (
                day     TEXT PRIMARY KEY,
                prompts TEXT NOT NULL            -- JSON array, frozen when first dealt
            );
            CREATE TABLE IF NOT EXISTS turns (
                day         TEXT    NOT NULL,
                player      TEXT    NOT NULL,
                idx         INTEGER NOT NULL,
                revealed_at INTEGER NOT NULL,    -- ms since epoch
                outcome     TEXT,                -- NULL while the clock runs
                answer      TEXT,
                canonical   TEXT,
                reason      TEXT,
                points      INTEGER NOT NULL DEFAULT 0,
                share       REAL,
                settled_at  INTEGER,
                PRIMARY KEY (day, player, idx)
            );
            CREATE TABLE IF NOT EXISTS school (
                day       TEXT    NOT NULL,
                idx       INTEGER NOT NULL,
                canonical TEXT    NOT NULL,
                label     TEXT    NOT NULL,
                count     INTEGER NOT NULL,
                prior     REAL    NOT NULL,
                PRIMARY KEY (day, idx, canonical)
            );
            CREATE TABLE IF NOT EXISTS verdicts (
                day       TEXT    NOT NULL,
                idx       INTEGER NOT NULL,
                key       TEXT    NOT NULL,
                canonical TEXT,                  -- NULL when rejected
                prior     REAL,
                reason    TEXT,
                PRIMARY KEY (day, idx, key)
            );
            ",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The day's prompts, dealing them with `deal` the first time the day is seen.
    pub fn day_prompts(
        &self,
        day: &str,
        deal: impl FnOnce() -> Vec<String>,
    ) -> Result<Vec<String>> {
        let conn = self.conn();
        let existing: Option<String> = conn
            .query_row("SELECT prompts FROM days WHERE day = ?1", [day], |r| {
                r.get(0)
            })
            .optional()?;
        let json = match existing {
            Some(json) => json,
            None => {
                let json = serde_json::to_string(&deal()).expect("prompts serialize");
                conn.execute(
                    "INSERT OR IGNORE INTO days (day, prompts) VALUES (?1, ?2)",
                    params![day, json],
                )?;
                conn.query_row("SELECT prompts FROM days WHERE day = ?1", [day], |r| {
                    r.get(0)
                })?
            }
        };
        Ok(serde_json::from_str(&json).unwrap_or_default())
    }

    pub fn known_day(&self, day: &str) -> Result<Option<Vec<String>>> {
        let json: Option<String> = self
            .conn()
            .query_row("SELECT prompts FROM days WHERE day = ?1", [day], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(json.map(|j| serde_json::from_str(&j).unwrap_or_default()))
    }

    pub fn turns(&self, day: &str, player: &str) -> Result<Vec<Turn>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT idx, revealed_at, outcome, answer, canonical, reason, points, share
             FROM turns WHERE day = ?1 AND player = ?2 ORDER BY idx",
        )?;
        let rows = stmt.query_map(params![day, player], |r| {
            Ok(Turn {
                idx: r.get::<_, i64>(0)? as usize,
                revealed_at: r.get(1)?,
                outcome: r
                    .get::<_, Option<String>>(2)?
                    .as_deref()
                    .and_then(Outcome::parse),
                answer: r.get(3)?,
                canonical: r.get(4)?,
                reason: r.get(5)?,
                points: r.get(6)?,
                share: r.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// Starts the clock on a prompt. Does nothing if it was already revealed.
    pub fn open_turn(&self, day: &str, player: &str, idx: usize, now: i64) -> Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO turns (day, player, idx, revealed_at) VALUES (?1, ?2, ?3, ?4)",
            params![day, player, idx as i64, now],
        )?;
        Ok(())
    }

    /// Records a result, once. Returns false if the turn was already settled, in which
    /// case nothing (including the school) changes. A counted answer joins the school in
    /// the same transaction.
    pub fn settle_turn(
        &self,
        day: &str,
        player: &str,
        turn: &Turn,
        join_school: Option<&Cluster>,
        now: i64,
    ) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let updated = tx.execute(
            "UPDATE turns
             SET outcome = ?4, answer = ?5, canonical = ?6, reason = ?7, points = ?8, share = ?9, settled_at = ?10
             WHERE day = ?1 AND player = ?2 AND idx = ?3 AND outcome IS NULL",
            params![
                day,
                player,
                turn.idx as i64,
                turn.outcome.map(Outcome::as_str),
                turn.answer,
                turn.canonical,
                turn.reason,
                turn.points,
                turn.share,
                now
            ],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        if let Some(cluster) = join_school {
            tx.execute(
                "INSERT INTO school (day, idx, canonical, label, count, prior)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5)
                 ON CONFLICT (day, idx, canonical) DO UPDATE SET count = count + 1",
                params![
                    day,
                    turn.idx as i64,
                    cluster.canonical,
                    cluster.label,
                    cluster.prior
                ],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Answer groups for a prompt, most common first.
    pub fn school(&self, day: &str, idx: usize) -> Result<Vec<Cluster>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT canonical, label, count, prior FROM school
             WHERE day = ?1 AND idx = ?2 ORDER BY count DESC, canonical",
        )?;
        let rows = stmt.query_map(params![day, idx as i64], |r| {
            Ok(Cluster {
                canonical: r.get(0)?,
                label: r.get(1)?,
                count: r.get(2)?,
                prior: r.get(3)?,
            })
        })?;
        rows.collect()
    }

    pub fn verdict(&self, day: &str, idx: usize, key: &str) -> Result<Option<Verdict>> {
        self.conn()
            .query_row(
                "SELECT canonical, prior, reason FROM verdicts WHERE day = ?1 AND idx = ?2 AND key = ?3",
                params![day, idx as i64, key],
                |r| {
                    let canonical: Option<String> = r.get(0)?;
                    let prior: Option<f64> = r.get(1)?;
                    let reason: Option<String> = r.get(2)?;
                    Ok(match canonical {
                        Some(canonical) => Verdict::Counted { canonical, prior: prior.unwrap_or(0.04) },
                        None => Verdict::Rejected { reason: reason.unwrap_or_default() },
                    })
                },
            )
            .optional()
    }

    pub fn put_verdict(&self, day: &str, idx: usize, key: &str, verdict: &Verdict) -> Result<()> {
        let (canonical, prior, reason) = match verdict {
            Verdict::Counted { canonical, prior } => (Some(canonical.as_str()), Some(*prior), None),
            Verdict::Rejected { reason } => (None, None, Some(reason.as_str())),
        };
        self.conn().execute(
            "INSERT OR REPLACE INTO verdicts (day, idx, key, canonical, prior, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![day, idx as i64, key, canonical, prior, reason],
        )?;
        Ok(())
    }

    /// (finished dives today, how many of them scored fewer points than `points`).
    pub fn rank(&self, day: &str, points: i64, prompts_per_day: usize) -> Result<(i64, i64)> {
        self.conn().query_row(
            "SELECT COUNT(*), COALESCE(SUM(total < ?2), 0) FROM (
                SELECT SUM(points) AS total FROM turns
                WHERE day = ?1 AND outcome IS NOT NULL
                GROUP BY player HAVING COUNT(*) = ?3
            )",
            params![day, points, prompts_per_day as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled(idx: usize, points: i64) -> Turn {
        Turn {
            idx,
            revealed_at: 0,
            outcome: Some(Outcome::Counted),
            answer: Some("Apple".into()),
            canonical: Some("apple".into()),
            reason: None,
            points,
            share: Some(0.2),
        }
    }

    fn apple() -> Cluster {
        Cluster {
            canonical: "apple".into(),
            label: "Apple".into(),
            count: 1,
            prior: 0.2,
        }
    }

    #[test]
    fn days_are_dealt_once() {
        let store = Store::open_in_memory().unwrap();
        let first = store.day_prompts("d", || vec!["a".into()]).unwrap();
        let second = store.day_prompts("d", || vec!["b".into()]).unwrap();
        assert_eq!(first, vec!["a"]);
        assert_eq!(second, vec!["a"]);
    }

    #[test]
    fn a_turn_settles_once_and_joins_the_school_once() {
        let store = Store::open_in_memory().unwrap();
        store.open_turn("d", "p", 0, 1).unwrap();
        let cluster = apple();
        assert!(
            store
                .settle_turn("d", "p", &settled(0, 3), Some(&cluster), 2)
                .unwrap()
        );
        assert!(
            !store
                .settle_turn("d", "p", &settled(0, 9), Some(&cluster), 3)
                .unwrap()
        );

        let turns = store.turns("d", "p").unwrap();
        assert_eq!(turns[0].points, 3);
        assert_eq!(store.school("d", 0).unwrap()[0].count, 1);
    }

    #[test]
    fn rank_counts_only_finished_dives() {
        let store = Store::open_in_memory().unwrap();
        for (player, points) in [("a", 1), ("b", 5), ("c", 9)] {
            for idx in 0..2 {
                store.open_turn("d", player, idx, 0).unwrap();
                store
                    .settle_turn("d", player, &settled(idx, points), None, 0)
                    .unwrap();
            }
        }
        store.open_turn("d", "unfinished", 0, 0).unwrap();
        // Totals: a=2, b=10, c=18.
        assert_eq!(store.rank("d", 11, 2).unwrap(), (3, 2));
    }

    #[test]
    fn verdicts_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let counted = Verdict::Counted {
            canonical: "apple".into(),
            prior: 0.2,
        };
        let rejected = Verdict::Rejected {
            reason: "nope".into(),
        };
        store.put_verdict("d", 0, "apple", &counted).unwrap();
        store.put_verdict("d", 0, "sofa", &rejected).unwrap();
        assert_eq!(store.verdict("d", 0, "apple").unwrap(), Some(counted));
        assert_eq!(store.verdict("d", 0, "sofa").unwrap(), Some(rejected));
        assert_eq!(store.verdict("d", 0, "pear").unwrap(), None);
    }
}
