use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::archive::portable_path;
use crate::sources::Source;

pub mod cp;
pub mod discovery;
pub mod xxmi;

const GENERIC_PROFILE: &str = include_str!("../../profiles/generic.toml");

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameKind {
    Cyberpunk,
    Zzmi,
    Efmi,
    Generic,
}

impl FromStr for GameKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "cyberpunk" | "cyberpunk2077" => Ok(Self::Cyberpunk),
            "zzmi" => Ok(Self::Zzmi),
            "efmi" | "endfield" => Ok(Self::Efmi),
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
            Self::Efmi => "efmi",
            Self::Generic => "generic",
        })
    }
}

impl GameKind {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Cyberpunk => "Cyberpunk 2077",
            Self::Zzmi => "Zenless Zone Zero",
            Self::Efmi => "Arknights: Endfield",
            Self::Generic => "Custom game",
        }
    }

    pub const fn is_xxmi(self) -> bool {
        matches!(self, Self::Zzmi | Self::Efmi)
    }

    pub fn preferred_source(self) -> Source {
        match self {
            Self::Zzmi | Self::Efmi => Source::GameBanana,
            Self::Cyberpunk | Self::Generic => Source::Nexus,
        }
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
        GameKind::Cyberpunk => cp::PROFILE,
        GameKind::Zzmi => include_str!("zzz/profile.toml"),
        GameKind::Efmi => include_str!("efmi.toml"),
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
        GameKind::Cyberpunk => cp::plan(profile, files, slug, destination),
        GameKind::Zzmi | GameKind::Efmi => xxmi::plan(profile, files, slug, destination),
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
    if profile.kind.is_xxmi() {
        return xxmi::plan_bundle(profile, packages, safe_slug(slug)?, destination);
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
        GameKind::Cyberpunk => cp::detect(),
        GameKind::Zzmi | GameKind::Efmi => xxmi::detect(kind),
        GameKind::Generic => Ok(Vec::new()),
    }
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
        self.spawn_process()?;
        Ok(())
    }

    pub fn spawn_process(&self) -> Result<Child> {
        ensure!(!self.url, "a launch URL has no child process to track");
        self.process()
            .spawn()
            .with_context(|| format!("failed to launch {}", Path::new(&self.program).display()))
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
        GameKind::Cyberpunk => cp::launch(profile),
        GameKind::Zzmi | GameKind::Efmi => xxmi::launch(profile, root, xxmi, wine, wine_prefix),
        GameKind::Generic => bail!("generic games require a custom launch command"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endfield_launch_selects_efmi() -> Result<()> {
        let root = tempfile::tempdir()?;
        let launcher = root.path().join("XXMI Launcher.exe");
        let command = launch(
            GameKind::Efmi,
            root.path(),
            Some(&launcher),
            Some(Path::new("wine")),
            None,
            &[],
        )?;
        let process = command.process();
        let mut args = process.get_args();
        #[cfg(target_os = "linux")]
        assert_eq!(args.next(), Some(launcher.as_os_str()));
        assert!(args.eq(["--nogui", "--xxmi", "EFMI"].map(std::ffi::OsStr::new)));
        Ok(())
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
}
