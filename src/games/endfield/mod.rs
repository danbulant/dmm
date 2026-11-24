use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

#[cfg(feature = "rune-ui")]
use super::GamePresentation;
use super::xxmi::{SteamApp, XxmiAdapter, is_importer_mods};
use super::{GameKind, GameProfile};
use crate::sources::Source;

pub(super) const PROFILE: GameProfile = GameProfile {
    id: "efmi",
    aliases: &["endfield"],
    adapter: &XxmiAdapter::new(STEAM_APP, validate_installation_root),
    name: "Arknights: Endfield",
    nexus: None,
    gamebanana: Some("21842"),
    preferred_source: Source::GameBanana,
    installation_label: "EFMI Mods folder",
    #[cfg(feature = "rune-ui")]
    presentation: GamePresentation {
        icon: include_bytes!("icon.jpg"),
        setup_note: None,
        destination_override: false,
    },
};
pub(super) const EXE: &str = "Endfield.exe";

pub(super) const STEAM_APP: SteamApp = SteamApp {
    kind: GameKind::Efmi,
    id: 4_732_690,
    importer: "EFMI",
    #[cfg(target_os = "linux")]
    executables: &[EXE],
};

fn validate_installation_root(root: &Path, _steam_roots: &[PathBuf]) -> Result<()> {
    ensure!(
        is_importer_mods(root, GameKind::Efmi),
        "Choose the EFMI Mods folder from an installed XXMI importer."
    );
    Ok(())
}
