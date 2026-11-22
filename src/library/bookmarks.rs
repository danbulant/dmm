use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::{RemoteMod, SavedFavorite, SavedSearch, validate_slug};

pub(super) fn unsave(db: &Connection, item: &RemoteMod) -> Result<()> {
    db.execute(
        "DELETE FROM favorites WHERE data = ?1",
        [serde_json::to_string(item)?],
    )?;
    Ok(())
}

pub(super) fn save_favorite(
    db: &Connection,
    item: &RemoteMod,
    name: &str,
    image: Option<&str>,
) -> Result<()> {
    let id = serde_json::to_string(item)?;
    ensure!(!name.trim().is_empty(), "saved mod name cannot be empty");
    db.execute(
        "INSERT INTO favorites VALUES (?1, ?2, ?3)
         ON CONFLICT(data) DO UPDATE SET name=excluded.name, image=COALESCE(excluded.image, favorites.image)",
        params![id, name, image],
    )?;
    Ok(())
}

pub(super) fn saved_favorites(db: &Connection) -> Result<Vec<SavedFavorite>> {
    let mut statement = db.prepare("SELECT data, name, image FROM favorites ORDER BY data")?;
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
        })?
        .map(|row| {
            let (id, name, image) = row?;
            Ok(SavedFavorite {
                item: serde_json::from_str(&id)?,
                name,
                image,
            })
        })
        .collect()
}

pub(super) fn save_search(db: &Connection, name: &str, search: &SavedSearch) -> Result<()> {
    validate_slug(name)?;
    db.execute(
        "INSERT INTO searches VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET data=excluded.data",
        params![name, serde_json::to_string(search)?],
    )?;
    Ok(())
}

pub(super) fn saved_search(db: &Connection, name: &str) -> Result<SavedSearch> {
    db.query_row("SELECT data FROM searches WHERE name = ?1", [name], |r| {
        r.get::<_, String>(0)
    })
    .optional()?
    .map(|row| serde_json::from_str(&row))
    .transpose()?
    .with_context(|| format!("unknown saved search {name}"))
}
