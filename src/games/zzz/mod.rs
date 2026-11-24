use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

use super::GameKind;
use super::xxmi::{SteamApp, is_importer_mods};

pub(super) const NAME: &str = "Zenless Zone Zero";
pub(super) const NEXUS_GAME: &str = "zenlesszonezero";
pub(super) const GAMEBANANA_GAME: &str = "19567";
pub(super) const INSTALLATION_LABEL: &str = "ZZMI Mods folder";
#[cfg(all(feature = "rune-ui", target_os = "linux"))]
pub(super) const PROCESS_NAME: &str = "ZenlessZoneZero";
#[cfg(all(feature = "rune-ui", target_os = "linux"))]
pub(super) const PROCESS_IMAGES: &[&str] = &["ZenlessZoneZero.exe", "ZenlessZoneZeroBeta.exe"];

pub(super) const STEAM_APP: SteamApp = SteamApp {
    kind: GameKind::Zzmi,
    id: 4_162_040,
    importer: "ZZMI",
    #[cfg(target_os = "linux")]
    executables: &[
        "ZenlessZoneZero.exe",
        "games/ZenlessZoneZero Game/ZenlessZoneZero.exe",
    ],
};

pub(super) fn validate_installation_root(root: &Path, steam_roots: &[PathBuf]) -> Result<()> {
    if steam_roots.iter().any(|game| root.starts_with(game)) {
        ensure!(
            is_importer_mods(root, GameKind::Zzmi),
            "Choose the ZZMI Mods folder, not the Steam game installation."
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn steam_play_option(executable: &str) -> String {
    let quoted = executable.replace('\'', "'\\''");
    format!(
        r#"bash -c 'launcher=$1; shift; args=("$@"); for i in "${{!args[@]}}"; do if [[ ${{args[i]}} == */HYP.exe ]]; then args[i]=$launcher; exec "${{args[@]}}"; fi; done; echo "HYP.exe not found in Steam launch command" >&2; exit 1' _ '{quoted}' %command%"#
    )
}

#[cfg(target_os = "linux")]
pub(super) const STEAM_PLAY_INSTRUCTIONS: [&str; 2] = [
    "To open XXMI from Steam, save your current launch options, then paste this command into Steam → Library → Zenless Zone Zero → Properties → General → Launch Options.",
    "Steam’s Play button will open XXMI in this game’s Proton session instead of HoYoPlay. In XXMI, set Game Launch to Native, then click Start. Restore your previous Steam launch options to open HoYoPlay again.",
];
