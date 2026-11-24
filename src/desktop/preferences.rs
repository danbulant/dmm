use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};
use dmm::games::GameKind;
use dmm::sources::{SearchQuery, Source};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Preferences {
    pub game: GameKind,
    pub enable_new_mods: bool,
    pub notify_downloads_complete: bool,
    pub browse: Vec<BrowsePreferences>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct BrowsePreferences {
    pub game_kind: GameKind,
    pub source: Source,
    pub provider_game: String,
    pub query: SearchQuery,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            game: GameKind::default(),
            enable_new_mods: false,
            notify_downloads_complete: false,
            browse: Vec::new(),
        }
    }
}

impl Preferences {
    pub fn load(dir: &Path) -> Result<Self> {
        let file = match File::open(dir.join("desktop.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error).context("reading desktop preferences"),
        };
        serde_json::from_reader(file).context("decoding desktop preferences")
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        let mut file = NamedTempFile::new_in(dir)?;
        serde_json::to_writer(file.as_file_mut(), self)?;
        file.persist(dir.join("desktop.json"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_selected_game_and_enable_preference() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let initial = Preferences::load(dir.path())?;
        assert_eq!(initial.game, GameKind::Cyberpunk);
        assert!(!initial.enable_new_mods);
        Preferences {
            game: GameKind::Zzmi,
            enable_new_mods: true,
            ..initial
        }
        .save(dir.path())?;
        let restored = Preferences::load(dir.path())?;
        assert_eq!(restored.game, GameKind::Zzmi);
        assert!(restored.enable_new_mods);
        Ok(())
    }
}
