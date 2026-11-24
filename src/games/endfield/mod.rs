use std::path::Path;

use anyhow::{Result, ensure};

use super::GameKind;
use super::xxmi::{SteamApp, is_importer_mods};

pub(super) const NAME: &str = "Arknights: Endfield";
pub(super) const GAMEBANANA_GAME: &str = "21842";
pub(super) const INSTALLATION_LABEL: &str = "EFMI Mods folder";
pub(super) const EXE: &str = "Endfield.exe";

pub(super) const STEAM_APP: SteamApp = SteamApp {
    kind: GameKind::Efmi,
    id: 4_732_690,
    importer: "EFMI",
    #[cfg(target_os = "linux")]
    executables: &[EXE],
};

pub(super) fn validate_installation_root(root: &Path) -> Result<()> {
    ensure!(
        is_importer_mods(root, GameKind::Efmi),
        "Choose the EFMI Mods folder from an installed XXMI importer."
    );
    Ok(())
}
