use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::archive::portable_path;
use crate::sources::Source;

#[cfg(target_os = "linux")]
pub mod steam_xxmi;

const CYBERPUNK_PROFILE: &str = include_str!("../profiles/cyberpunk.toml");
const ZZMI_PROFILE: &str = include_str!("../profiles/zzmi.toml");
const GENERIC_PROFILE: &str = include_str!("../profiles/generic.toml");

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameKind {
    Cyberpunk,
    Zzmi,
    Generic,
}

impl FromStr for GameKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "cyberpunk" | "cyberpunk2077" => Ok(Self::Cyberpunk),
            "zzmi" => Ok(Self::Zzmi),
            "generic" => Ok(Self::Generic),
            _ => Err(format!("unknown game kind: {value}")),
        }
    }
}

impl fmt::Display for GameKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cyberpunk => "cyberpunk",
            Self::Zzmi => "zzmi",
            Self::Generic => "generic",
        })
    }
}

#[derive(Clone, Debug)]
pub struct GameProfile {
    pub kind: GameKind,
    pub id: String,
    pub name: String,
    /// Optional relative destination prefix for a folder-based profile.
    pub installation_root: Option<PathBuf>,
    /// Required file extension, such as `.ini`, for folder-based imports.
    pub required_file: Option<String>,
    source_games: BTreeMap<String, String>,
    settings: ProfileSettings,
}

impl GameProfile {
    pub fn source_game(&self, source: Source) -> Option<&str> {
        let key = match source {
            Source::GameBanana => "gamebanana",
            Source::Nexus => "nexus",
            Source::Local => return None,
        };
        self.source_games.get(key).map(String::as_str)
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ProfileSettings {
    #[serde(default)]
    steam_app_id: Option<u32>,
    #[serde(default)]
    steam_url: Option<String>,
    installation: InstallationSettings,
    launch: LaunchSettings,
}

#[derive(Clone, Debug, Deserialize)]
struct InstallationSettings {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    recognized_roots: Vec<String>,
    #[serde(default)]
    required_file: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct LaunchSettings {
    #[serde(default)]
    arguments: Vec<String>,
}

#[derive(Deserialize)]
struct RawProfile {
    id: String,
    name: String,
    #[serde(default)]
    sources: BTreeMap<String, String>,
    #[serde(flatten)]
    settings: ProfileSettings,
}

pub fn profile(kind: GameKind) -> Result<GameProfile> {
    let contents = match kind {
        GameKind::Cyberpunk => CYBERPUNK_PROFILE,
        GameKind::Zzmi => ZZMI_PROFILE,
        GameKind::Generic => GENERIC_PROFILE,
    };
    parse_profile(kind, contents)
}

/// Parse a profile using the same TOML schema as the built-in descriptors.
///
/// Generic profiles can set `[installation].root` and `required_file`.
pub fn parse_profile(kind: GameKind, contents: &str) -> Result<GameProfile> {
    let raw: RawProfile = toml::from_str(contents).context("invalid game profile")?;
    if raw.id.trim().is_empty() || raw.name.trim().is_empty() {
        bail!("game profile id and name must not be empty");
    }
    Ok(GameProfile {
        kind,
        id: raw.id,
        name: raw.name,
        installation_root: raw.settings.installation.root.as_deref().map(PathBuf::from),
        required_file: raw.settings.installation.required_file.clone(),
        source_games: raw.sources,
        settings: raw.settings,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct FileMapping {
    pub source: PathBuf,
    pub destination: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    #[default]
    Layout,
    Inferred,
    Explicit,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct InstallPlan {
    pub placement: Placement,
    pub files: Vec<FileMapping>,
    pub warnings: Vec<String>,
}

pub fn plan(
    profile: &GameProfile,
    files: &[PathBuf],
    slug: &str,
    destination: Option<&Path>,
) -> Result<InstallPlan> {
    let slug = safe_slug(slug)?;
    if files.is_empty() {
        bail!("cannot plan an installation with no files");
    }
    match profile.kind {
        GameKind::Cyberpunk => plan_cyberpunk(profile, files, slug, destination),
        GameKind::Zzmi => {
            ensure!(
                destination.is_none(),
                "ZZMI mods must install directly under the configured Mods folder"
            );
            plan_mod_folder(profile, files, slug, None, true)
        }
        GameKind::Generic => plan_mod_folder(
            profile,
            files,
            slug,
            destination.or(profile.installation_root.as_deref()),
            true,
        ),
    }
}
pub fn plan_bundle(
    profile: &GameProfile,
    packages: &[&[PathBuf]],
    slug: &str,
    destination: Option<&Path>,
) -> Result<Vec<InstallPlan>> {
    if profile.kind == GameKind::Zzmi {
        ensure!(
            destination.is_none(),
            "ZZMI mods must install directly under the configured Mods folder"
        );
        let required_file = profile
            .settings
            .installation
            .required_file
            .as_deref()
            .filter(|value| !value.is_empty());
        if let Some(required_file) = required_file {
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
        return packages
            .iter()
            .map(|files| plan_mod_folder(profile, files, slug, None, false))
            .collect();
    }
    packages
        .iter()
        .map(|files| plan(profile, files, slug, destination))
        .collect()
}

fn plan_mod_folder(
    profile: &GameProfile,
    files: &[PathBuf],
    slug: &str,
    destination: Option<&Path>,
    enforce_required: bool,
) -> Result<InstallPlan> {
    let paths = normalized_paths(files)?;
    let required_file = profile
        .settings
        .installation
        .required_file
        .as_deref()
        .filter(|value| !value.is_empty());
    let requires_file = enforce_required && required_file.is_some();
    if enforce_required && let Some(extension) = required_file {
        let suffix = format!(".{}", extension.trim_start_matches('.')).to_ascii_lowercase();
        if !paths
            .iter()
            .any(|(_, path)| path.to_ascii_lowercase().ends_with(&suffix))
        {
            bail!(
                "{} mods must contain at least one {suffix} file",
                profile.name
            );
        }
    }
    let wrapper = common_wrapper(paths.iter().map(|(_, path)| path.as_str()));
    let root = destination
        .map(portable_path)
        .transpose()?
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::new());
    let mut plan = InstallPlan::default();
    if profile.kind == GameKind::Generic {
        plan.placement = Placement::Explicit;
    }
    for (source, path) in paths {
        let relative = strip_wrapper(&path, wrapper.as_deref());
        if relative.is_empty() {
            continue;
        }
        let mut target = root.clone();
        target.push(slug);
        target.push(relative);
        plan.files.push(FileMapping {
            source,
            destination: target,
        });
    }
    if profile.kind == GameKind::Generic && !requires_file {
        plan.warnings.push(
            "Generic folder placement is profile-defined; review the destination before deployment."
                .into(),
        );
    }

    validate_plan(plan)
}

fn plan_cyberpunk(
    profile: &GameProfile,
    files: &[PathBuf],
    slug: &str,
    override_dir: Option<&Path>,
) -> Result<InstallPlan> {
    let mut paths = normalized_paths(files)?;
    let recognized = &profile.settings.installation.recognized_roots;
    let wrapper = common_wrapper(
        paths
            .iter()
            .filter(|(_, path)| !documentation(path))
            .map(|(_, path)| path.as_str()),
    )
    .filter(|wrapper| {
        !recognized
            .iter()
            .any(|root| root.eq_ignore_ascii_case(wrapper))
    });
    for (_, path) in &mut paths {
        *path = strip_wrapper(path, wrapper.as_deref());
    }

    let standalone_redmod = paths
        .iter()
        .any(|(_, path)| path.eq_ignore_ascii_case("info.json"))
        && paths.iter().any(|(_, path)| {
            path.split_once('/')
                .is_some_and(|(head, _)| head.eq_ignore_ascii_case("archive"))
                && extension(path).as_deref() == Some("archive")
        });
    let has_game_layout = !standalone_redmod
        && paths.iter().any(|(_, path)| {
            path.split_once('/').is_some_and(|(head, _)| {
                recognized
                    .iter()
                    .any(|root| head.eq_ignore_ascii_case(root))
            })
        });
    let (cet_root, redmod_root) = if has_game_layout {
        (None, None)
    } else {
        let cet = package_root(&paths, "init.lua");
        let redmod = package_root(&paths, "info.json");
        ensure!(
            cet.is_none() || redmod.is_none(),
            "archive contains both CET and REDmod markers; select one package"
        );
        (cet.map(str::to_owned), redmod.map(str::to_owned))
    };
    let override_dir = override_dir
        .map(portable_path)
        .transpose()?
        .map(PathBuf::from);
    let mut plan = InstallPlan::default();
    if !has_game_layout && cet_root.is_none() && redmod_root.is_none() {
        plan.placement = Placement::Inferred;
    }
    for (source, path) in paths {
        let rooted = if has_game_layout {
            path.split_once('/').and_then(|(head, rest)| {
                recognized
                    .iter()
                    .find(|root| head.eq_ignore_ascii_case(root))
                    .map(|root| (root, rest))
            })
        } else {
            None
        };
        if rooted.is_none() && cet_root.is_none() && redmod_root.is_none() && documentation(&path) {
            plan.warnings.push(format!(
                "Skipped documentation outside the game layout: {path}"
            ));
            continue;
        }
        let target = if let Some((root, rest)) = rooted {
            PathBuf::from(root).join(rest)
        } else if has_game_layout {
            explicit_placement(&path, override_dir.as_deref(), &mut plan)?
        } else if let Some(root) = &cet_root {
            if let Some(inner) = strip_prefix_ci(&path, root) {
                PathBuf::from("bin/x64/plugins/cyber_engine_tweaks/mods")
                    .join(slug)
                    .join(inner)
            } else {
                standalone_target(&path, slug, override_dir.as_deref(), &mut plan)?
            }
        } else if let Some(root) = &redmod_root {
            if let Some(inner) = strip_prefix_ci(&path, root) {
                PathBuf::from("mods").join(slug).join(inner)
            } else {
                standalone_target(&path, slug, override_dir.as_deref(), &mut plan)?
            }
        } else {
            standalone_target(&path, slug, override_dir.as_deref(), &mut plan)?
        };
        plan.files.push(FileMapping {
            source,
            destination: target,
        });
    }
    validate_plan(plan)
}

fn standalone_target(
    path: &str,
    slug: &str,
    override_dir: Option<&Path>,
    plan: &mut InstallPlan,
) -> Result<PathBuf> {
    Ok(match extension(path).as_deref() {
        Some("archive" | "xl") => PathBuf::from("archive/pc/mod").join(file_name(path)),
        Some("reds") => PathBuf::from("r6/scripts").join(slug).join(path),
        Some("yaml" | "yml" | "tweak") => PathBuf::from("r6/tweaks").join(file_name(path)),
        _ => explicit_placement(path, override_dir, plan)?,
    })
}

fn explicit_placement(
    path: &str,
    destination: Option<&Path>,
    plan: &mut InstallPlan,
) -> Result<PathBuf> {
    let dir = destination.with_context(|| {
        format!(
            "ambiguous Cyberpunk file placement for '{path}'; use --into to choose a destination"
        )
    })?;
    plan.placement = Placement::Explicit;
    plan.warnings
        .push(format!("Placed '{path}' using an explicit destination."));
    Ok(dir.join(path))
}

fn package_root<'a>(paths: &'a [(PathBuf, String)], marker: &str) -> Option<&'a str> {
    paths.iter().find_map(|(_, path)| {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        name.eq_ignore_ascii_case(marker).then_some(parent)
    })
}

fn documentation(path: &str) -> bool {
    let name = file_name(path);
    let stem = if let Some((stem, extension)) = name.rsplit_once('.') {
        if !["txt", "md", "pdf", "rtf"]
            .iter()
            .any(|format| extension.eq_ignore_ascii_case(format))
        {
            return false;
        }
        stem
    } else {
        name
    };
    ["readme", "license", "changelog", "credits"]
        .iter()
        .any(|label| stem.eq_ignore_ascii_case(label))
}

fn validate_plan(plan: InstallPlan) -> Result<InstallPlan> {
    ensure!(
        !plan.files.is_empty(),
        "archive contains no installable files"
    );
    let mut destinations = HashSet::with_capacity(plan.files.len());
    for mapping in &plan.files {
        let key = mapping.destination.to_string_lossy().to_lowercase();
        if !destinations.insert(key) {
            bail!(
                "multiple source files map to the same destination '{}'",
                mapping.destination.display()
            );
        }
    }
    Ok(plan)
}

fn normalized_paths(files: &[PathBuf]) -> Result<Vec<(PathBuf, String)>> {
    files
        .iter()
        .map(|source| {
            let path = portable_path(source)
                .with_context(|| format!("invalid source path {}", source.display()))?;
            Ok((source.clone(), path))
        })
        .collect()
}

fn common_wrapper<'a>(mut paths: impl Iterator<Item = &'a str>) -> Option<String> {
    let (first, _) = paths.next()?.split_once('/')?;
    paths
        .all(|path| {
            path.split_once('/')
                .is_some_and(|(head, _)| head.eq_ignore_ascii_case(first))
        })
        .then(|| first.to_owned())
}

fn strip_wrapper(path: &str, wrapper: Option<&str>) -> String {
    wrapper
        .and_then(|wrapper| {
            path.split_once('/')
                .filter(|(head, _)| head.eq_ignore_ascii_case(wrapper))
                .map(|(_, rest)| rest.to_owned())
        })
        .unwrap_or_else(|| path.to_owned())
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
fn extension(path: &str) -> Option<String> {
    file_name(path)
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
}
fn strip_prefix_ci<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return Some(path);
    }
    if path.eq_ignore_ascii_case(prefix) {
        return Some("");
    }
    let head = path.get(..prefix.len())?;
    let rest = path.get(prefix.len()..)?;
    if head.eq_ignore_ascii_case(prefix) {
        rest.strip_prefix('/')
    } else {
        None
    }
}
fn safe_slug(slug: &str) -> Result<&str> {
    let path = Path::new(slug);
    if slug.trim().is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || slug.contains('/')
        || slug.contains('\\')
    {
        bail!("mod slug must be a single safe path component");
    }
    portable_path(path)?;
    Ok(slug)
}

pub fn detect(kind: GameKind) -> Result<Vec<PathBuf>> {
    match kind {
        GameKind::Cyberpunk => detect_steam_app(
            profile(kind)?
                .settings
                .steam_app_id
                .context("Cyberpunk profile has no Steam AppID")?,
        ),
        GameKind::Zzmi => {
            let mut candidates = standard_xxmi_roots()
                .into_iter()
                .flat_map(|base| [base.join("ZZMI/Mods"), base.join("Mods")])
                .chain(
                    detect_zzz_game()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|root| root.join("ZZMI/Mods")),
                )
                .filter(|path| is_zzmi_mods(path))
                .filter_map(|path| path.canonicalize().ok())
                .collect::<Vec<_>>();
            candidates.sort();
            candidates.dedup();
            Ok(candidates)
        }
        GameKind::Generic => Ok(Vec::new()),
    }
}

fn detect_steam_app(app_id: u32) -> Result<Vec<PathBuf>> {
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
/// Find native Zenless Zone Zero installations registered in Steam.
pub fn detect_zzz_game() -> Result<Vec<PathBuf>> {
    detect_steam_app(4_162_040)
}

/// Whether a Mods directory has the ZZMI package's loader configuration.
pub fn is_zzmi_mods(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "Mods")
        && path.parent().is_some_and(valid_zzmi_package)
}

fn valid_zzmi_package(package: &Path) -> bool {
    let ini = package.join("d3dx.ini");
    if !ini.is_file() || !package.join("Mods").is_dir() {
        return false;
    }
    let Ok(contents) = std::fs::read_to_string(ini) else {
        return false;
    };
    contents.lines().any(|line| {
        let line = line.trim();
        line.split_once('=').is_some_and(|(key, value)| {
            key.trim().eq_ignore_ascii_case("include_recursive")
                && value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case("Mods")
        })
    })
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
                        roots.push(library.path().join("steamapps/compatdata/4162040/pfx/drive_c/users/steamuser/AppData/Roaming/XXMI Launcher"));
                        #[cfg(target_os = "linux")]
                        roots.push(
                            steam_xxmi::compatdata_dir(library.path())
                                .join(steam_xxmi::PORTABLE_DIR),
                        );
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

#[derive(Debug)]
pub struct LaunchCommand {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
    url: bool,
}

impl LaunchCommand {
    pub fn spawn(&self) -> Result<()> {
        if self.url {
            let value = self
                .program
                .to_str()
                .context("launch URL is not valid UTF-8")?;
            open::that(value).context("failed to open game launch URL")?;
            return Ok(());
        }
        self.process()
            .spawn()
            .with_context(|| format!("failed to launch {}", Path::new(&self.program).display()))?;
        Ok(())
    }

    fn process(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).stdin(Stdio::null());
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }
}

pub fn launch(
    kind: GameKind,
    root: &Path,
    xxmi: Option<&Path>,
    wine: Option<&Path>,
    wine_prefix: Option<&Path>,
    custom: &[OsString],
) -> Result<LaunchCommand> {
    launch_profile(&profile(kind)?, root, xxmi, wine, wine_prefix, custom)
}

pub fn launch_profile(
    profile: &GameProfile,
    root: &Path,
    xxmi: Option<&Path>,
    wine: Option<&Path>,
    wine_prefix: Option<&Path>,
    custom: &[OsString],
) -> Result<LaunchCommand> {
    if !custom.is_empty() {
        if custom[0].is_empty() {
            bail!("custom launch command executable cannot be empty");
        }
        return Ok(LaunchCommand {
            program: custom[0].clone(),
            args: custom[1..].to_vec(),
            cwd: Some(root.to_owned()),
            env: Vec::new(),
            url: false,
        });
    }
    match profile.kind {
        GameKind::Cyberpunk => Ok(LaunchCommand {
            program: OsString::from(
                profile
                    .settings
                    .steam_url
                    .as_deref()
                    .context("Cyberpunk profile has no Steam URL")?,
            ),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            url: true,
        }),
        GameKind::Zzmi => xxmi_command(
            Some(root),
            xxmi,
            wine,
            wine_prefix,
            profile
                .settings
                .launch
                .arguments
                .iter()
                .map(OsString::from)
                .collect(),
        ),
        GameKind::Generic => bail!("generic games require a custom launch command"),
    }
}

/// Open the XXMI settings window without starting a game.
pub fn start_xxmi(
    root: Option<&Path>,
    xxmi: Option<&Path>,
    wine_prefix: Option<&Path>,
) -> Result<LaunchCommand> {
    xxmi_command(root, xxmi, None, wine_prefix, Vec::new())
}

fn xxmi_command(
    root: Option<&Path>,
    xxmi: Option<&Path>,
    wine: Option<&Path>,
    wine_prefix: Option<&Path>,
    arguments: Vec<OsString>,
) -> Result<LaunchCommand> {
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
            return Ok(command);
        }
        let wine = wine
            .map(Path::to_owned)
            .or_else(|| std::env::var_os("WINE").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("wine"));
        let prefix = wine_prefix
            .map(Path::to_owned)
            .or_else(|| std::env::var_os("WINEPREFIX").map(PathBuf::from))
            .or_else(|| wine_prefix_for_launcher(&launcher));
        let mut args = vec![launcher.into_os_string()];
        args.extend(arguments);
        let env = prefix
            .into_iter()
            .map(|prefix| ("WINEPREFIX".into(), prefix.into_os_string()))
            .collect();
        Ok(LaunchCommand {
            program: wine.into_os_string(),
            args,
            cwd,
            env,
            url: false,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (wine, wine_prefix);
        Ok(LaunchCommand {
            program: launcher.into_os_string(),
            args: arguments,
            cwd,
            env: Vec::new(),
            url: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyberpunk_preserves_game_root_and_strips_wrapper_once() {
        let profile = profile(GameKind::Cyberpunk).unwrap();
        let files = [
            PathBuf::from("Release/archive/pc/mod/body.archive"),
            PathBuf::from("Release/r6/tweaks/body.yaml"),
        ];
        let plan = plan(&profile, &files, "body", None).unwrap();
        assert_eq!(
            plan.files
                .iter()
                .map(|f| f.destination.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["archive/pc/mod/body.archive", "r6/tweaks/body.yaml"]
        );
    }

    #[test]
    fn cyberpunk_places_standalone_formats_and_rejects_ambiguity() {
        let profile = profile(GameKind::Cyberpunk).unwrap();
        let files = [
            "pack.archive",
            "pack.xl",
            "script.reds",
            "config.yaml",
            "settings.tweak",
        ]
        .map(PathBuf::from);
        let inferred = plan(&profile, &files, "mod", None).unwrap();
        assert_eq!(
            inferred
                .files
                .iter()
                .map(|f| f.destination.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "archive/pc/mod/pack.archive",
                "archive/pc/mod/pack.xl",
                "r6/scripts/mod/script.reds",
                "r6/tweaks/config.yaml",
                "r6/tweaks/settings.tweak"
            ]
        );
        assert!(plan(&profile, &[PathBuf::from("plugin.dll")], "mod", None).is_err());
        assert!(
            plan(
                &profile,
                &[PathBuf::from("plugin.dll")],
                "mod",
                Some(Path::new("bin/x64/plugins"))
            )
            .is_ok()
        );
    }

    #[test]
    fn zzmi_preserves_mod_assets_and_requires_ini() {
        let profile = profile(GameKind::Zzmi).unwrap();
        let files = [
            PathBuf::from("wrapper/mod.ini"),
            PathBuf::from("wrapper/textures/body.dds"),
        ];
        let folder = plan(&profile, &files, "skin", None).unwrap();
        assert_eq!(
            folder
                .files
                .iter()
                .map(|f| f.destination.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["skin/mod.ini", "skin/textures/body.dds"]
        );
        assert!(plan(&profile, &[PathBuf::from("texture.dds")], "skin", None).is_err());
    }

    #[test]
    fn cyberpunk_detects_cet_and_redmod_package_layouts() {
        let profile = profile(GameKind::Cyberpunk).unwrap();
        let cet = [
            PathBuf::from("Ellen/init.lua"),
            PathBuf::from("Ellen/scripts/main.lua"),
        ];
        let cet_plan = plan(&profile, &cet, "ellen", None).unwrap();
        assert_eq!(
            cet_plan
                .files
                .iter()
                .map(|f| f.destination.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "bin/x64/plugins/cyber_engine_tweaks/mods/ellen/init.lua",
                "bin/x64/plugins/cyber_engine_tweaks/mods/ellen/scripts/main.lua",
            ]
        );

        let redmod = [
            PathBuf::from("Package/info.json"),
            PathBuf::from("Package/archive/base.archive"),
        ];
        let plan = plan(&profile, &redmod, "package", None).unwrap();
        assert_eq!(
            plan.files
                .iter()
                .map(|f| f.destination.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "mods/package/info.json",
                "mods/package/archive/base.archive",
            ]
        );
    }

    #[test]
    fn prefix_matching_is_utf8_safe() {
        assert_eq!(
            strip_prefix_ci("éclair/file.lua", "éclair"),
            Some("file.lua")
        );
        assert_eq!(strip_prefix_ci("éclair/file.lua", "x"), None);
    }

    #[test]
    fn generic_profile_configures_destination_and_source_ids() {
        let contents = r#"
id = "custom"
name = "Custom Game"

[sources]
nexus = "custom-game"

[installation]
root = "mods"
required_file = ""

[launch]
"#;
        let profile = parse_profile(GameKind::Generic, contents).unwrap();
        assert_eq!(profile.source_game(Source::Nexus), Some("custom-game"));
        assert_eq!(
            profile.installation_root.as_deref(),
            Some(Path::new("mods"))
        );
        let plan = plan(&profile, &[PathBuf::from("wrapper/file.bin")], "slug", None).unwrap();
        assert_eq!(
            plan.files[0].destination,
            PathBuf::from("mods/slug/file.bin")
        );
    }
    #[test]
    fn zzmi_package_detection_requires_loader_include_and_mods() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("ZZMI");
        std::fs::create_dir_all(package.join("Mods")).unwrap();
        std::fs::write(
            package.join("d3dx.ini"),
            "[Include]\ninclude_recursive = Mods\n",
        )
        .unwrap();
        assert!(valid_zzmi_package(&package));
        std::fs::write(
            package.join("d3dx.ini"),
            "[Include]\ninclude_recursive = Core\n",
        )
        .unwrap();
        assert!(!valid_zzmi_package(&package));
        std::fs::remove_file(package.join("d3dx.ini")).unwrap();
        assert!(!valid_zzmi_package(&package));
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
