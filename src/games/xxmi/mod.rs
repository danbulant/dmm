#[cfg(target_os = "linux")]
pub mod steam_xxmi;
#[cfg(feature = "rune-ui")]
pub mod game_process;
#[cfg(feature = "rune-ui")]
pub mod launcher_window;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use super::discovery::detect_steam_app;
use super::{
    FileMapping, GameAdapter, GameKind, InstallPlan, LaunchCommand, LaunchContext, common_wrapper,
    endfield, normalized_paths, strip_wrapper, validate_plan, zzz,
};

#[derive(Clone, Copy)]
pub(super) struct SteamApp {
    pub(super) kind: GameKind,
    pub(super) id: u32,
    pub(super) importer: &'static str,
    #[cfg(target_os = "linux")]
    pub(super) executables: &'static [&'static str],
}

impl SteamApp {
    fn compatdata(self, library: &Path) -> PathBuf {
        library.join("steamapps/compatdata").join(self.id.to_string())
    }
}

const STEAM_APPS: [SteamApp; 2] = [zzz::STEAM_APP, endfield::STEAM_APP];

fn validate_destination(destination: Option<&Path>) -> Result<()> {
    ensure!(
        destination.is_none(),
        "XXMI mods must install directly under the configured Mods folder"
    );
    Ok(())
}

pub(super) struct XxmiAdapter {
    app: SteamApp,
    validate_setup: fn(&Path, &[PathBuf]) -> Result<()>,
}

impl XxmiAdapter {
    pub(super) const fn new(
        app: SteamApp,
        validate_setup: fn(&Path, &[PathBuf]) -> Result<()>,
    ) -> Self {
        Self { app, validate_setup }
    }
}

impl GameAdapter for XxmiAdapter {
    fn is_xxmi(&self) -> bool {
        true
    }

    fn detect(&self) -> Result<Vec<PathBuf>> {
        let app = self.app;
        let steam_roots = detect_steam_app(app.id).unwrap_or_default();
        let mut candidates = standard_xxmi_roots()
            .into_iter()
            .flat_map(|base| [base.join(app.importer).join("Mods"), base.join("Mods")])
            .chain(steam_roots.into_iter().map(|root| root.join(app.importer).join("Mods")))
            .filter(|path| is_importer_mods(path, app.kind))
            .filter_map(|path| path.canonicalize().ok())
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        Ok(candidates)
    }

    fn launch(&self, context: LaunchContext<'_>) -> Result<LaunchCommand> {
        let args = ["--nogui", "--xxmi", self.app.importer].map(OsString::from).into();
        xxmi_command(
            Some(context.root),
            context.xxmi,
            context.wine,
            context.wine_prefix,
            args,
        )
        .map(|(command, _)| command)
    }

    fn validate_setup_root(&self, root: &Path, steam_game_roots: &[PathBuf]) -> Result<()> {
        (self.validate_setup)(root, steam_game_roots)
    }

    fn plan(&self, files: &[PathBuf], slug: &str, destination: Option<&Path>) -> Result<InstallPlan> {
        validate_destination(destination)?;
        require_ini(self.app.kind, files.iter())?;
        plan_mod_folder(files, slug)
    }

    fn plan_bundle(
        &self,
        packages: &[&[PathBuf]],
        slug: &str,
        destination: Option<&Path>,
    ) -> Result<Vec<InstallPlan>> {
        validate_destination(destination)?;
        require_ini(self.app.kind, packages.iter().flat_map(|package| package.iter()))?;
        packages
            .iter()
            .map(|files| plan_mod_folder(files, slug))
            .collect()
    }
}

fn require_ini<'a>(kind: GameKind, mut files: impl Iterator<Item = &'a PathBuf>) -> Result<()> {
    ensure!(
        files.any(|path| path.to_str().is_some_and(|path| {
            path.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("ini"))
        })),
        "{} mods must contain at least one .ini file",
        kind.display_name()
    );
    Ok(())
}

fn plan_mod_folder(files: &[PathBuf], slug: &str) -> Result<InstallPlan> {
    let paths = normalized_paths(files)?;
    let wrapper = common_wrapper(paths.iter().map(|(_, path)| path.as_str()));
    validate_plan(InstallPlan {
        files: paths
            .into_iter()
            .map(|(source, path)| FileMapping {
                source,
                destination: Path::new(slug).join(strip_wrapper(&path, wrapper.as_deref())),
            })
            .collect(),
        ..InstallPlan::default()
    })
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

fn steam_app(kind: GameKind) -> SteamApp {
    STEAM_APPS
        .into_iter()
        .find(|app| app.kind == kind)
        .expect("not an XXMI game")
}

pub(super) fn detect_game(kind: GameKind) -> Result<Vec<PathBuf>> {
    detect_steam_app(steam_app(kind).id)
}

pub(super) fn is_importer_mods(path: &Path, kind: GameKind) -> bool {
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
                && value.eq_ignore_ascii_case(endfield::EXE);
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
    use super::{Path, PathBuf};
    use crate::games::{GameKind, plan, validate_installation_root};

    #[test]
    fn preserves_mod_assets_and_requires_ini() {
        let files = [
            PathBuf::from("wrapper/mod.ini"),
            PathBuf::from("wrapper/textures/body.dds"),
        ];
        let folder = plan(GameKind::Zzmi, &files, "skin", None).unwrap();
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
        assert!(plan(GameKind::Zzmi, &[PathBuf::from("texture.dds")], "skin", None).is_err());
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
