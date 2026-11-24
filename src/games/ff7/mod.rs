use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail, ensure};

#[cfg(feature = "rune-ui")]
use super::GamePresentation;
use super::discovery::detect_steam_app;
use super::{
    FileMapping, GameAdapter, GameProfile, InstallPlan, LaunchCommand, LaunchContext, Placement,
    validate_plan,
};
use crate::archive::{ArchiveTree, copy_standalone, portable_path, unpack};
use crate::sources::Source;

pub(super) const PROFILE: GameProfile = GameProfile {
    id: "ff7_remake",
    aliases: &["ff7remake", "ff7r"],
    adapter: &Ff7Remake,
    name: "Final Fantasy VII Remake Intergrade",
    nexus: Some("finalfantasy7remake"),
    gamebanana: Some("15536"),
    preferred_source: Source::Nexus,
    installation_label: "Game installation folder",
    #[cfg(feature = "rune-ui")]
    presentation: GamePresentation {
        icon: include_bytes!("icon.jpg"),
        setup_note: Some(
            "PAK mods go in End/Content/Paks/~mods. Launch game opens the Steam-registered copy.",
        ),
        destination_override: false,
    },
};
const STEAM_APP_ID: u32 = 1_462_040;
const PAKS: &str = "End/Content/Paks";
const MODS: &str = "End/Content/Paks/~mods";

struct Ff7Remake;

impl GameAdapter for Ff7Remake {
    fn detect(&self) -> Result<Vec<PathBuf>> {
        let mut roots = detect_steam_app(STEAM_APP_ID)?;
        roots.retain(|root| root.join(PAKS).is_dir());
        Ok(roots)
    }

    fn launch(&self, _context: LaunchContext<'_>) -> Result<LaunchCommand> {
        Ok(LaunchCommand::steam(STEAM_APP_ID))
    }

    fn plan(
        &self,
        files: &[PathBuf],
        _slug: &str,
        destination: Option<&Path>,
    ) -> Result<InstallPlan> {
        ensure!(
            destination.is_none(),
            "FF7 Remake PAK mods must go in {MODS}; remove the destination override"
        );
        let mut plan = InstallPlan {
            placement: Placement::Inferred,
            ..Default::default()
        };
        let mut package_dir = None;
        for source in files {
            let path = portable_path(source)?;
            let (parent, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
            let file = Path::new(name);
            if !is_pak(file) {
                if file.extension().is_some_and(|ext| {
                    ["dll", "asi", "exe", "ini", "utoc", "ucas"]
                        .iter()
                        .any(|kind| ext.eq_ignore_ascii_case(kind))
                }) {
                    bail!("FF7 Remake only supports standalone PAK mods; unsupported file: {path}");
                }
                plan.warnings.push(format!("Skipped non-PAK file: {path}"));
                continue;
            }
            if let Some(first) = &package_dir {
                ensure!(
                    first == parent,
                    "PAK files are in multiple folders; extract and import only the variant or package you want"
                );
            } else {
                package_dir = Some(parent.to_owned());
                let mut components = parent.rsplit('/');
                let has_layout = MODS.rsplit('/').all(|expected| {
                    components
                        .next()
                        .is_some_and(|part| part.eq_ignore_ascii_case(expected))
                });
                if has_layout {
                    plan.placement = Placement::Layout;
                }
            }
            plan.files.push(FileMapping {
                source: source.clone(),
                destination: Path::new(MODS).join(file),
            });
        }
        ensure!(
            !plan.files.is_empty(),
            "FF7 Remake mod contains no .pak files"
        );
        validate_plan(plan)
    }

    fn validate_registration_root(&self, root: &Path) -> Result<()> {
        ensure!(
            root.join(PAKS).is_dir(),
            "select the FF7 Remake installation folder containing {PAKS}"
        );
        Ok(())
    }

    fn unpack_mod(
        &self,
        input: &Path,
        scratch: &Path,
        filename: Option<&str>,
    ) -> Result<ArchiveTree> {
        if let Some(name) = filename.map(OsStr::new).or_else(|| input.file_name())
            && is_pak(Path::new(name))
            && !input.is_dir()
        {
            return copy_standalone(input, scratch, name);
        }
        unpack(input, scratch)
    }
}

fn is_pak(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pak"))
}
