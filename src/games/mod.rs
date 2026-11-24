use std::collections::HashSet;
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
mod endfield;
pub mod xxmi;
mod zzz;

pub const SUPPORTED: [GameKind; 3] = [GameKind::Cyberpunk, GameKind::Zzmi, GameKind::Efmi];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameKind {
    Cyberpunk,
    Zzmi,
    Efmi,
}

impl FromStr for GameKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "cyberpunk" | "cyberpunk2077" => Ok(Self::Cyberpunk),
            "zzmi" => Ok(Self::Zzmi),
            "efmi" | "endfield" => Ok(Self::Efmi),
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
        })
    }
}

impl GameKind {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Cyberpunk => cp::NAME,
            Self::Zzmi => zzz::NAME,
            Self::Efmi => endfield::NAME,
        }
    }

    pub const fn is_xxmi(self) -> bool {
        matches!(self, Self::Zzmi | Self::Efmi)
    }

    pub fn preferred_source(self) -> Source {
        match self {
            Self::Zzmi | Self::Efmi => Source::GameBanana,
            Self::Cyberpunk => Source::Nexus,
        }
    }

    pub const fn source_game(self, source: Source) -> Option<&'static str> {
        match (self, source) {
            (Self::Cyberpunk, Source::Nexus) => Some(cp::NEXUS_GAME),
            (Self::Cyberpunk, Source::GameBanana) => Some(cp::GAMEBANANA_GAME),
            (Self::Zzmi, Source::Nexus) => Some(zzz::NEXUS_GAME),
            (Self::Zzmi, Source::GameBanana) => Some(zzz::GAMEBANANA_GAME),
            (Self::Efmi, Source::Nexus) | (_, Source::Local) => None,
            (Self::Efmi, Source::GameBanana) => Some(endfield::GAMEBANANA_GAME),
        }
    }

    pub const fn installation_label(self) -> &'static str {
        match self {
            Self::Cyberpunk => cp::INSTALLATION_LABEL,
            Self::Zzmi => zzz::INSTALLATION_LABEL,
            Self::Efmi => endfield::INSTALLATION_LABEL,
        }
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
    match kind {
        GameKind::Cyberpunk => cp::plan(files, slug, destination),
        GameKind::Zzmi | GameKind::Efmi => xxmi::plan(kind, files, slug, destination),
    }
}
pub fn plan_bundle(
    kind: GameKind,
    packages: &[&[PathBuf]],
    slug: &str,
    destination: Option<&Path>,
) -> Result<Vec<InstallPlan>> {
    if kind.is_xxmi() {
        return xxmi::plan_bundle(kind, packages, safe_slug(slug)?, destination);
    }
    packages
        .iter()
        .map(|files| plan(kind, files, slug, destination))
        .collect()
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
    match kind {
        GameKind::Cyberpunk => cp::detect(),
        GameKind::Zzmi | GameKind::Efmi => xxmi::detect(kind),
    }
}

pub fn validate_installation_root(
    kind: GameKind,
    root: &Path,
    steam_game_roots: &[PathBuf],
) -> Result<()> {
    match kind {
        GameKind::Cyberpunk => Ok(()),
        GameKind::Zzmi => zzz::validate_installation_root(root, steam_game_roots),
        GameKind::Efmi => endfield::validate_installation_root(root),
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
    match kind {
        GameKind::Cyberpunk => Ok(cp::launch()),
        GameKind::Zzmi | GameKind::Efmi => xxmi::launch(kind, root, xxmi, wine, wine_prefix),
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
}
