use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use dmm::library::Library;
use dmm::sources::RemoteMod;
use serde::{Deserialize, Serialize};
use tempfile::Builder;

#[derive(Serialize, Deserialize)]
pub(super) struct Bookmark {
    pub item: RemoteMod,
    pub name: String,
    pub image: Option<String>,
}

pub(super) struct Recovery {
    dir: PathBuf,
    _lock: File,
}

impl Recovery {
    pub(super) fn open(state: &Path) -> Result<Self> {
        let dir = state.join("download-recovery");
        fs::create_dir_all(&dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))?;
        lock.try_lock()
            .context("Another DMM window owns the download queue")?;
        Ok(Self { dir, _lock: lock })
    }

    pub(super) fn record(&self, bookmark: &Bookmark) -> Result<PathBuf> {
        ensure!(!bookmark.name.trim().is_empty(), "mod has no name");
        let mut file = Builder::new().prefix("pending-").tempfile_in(&self.dir)?;
        serde_json::to_writer(&mut file, bookmark)?;
        file.as_file().sync_all()?;
        let path = file.path().with_extension("json");
        file.persist_noclobber(&path)?;
        Ok(path)
    }

    pub(super) fn clean_downloads(&self, state: &Path) -> Result<()> {
        match fs::remove_dir_all(state.join("download-scratch")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("removing interrupted download files"),
        }
    }

    pub(super) fn interrupted(&self) -> Result<Vec<PathBuf>> {
        fs::read_dir(&self.dir)?
            .filter_map(|entry| match entry {
                Ok(entry)
                    if entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "json") =>
                {
                    Some(Ok(entry.path()))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error.into())),
            })
            .collect()
    }
}

pub(super) fn discard(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("removing download recovery record"),
    }
}

pub(super) fn save(library: &Library, paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("reading download recovery record"),
        };
        let bookmark: Bookmark = serde_json::from_reader(file)?;
        library.save_favorite(&bookmark.item, &bookmark.name, bookmark.image.as_deref())?;
        discard(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmm::sources::Source;

    #[test]
    fn a_restart_recovers_only_unfinished_mods_as_named_bookmarks() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let bookmark = |id| Bookmark {
            item: RemoteMod {
                source: Source::GameBanana,
                game: "19567".into(),
                id,
            },
            name: format!("Mod {id}"),
            image: Some("https://images.gamebanana.com/preview.webp".into()),
        };
        let scratch = dir.path().join("download-scratch");
        let unrelated = dir.path().join("scratch-cli");
        fs::create_dir_all(&scratch)?;
        fs::create_dir_all(&unrelated)?;
        fs::write(scratch.join("partial"), b"interrupted archive")?;
        {
            let recovery = Recovery::open(dir.path())?;
            recovery.record(&bookmark(1))?;
            discard(&recovery.record(&bookmark(2))?)?;
        }
        let recovery = Recovery::open(dir.path())?;
        recovery.clean_downloads(dir.path())?;
        assert!(!scratch.exists());
        assert!(unrelated.is_dir());
        let library = Library::open(dir.path())?;
        save(&library, &recovery.interrupted()?)?;
        let favorites = library.saved_favorites()?;
        assert_eq!(favorites.len(), 1);
        assert_eq!(favorites[0].item, bookmark(1).item);
        assert_eq!(favorites[0].name, "Mod 1");
        assert_eq!(favorites[0].image, bookmark(1).image);
        assert!(recovery.interrupted()?.is_empty());
        Ok(())
    }
}
