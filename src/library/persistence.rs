use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

pub(super) fn open(state: &Path) -> Result<Connection> {
    let db = Connection::open(state.join("library.sqlite3"))?;
    db.execute_batch(include_str!("../schema.sql"))?;
    migrate_favorites(&db)?;
    Ok(db)
}

fn migrate_favorites(db: &Connection) -> Result<()> {
    let has_name = {
        let mut statement = db.prepare("PRAGMA table_info(favorites)")?;
        statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|column| column == "name")
    };
    if has_name {
        return Ok(());
    }
    let tx = db.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS favorite_metadata (id TEXT PRIMARY KEY NOT NULL, name TEXT, image TEXT) STRICT;
         CREATE TABLE favorites_new (
             data TEXT PRIMARY KEY NOT NULL,
             name TEXT NOT NULL,
             image TEXT
         ) STRICT;
         INSERT INTO favorites_new(data, name, image)
             SELECT favorites.data, favorite_metadata.name, favorite_metadata.image
             FROM favorites JOIN favorite_metadata ON favorites.data = favorite_metadata.id
             WHERE favorite_metadata.name IS NOT NULL AND trim(favorite_metadata.name) != '';
         DROP TABLE favorite_metadata;
         DROP TABLE favorites;
         ALTER TABLE favorites_new RENAME TO favorites;",
    )?;
    tx.commit()?;
    Ok(())
}
