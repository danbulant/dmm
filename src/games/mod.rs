use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::archive::{ArchiveTree, portable_path, unpack};
use crate::sources::Source;

pub mod cp;
pub mod discovery;
mod endfield;
mod ff7;
pub mod xxmi;
mod zzz;

pub const SUPPORTED: [GameKind; 4] = [
    GameKind::Cyberpunk,
    GameKind::Zzmi,
    GameKind::Efmi,
    GameKind::Ff7Remake,
];

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameKind {
    #[default]
    Cyberpunk,
    Zzmi,
    Efmi,
    Ff7Remake,
}

impl FromStr for GameKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        SUPPORTED
            .into_iter()
            .find(|kind| {
                let profile = kind.profile();
                value.eq_ignore_ascii_case(profile.id)
                    || profile
                        .aliases
                        .iter()
                        .any(|alias| value.eq_ignore_ascii_case(alias))
            })
            .ok_or_else(|| format!("unknown game kind: {value}"))
    }
}

impl fmt::Display for GameKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.profile().id)
    }
}

struct GameProfile {
    id: &'static str,
    aliases: &'static [&'static str],
    adapter: &'static dyn GameAdapter,
    name: &'static str,
    nexus: Option<&'static str>,
    gamebanana: Option<&'static str>,
    preferred_source: Source,
    installation_label: &'static str,
    #[cfg(feature = "rune-ui")]
    presentation: GamePresentation,
}

#[cfg(feature = "rune-ui")]
pub struct GamePresentation {
    pub icon: &'static [u8],
    pub setup_note: Option<&'static str>,
    pub destination_override: bool,
}

impl GameKind {
    const fn profile(self) -> &'static GameProfile {
        match self {
            Self::Cyberpunk => &cp::PROFILE,
            Self::Zzmi => &zzz::PROFILE,
            Self::Efmi => &endfield::PROFILE,
            Self::Ff7Remake => &ff7::PROFILE,
        }
    }

    pub const fn display_name(self) -> &'static str {
        self.profile().name
    }

    #[cfg(feature = "rune-ui")]
    pub const fn presentation(self) -> &'static GamePresentation {
        &self.profile().presentation
    }

    pub fn is_xxmi(self) -> bool {
        self.profile().adapter.is_xxmi()
    }

    pub fn preferred_source(self) -> Source {
        self.profile().preferred_source
    }

    pub const fn source_game(self, source: Source) -> Option<&'static str> {
        match source {
            Source::Nexus => self.profile().nexus,
            Source::GameBanana => self.profile().gamebanana,
            Source::Local => None,
        }
    }

    pub const fn installation_label(self) -> &'static str {
        self.profile().installation_label
    }
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

trait GameAdapter: Sync {
    fn detect(&self) -> Result<Vec<PathBuf>>;

    fn launch(&self, context: LaunchContext<'_>) -> Result<LaunchCommand>;

    fn plan(&self, files: &[PathBuf], slug: &str, destination: Option<&Path>) -> Result<InstallPlan>;

    fn plan_bundle(
        &self,
        packages: &[&[PathBuf]],
        slug: &str,
        destination: Option<&Path>,
    ) -> Result<Vec<InstallPlan>> {
        packages
            .iter()
            .map(|files| {
                ensure!(!files.is_empty(), "cannot plan an installation with no files");
                self.plan(files, slug, destination)
            })
            .collect()
    }

    fn is_xxmi(&self) -> bool {
        false
    }

    fn validate_registration_root(&self, _root: &Path) -> Result<()> {
        Ok(())
    }

    fn validate_setup_root(&self, root: &Path, _steam_game_roots: &[PathBuf]) -> Result<()> {
        self.validate_registration_root(root)
    }

    fn unpack_mod(
        &self,
        input: &Path,
        scratch: &Path,
        _filename: Option<&str>,
    ) -> Result<ArchiveTree> {
        unpack(input, scratch)
    }
}

struct LaunchContext<'a> {
    root: &'a Path,
    xxmi: Option<&'a Path>,
    wine: Option<&'a Path>,
    wine_prefix: Option<&'a Path>,
}

pub fn plan(
    kind: GameKind,
    files: &[PathBuf],
    slug: &str,
    destination: Option<&Path>,
) -> Result<InstallPlan> {
    let slug = safe_slug(slug)?;
    if files.is_empty() {
        bail!("cannot plan an installation with no files");
    }
    kind.profile().adapter.plan(files, slug, destination)
}
pub fn plan_bundle(
    kind: GameKind,
    packages: &[&[PathBuf]],
    slug: &str,
    destination: Option<&Path>,
) -> Result<Vec<InstallPlan>> {
    kind.profile().adapter.plan_bundle(packages, safe_slug(slug)?, destination)
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

fn strip_wrapper<'a>(path: &'a str, wrapper: Option<&str>) -> &'a str {
    wrapper
        .and_then(|wrapper| {
            path.split_once('/')
                .filter(|(head, _)| head.eq_ignore_ascii_case(wrapper))
                .map(|(_, rest)| rest)
        })
        .unwrap_or(path)
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
    kind.profile().adapter.detect()
}

pub fn validate_installation_root(
    kind: GameKind,
    root: &Path,
    steam_game_roots: &[PathBuf],
) -> Result<()> {
    kind.profile().adapter.validate_setup_root(root, steam_game_roots)
}

pub(crate) fn validate_registration_root(kind: GameKind, root: &Path) -> Result<()> {
    kind.profile().adapter.validate_registration_root(root)
}

pub(crate) fn unpack_mod(
    kind: GameKind,
    input: &Path,
    scratch: &Path,
    filename: Option<&str>,
) -> Result<ArchiveTree> {
    kind.profile().adapter.unpack_mod(input, scratch, filename)
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
    fn steam(app_id: u32) -> Self {
        Self {
            program: OsString::from(format!("steam://rungameid/{app_id}")),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            url: true,
        }
    }

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
    kind.profile().adapter.launch(LaunchContext {
        root,
        xxmi,
        wine,
        wine_prefix,
    })
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
}
