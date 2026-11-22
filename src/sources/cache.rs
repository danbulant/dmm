use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Serialize, de::DeserializeOwned};

const ITEMS: usize = 256;
const MAX_VALUE: usize = 2 * 1024 * 1024;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Cache {
    memory: RefCell<Memory>,
    disk: Option<Connection>,
}

#[derive(Default)]
struct Memory {
    values: HashMap<String, (Instant, Vec<u8>)>,
    bytes: usize,
}

impl Cache {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let disk = Connection::open(path)?;
        disk.busy_timeout(Duration::from_millis(250))?;
        disk.execute_batch(
            "PRAGMA auto_vacuum = FULL;
            CREATE TABLE IF NOT EXISTS responses(
                key TEXT PRIMARY KEY, expires INTEGER NOT NULL,
                touched INTEGER NOT NULL, body BLOB NOT NULL
            ) STRICT;",
        )?;
        Ok(Self {
            disk: Some(disk),
            ..Self::default()
        })
    }

    pub(super) fn get<T: Serialize + DeserializeOwned>(
        &self,
        namespace: &str,
        input: &impl Serialize,
        ttl: Duration,
        persistent: bool,
        load: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let mut key = format!("dmm-metadata-v1/{namespace}/");
        let hash = blake3::hash(&serde_json::to_vec(input)?);
        write!(key, "{hash}")?;
        let start = Instant::now();
        if let Some((until, bytes)) = self.memory.borrow().values.get(&key)
            && *until > start
            && let Ok(value) = serde_json::from_slice(bytes)
        {
            return Ok(value);
        }
        let now = timestamp();
        if persistent && let Some(db) = &self.disk
            && let Ok((until, bytes)) = db.query_row(
                "SELECT expires, body FROM responses WHERE key = ?1 AND expires > ?2 AND length(body) <= ?3",
                params![key, now, MAX_VALUE as i64], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            ) && let Ok(value) = serde_json::from_slice(&bytes) {
            let _ = db.execute("UPDATE responses SET touched=?2 WHERE key=?1", params![key, now]);
            self.remember(key, start + Duration::from_secs((until - now) as u64).min(ttl), bytes);
            return Ok(value);
        }
        let value = load()?;
        if let Ok(bytes) = serde_json::to_vec(&value)
            && bytes.len() <= MAX_VALUE
        {
            if persistent && let Some(db) = &self.disk {
                let now = timestamp();
                if let Ok(tx) = db.unchecked_transaction() {
                    let stored = tx.execute(
                        "INSERT OR REPLACE INTO responses VALUES (?1, ?2, ?3, ?4)",
                        params![key, now + ttl.as_secs() as i64, now, bytes],
                    );
                    if stored.is_ok() && tx.execute("DELETE FROM responses WHERE expires <= ?1 OR key IN (
                        SELECT key FROM (
                            SELECT key, row_number() OVER (ORDER BY touched DESC, rowid DESC) AS rank,
                                sum(length(body)) OVER (ORDER BY touched DESC, rowid DESC) AS bytes FROM responses
                        ) WHERE rank > ?2 OR bytes > ?3
                    )", params![now, ITEMS as i64, MAX_BYTES as i64]).is_ok() {
                        let _ = tx.commit();
                    }
                }
            }
            self.remember(key, Instant::now() + ttl, bytes);
        }
        Ok(value)
    }

    fn remember(&self, key: String, until: Instant, value: Vec<u8>) {
        let mut memory = self.memory.borrow_mut();
        if let Some((_, old)) = memory.values.remove(&key) {
            memory.bytes -= old.len();
        }
        if memory.values.len() == ITEMS || memory.bytes + value.len() > MAX_BYTES {
            memory.values.clear();
            memory.bytes = 0;
        }
        memory.bytes += value.len();
        memory.values.insert(key, (until, value));
    }
}

fn timestamp() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub(super) fn persistent(path: &Path) -> Cache {
    if let Some(dir) = path.parent()
        && fs::create_dir_all(dir).is_ok()
    {
        Cache::open(path).unwrap_or_default()
    } else {
        Cache::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisting_cache_reuses_results_but_not_uncached_failures_or_other_queries() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("cache.sqlite3");
        let ttl = Duration::from_secs(60);
        let cache = Cache::open(&path)?;
        assert!(
            cache
                .get::<String>("search", &"safe", ttl, true, || anyhow::bail!(
                    "unavailable"
                ))
                .is_err()
        );
        assert_eq!(
            cache.get(
                "search",
                &"safe",
                ttl,
                true,
                || Ok("safe result".to_owned())
            )?,
            "safe result"
        );
        assert_eq!(
            cache.get("search", &"adult", ttl, true, || Ok(
                "adult result".to_owned()
            ))?,
            "adult result"
        );
        drop(cache);
        let cache = Cache::open(&path)?;
        assert_eq!(
            cache.get::<String>("search", &"safe", ttl, true, || anyhow::bail!(
                "should have hit disk"
            ))?,
            "safe result"
        );
        cache
            .disk
            .as_ref()
            .unwrap()
            .execute("UPDATE responses SET expires=0", [])?;
        drop(cache);
        let cache = Cache::open(&path)?;
        assert_eq!(
            cache.get("search", &"safe", ttl, true, || Ok(
                "refreshed result".to_owned()
            ))?,
            "refreshed result"
        );
        Ok(())
    }
}
