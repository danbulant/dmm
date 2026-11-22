use std::io::Cursor;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use image::{ExtendedColorType, ImageEncoder, RgbaImage, codecs::webp::WebPEncoder};
use rusqlite::{Connection, params};

const MAX_BODY: i64 = 512 * 1024;
const MAX_CACHE: i64 = 64 * 1024 * 1024;

pub(super) struct ThumbnailCache(Connection);

impl ThumbnailCache {
    pub(super) fn open(state: &Path) -> Result<Self> {
        std::fs::create_dir_all(state)?;
        let db = Connection::open(state.join("thumbnails.sqlite3"))?;
        db.busy_timeout(Duration::from_millis(400))?;
        db.execute_batch(
            "PRAGMA auto_vacuum=FULL;
             CREATE TABLE IF NOT EXISTS thumbnails (
                 key TEXT PRIMARY KEY, expires INTEGER NOT NULL,
                 touched INTEGER NOT NULL, body BLOB NOT NULL
             ) STRICT;",
        )?;
        Ok(Self(db))
    }

    pub(super) fn get(&self, url: &str, saved: bool) -> Option<(Vec<u8>, Duration)> {
        let key = blake3::hash(url.as_bytes()).to_hex();
        let now = timestamp();
        let (expires, bytes): (i64, Vec<u8>) = self
            .0
            .query_row(
                "SELECT expires,body FROM thumbnails
             WHERE key=?1 AND (expires>?2 OR ?3) AND length(body)<=?4",
                params![key.as_str(), now, saved, MAX_BODY],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()?;
        let _ = self.0.execute(
            "UPDATE thumbnails SET touched=?2 WHERE key=?1",
            params![key.as_str(), now],
        );
        let ttl = if saved {
            Duration::from_secs(60)
        } else {
            Duration::from_secs((expires - now) as u64)
        };
        Some((bytes, ttl))
    }

    pub(super) fn put(&self, url: &str, rgba: &RgbaImage, ttl: Duration) -> Result<()> {
        if ttl.is_zero() {
            return Ok(());
        }
        let mut bytes = Cursor::new(Vec::new());
        WebPEncoder::new_lossless(&mut bytes).write_image(
            rgba,
            rgba.width(),
            rgba.height(),
            ExtendedColorType::Rgba8,
        )?;
        let bytes = bytes.into_inner();
        if bytes.len() as i64 > MAX_BODY {
            return Ok(());
        }
        let key = blake3::hash(url.as_bytes()).to_hex();
        let now = timestamp();
        let tx = self.0.unchecked_transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO thumbnails VALUES (?1,?2,?3,?4)",
            params![key.as_str(), now + ttl.as_secs() as i64, now, bytes],
        )?;
        tx.execute("DELETE FROM thumbnails WHERE key IN (
                     SELECT key FROM (
                         SELECT key, sum(length(body)) OVER(ORDER BY touched DESC,rowid DESC) AS bytes
                         FROM thumbnails
                     ) WHERE bytes>?1
                   )", [MAX_CACHE])?;
        tx.commit()?;
        Ok(())
    }
}

fn timestamp() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_thumbnails_survive_cache_reopen_and_expiry() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let url = "https://images.gamebanana.com/mod.png";
        let pixel = image::Rgba([0, 1, 2, 255]);
        let rgba = RgbaImage::from_pixel(8, 4, pixel);
        {
            let cache = ThumbnailCache::open(dir.path())?;
            cache.put(url, &rgba, Duration::from_secs(60))?;
            assert!(cache.get(url, false).is_some());
            cache.0.execute("UPDATE thumbnails SET expires=0", [])?;
        }
        let cache = ThumbnailCache::open(dir.path())?;
        assert!(cache.get(url, false).is_none());
        let (bytes, _) = cache.get(url, true).expect("persisted thumbnail");
        assert_eq!(image::load_from_memory(&bytes)?.into_rgba8(), rgba);
        Ok(())
    }
}
