use std::path::PathBuf;

use anyhow::{Context, Result};

use super::GameKind;

#[derive(Clone, Debug)]
pub struct DetectedGame {
    pub kind: GameKind,
    pub root: PathBuf,
}

#[derive(Default)]
pub struct Discovery {
    pub games: Vec<DetectedGame>,
    pub steam_games: Vec<DetectedGame>,
    #[cfg(target_os = "linux")]
    pub steam_xxmi: Vec<super::xxmi::steam_xxmi::SteamXxmi>,
    #[cfg(target_os = "linux")]
    pub protontricks: Option<super::xxmi::steam_xxmi::Protontricks>,
    pub notices: Vec<(GameKind, String)>,
}

pub const SUPPORTED: [(GameKind, &str); 3] = [
    (GameKind::Cyberpunk, GameKind::Cyberpunk.display_name()),
    (GameKind::Zzmi, GameKind::Zzmi.display_name()),
    (GameKind::Efmi, GameKind::Efmi.display_name()),
];

pub fn discover() -> Discovery {
    let mut found = Discovery::default();
    for (kind, label) in SUPPORTED {
        match super::detect(kind) {
            Ok(roots) => found
                .games
                .extend(roots.into_iter().map(|root| DetectedGame {
                    kind,
                    root: root.canonicalize().unwrap_or(root),
                })),
            Err(error) => found.notices.push((kind, format!("{label}: {error:#}"))),
        }
    }
    for kind in [GameKind::Zzmi, GameKind::Efmi] {
        match super::xxmi::detect_game(kind) {
            Ok(roots) => found
                .steam_games
                .extend(roots.into_iter().map(|root| DetectedGame { kind, root })),
            Err(error) => found.notices.push((kind, format!("Steam: {error:#}"))),
        }
    }
    #[cfg(target_os = "linux")]
    if !found.steam_games.is_empty() {
        match super::xxmi::steam_xxmi::discover() {
            Ok(targets) => found.steam_xxmi = targets,
            Err(error) => {
                for kind in [GameKind::Zzmi, GameKind::Efmi] {
                    found.notices.push((kind, format!("Steam Proton: {error:#}")));
                }
            }
        }
        if !found.steam_xxmi.is_empty() {
            found.protontricks = super::xxmi::steam_xxmi::Protontricks::find();
        }
    }
    found
}

pub(super) fn detect_steam_app(app_id: u32) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for steam in steamlocate::locate_all().context("could not locate Steam")? {
        for library in steam
            .libraries()
            .context("could not inspect Steam libraries")?
            .filter_map(std::result::Result::ok)
        {
            if let Some(app) = library
                .app(app_id)
                .transpose()
                .context("could not inspect Steam applications")?
            {
                let root = library
                    .path()
                    .join("steamapps/common")
                    .join(app.install_dir);
                if root.is_dir() {
                    roots.push(root.canonicalize().unwrap_or(root));
                }
            }
        }
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}
