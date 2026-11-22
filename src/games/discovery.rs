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
    pub zzz_steam_roots: Vec<PathBuf>,
    #[cfg(target_os = "linux")]
    pub steam_xxmi: Vec<super::zzz::steam_xxmi::SteamXxmi>,
    #[cfg(target_os = "linux")]
    pub protontricks: Option<super::zzz::steam_xxmi::Protontricks>,
    pub notices: Vec<(GameKind, String)>,
}

pub const SUPPORTED: [(GameKind, &str); 2] = [
    (GameKind::Cyberpunk, GameKind::Cyberpunk.display_name()),
    (GameKind::Zzmi, GameKind::Zzmi.display_name()),
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
    match super::zzz::detect_game() {
        Ok(roots) => {
            found.zzz_steam_roots = roots
                .into_iter()
                .map(|root| root.canonicalize().unwrap_or(root))
                .collect()
        }
        Err(error) => found
            .notices
            .push((GameKind::Zzmi, format!("Steam: {error:#}"))),
    }
    #[cfg(target_os = "linux")]
    if !found.zzz_steam_roots.is_empty() {
        match super::zzz::steam_xxmi::discover() {
            Ok(targets) => found.steam_xxmi = targets,
            Err(error) => found
                .notices
                .push((GameKind::Zzmi, format!("Steam Proton: {error:#}"))),
        }
        if !found.steam_xxmi.is_empty() {
            found.protontricks = super::zzz::steam_xxmi::Protontricks::find();
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
