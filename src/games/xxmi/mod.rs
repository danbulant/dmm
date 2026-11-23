#[cfg(target_os = "linux")]
pub mod steam_xxmi;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use super::discovery::detect_steam_app;
use super::{GameKind, GameProfile, InstallPlan, LaunchCommand, plan_mod_folder};

#[derive(Clone, Copy)]
struct SteamApp {
    kind: GameKind,
    id: u32,
    importer: &'static str,
}

impl SteamApp {
    fn compatdata(self, library: &Path) -> PathBuf {
        library.join("steamapps/compatdata").join(self.id.to_string())
    }
}

const STEAM_APPS: [SteamApp; 2] = [
    SteamApp {
        kind: GameKind::Zzmi,
        id: 4_162_040,
        importer: "ZZMI",
    },
    SteamApp {
        kind: GameKind::Efmi,
        id: 4_732_690,
        importer: "EFMI",
    },
];

fn validate_destination(destination: Option<&Path>) -> Result<()> {
    ensure!(
        destination.is_none(),
        "XXMI mods must install directly under the configured Mods folder"
    );
    Ok(())
}

pub(super) fn plan(
    profile: &GameProfile,
    files: &[PathBuf],
    slug: &str,
    destination: Option<&Path>,
) -> Result<InstallPlan> {
    validate_destination(destination)?;
    plan_mod_folder(profile, files, slug, None, true)
}

pub(super) fn plan_bundle(
    profile: &GameProfile,
    packages: &[&[PathBuf]],
    slug: &str,
    destination: Option<&Path>,
) -> Result<Vec<InstallPlan>> {
    validate_destination(destination)?;
    if let Some(required_file) = profile
        .required_file
        .as_deref()
        .filter(|file| !file.is_empty())
    {
        let suffix = format!(".{}", required_file.trim_start_matches('.')).to_ascii_lowercase();
        ensure!(
            packages
                .iter()
                .flat_map(|package| package.iter())
                .any(|path| {
                    path.to_string_lossy()
                        .to_ascii_lowercase()
                        .ends_with(&suffix)
                }),
            "{} mods must contain at least one {suffix} file",
            profile.name
        );
    }
    packages
        .iter()
        .map(|files| plan_mod_folder(profile, files, slug, None, false))
        .collect()
}

pub fn mod_folder(slug: &str, enabled: bool) -> String {
    if enabled {
        slug.to_owned()
    } else {
        format!("DISABLED_{slug}")
    }
}

pub fn deployed_path(path: &str, enabled: bool) -> String {
    mod_folder(path, enabled)
}

pub fn reserved_slug(slug: &str) -> bool {
    slug.starts_with("disabled")
}

pub(super) fn detect(kind: GameKind) -> Result<Vec<PathBuf>> {
    let app = steam_app(kind);
    let steam_roots = detect_game(kind).unwrap_or_default();
    let mut candidates = standard_xxmi_roots()
        .into_iter()
        .flat_map(|base| [base.join(app.importer).join("Mods"), base.join("Mods")])
        .chain(steam_roots.into_iter().map(|root| root.join(app.importer).join("Mods")))
        .filter(|path| is_importer_mods(path, kind))
        .filter_map(|path| path.canonicalize().ok())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

pub fn validate_installation_root(
    kind: GameKind,
    root: &Path,
    steam_game_roots: &[PathBuf],
) -> Result<()> {
    match kind {
        GameKind::Efmi => ensure!(
            is_importer_mods(root, kind),
            "Choose the EFMI Mods folder from an installed XXMI importer."
        ),
        GameKind::Zzmi if steam_game_roots.iter().any(|game| root.starts_with(game)) => ensure!(
            is_importer_mods(root, kind),
            "Choose the ZZMI Mods folder, not the Steam game installation."
        ),
        GameKind::Cyberpunk | GameKind::Generic | GameKind::Zzmi => {}
    }
    Ok(())
}

pub(super) fn launch(
    profile: &GameProfile,
    root: &Path,
    xxmi: Option<&Path>,
    wine: Option<&Path>,
    wine_prefix: Option<&Path>,
) -> Result<LaunchCommand> {
    let args = profile
        .settings
        .launch
        .arguments
        .iter()
        .map(OsString::from)
        .collect();
    xxmi_command(Some(root), xxmi, wine, wine_prefix, args).map(|(command, _)| command)
}

fn steam_app(kind: GameKind) -> SteamApp {
    STEAM_APPS
        .into_iter()
        .find(|app| app.kind == kind)
        .expect("not an XXMI game")
}

pub(super) fn detect_game(kind: GameKind) -> Result<Vec<PathBuf>> {
    detect_steam_app(steam_app(kind).id)
}

fn is_importer_mods(path: &Path, kind: GameKind) -> bool {
    let Some(package) = path.parent() else {
        return false;
    };
    if !path.file_name().is_some_and(|name| name == "Mods") || !path.is_dir() {
        return false;
    }
    let Ok(contents) = std::fs::read_to_string(package.join("d3dx.ini")) else {
        return false;
    };
    let mut includes_mods = false;
    let mut targets_endfield = false;
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.split(';').next().unwrap_or_default().trim();
            includes_mods |= key.eq_ignore_ascii_case("include_recursive")
                && value.eq_ignore_ascii_case("Mods");
            targets_endfield |= key.eq_ignore_ascii_case("target")
                && value.eq_ignore_ascii_case("Endfield.exe");
        }
    }
    includes_mods && (targets_endfield == (kind == GameKind::Efmi))
}

fn standard_xxmi_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.extend([
            home.join("XXMI Launcher"),
            home.join(".local/share/XXMI Launcher"),
            home.join("Games/XXMI Launcher"),
        ]);
        if cfg!(target_os = "linux") {
            let prefix = std::env::var_os("WINEPREFIX")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".wine"));
            if let Ok(users) = std::fs::read_dir(prefix.join("drive_c/users")) {
                roots.extend(
                    users
                        .take(32)
                        .filter_map(std::result::Result::ok)
                        .map(|user| user.path().join("AppData/Roaming/XXMI Launcher")),
                );
            }
        }
        if let Ok(steam_dirs) = steamlocate::locate_all() {
            for steam in steam_dirs {
                if let Ok(libraries) = steam.libraries() {
                    for library in libraries.filter_map(std::result::Result::ok) {
                        for app in STEAM_APPS {
                            let compatdata = app.compatdata(library.path());
                            roots.push(compatdata.join("pfx/drive_c/users/steamuser/AppData/Roaming/XXMI Launcher"));
                            #[cfg(target_os = "linux")]
                            roots.push(compatdata.join(steam_xxmi::PORTABLE_DIR));
                        }
                    }
                }
            }
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join("XXMI Launcher"));
    }
    if let Some(roaming) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(roaming).join("XXMI Launcher"));
    }
    roots.sort();
    roots.dedup();
    roots
}
fn find_launcher(root: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for ancestor in root.into_iter().flat_map(Path::ancestors).take(3) {
        candidates.push(ancestor.join("XXMI Launcher.exe"));
        candidates.push(ancestor.join("Resources/Bin/XXMI Launcher.exe"));
    }
    for base in standard_xxmi_roots() {
        candidates.push(base.join("XXMI Launcher.exe"));
        candidates.push(base.join("Resources/Bin/XXMI Launcher.exe"));
    }
    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(target_os = "linux")]
fn wine_prefix_for_launcher(launcher: &Path) -> Option<PathBuf> {
    launcher
        .ancestors()
        .find(|p| p.file_name().is_some_and(|name| name == "drive_c"))
        .and_then(Path::parent)
        .map(Path::to_owned)
}

/// Open the XXMI settings window without starting a game.
pub fn launch_xxmi(
    root: Option<&Path>,
    xxmi: Option<&Path>,
    wine_prefix: Option<&Path>,
) -> Result<(LaunchCommand, PathBuf)> {
    xxmi_command(root, xxmi, None, wine_prefix, Vec::new())
}

fn xxmi_command(
    root: Option<&Path>,
    xxmi: Option<&Path>,
    wine: Option<&Path>,
    wine_prefix: Option<&Path>,
    arguments: Vec<OsString>,
) -> Result<(LaunchCommand, PathBuf)> {
    let launcher = xxmi
        .map(Path::to_owned)
        .or_else(|| find_launcher(root))
        .context("XXMI Launcher.exe was not found. Select it in Game setup or pass --xxmi.")?;
    let launcher = std::env::current_dir()?.join(launcher);
    let cwd = launcher.parent().map(Path::to_path_buf);
    #[cfg(target_os = "linux")]
    {
        if wine.is_none()
            && wine_prefix.is_none()
            && let Some(command) = steam_xxmi::managed_launch(&launcher, &arguments)?
        {
            return Ok((command, launcher));
        }
        let wine = wine
            .map(Path::to_owned)
            .or_else(|| std::env::var_os("WINE").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("wine"));
        let prefix = wine_prefix
            .map(Path::to_owned)
            .or_else(|| std::env::var_os("WINEPREFIX").map(PathBuf::from))
            .or_else(|| wine_prefix_for_launcher(&launcher));
        let mut args = vec![launcher.as_os_str().to_owned()];
        args.extend(arguments);
        let env = prefix
            .into_iter()
            .map(|prefix| ("WINEPREFIX".into(), prefix.into_os_string()))
            .collect();
        Ok((LaunchCommand {
            program: wine.into_os_string(),
            args,
            cwd,
            env,
            url: false,
        }, launcher))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (wine, wine_prefix);
        Ok((LaunchCommand {
            program: launcher.as_os_str().to_owned(),
            args: arguments,
            cwd,
            env: Vec::new(),
            url: false,
        }, launcher))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::wine_prefix_for_launcher;
    use super::{Path, PathBuf, validate_installation_root};
    use crate::games::{GameKind, plan, profile};

    #[test]
    fn preserves_mod_assets_and_requires_ini() {
        let profile = profile(GameKind::Zzmi).unwrap();
        let files = [
            PathBuf::from("wrapper/mod.ini"),
            PathBuf::from("wrapper/textures/body.dds"),
        ];
        let folder = plan(&profile, &files, "skin", None).unwrap();
        let paths: Vec<_> = folder
            .files
            .iter()
            .map(|file| file.destination.as_path())
            .collect();
        assert_eq!(
            paths,
            [
                Path::new("skin/mod.ini"),
                Path::new("skin/textures/body.dds")
            ]
        );
        assert!(plan(&profile, &[PathBuf::from("texture.dds")], "skin", None).is_err());
    }

    #[test]
    fn steam_setup_requires_loader_include_and_mods() {
        let temp = tempfile::tempdir().unwrap();
        let steam_roots = [temp.path().to_owned()];
        let package = temp.path().join("ZZMI");
        let mods = package.join("Mods");
        let ini = package.join("d3dx.ini");
        std::fs::create_dir_all(&mods).unwrap();
        assert!(validate_installation_root(GameKind::Zzmi, temp.path(), &steam_roots).is_err());
        assert!(validate_installation_root(GameKind::Zzmi, &mods, &steam_roots).is_err());
        std::fs::write(&ini, "[Include]\ninclude_recursive = Core\n").unwrap();
        assert!(validate_installation_root(GameKind::Zzmi, &mods, &steam_roots).is_err());
        std::fs::write(&ini, "[Include]\ninclude_recursive = Mods\n").unwrap();
        assert!(validate_installation_root(GameKind::Zzmi, &mods, &steam_roots).is_ok());
        assert!(validate_installation_root(GameKind::Efmi, &mods, &[]).is_err());
        std::fs::write(&ini, "[Loader]\ntarget = Endfield.exe\n[Include]\ninclude_recursive = Mods\n").unwrap();
        assert!(validate_installation_root(GameKind::Efmi, &mods, &[]).is_ok());
        assert!(validate_installation_root(GameKind::Zzmi, &mods, &steam_roots).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proton_launcher_uses_own_prefix() {
        let launcher = Path::new(
            "/games/steamapps/compatdata/4162040/pfx/drive_c/users/steamuser/AppData/Roaming/XXMI Launcher/Resources/Bin/XXMI Launcher.exe",
        );
        assert_eq!(
            wine_prefix_for_launcher(launcher),
            Some(PathBuf::from("/games/steamapps/compatdata/4162040/pfx"))
        );
    }
}
