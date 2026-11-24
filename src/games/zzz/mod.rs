use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

#[cfg(feature = "rune-ui")]
use super::GamePresentation;
use super::xxmi::{SteamApp, XxmiAdapter, is_importer_mods};
use super::{GameKind, GameProfile};
use crate::sources::Source;

pub(super) const PROFILE: GameProfile = GameProfile {
    id: "zzmi",
    aliases: &[],
    adapter: &XxmiAdapter::new(STEAM_APP, validate_installation_root),
    name: "Zenless Zone Zero",
    nexus: Some("zenlesszonezero"),
    gamebanana: Some("19567"),
    preferred_source: Source::GameBanana,
    installation_label: "ZZMI Mods folder",
    #[cfg(feature = "rune-ui")]
    presentation: GamePresentation {
        icon: include_bytes!("icon.png"),
        setup_note: None,
        destination_override: false,
    },
};
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

fn validate_installation_root(root: &Path, steam_roots: &[PathBuf]) -> Result<()> {
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
