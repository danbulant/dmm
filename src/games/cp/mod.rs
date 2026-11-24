use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

#[cfg(feature = "rune-ui")]
use super::GamePresentation;
use super::discovery::detect_steam_app;
use super::{
    FileMapping, GameAdapter, GameProfile, InstallPlan, LaunchCommand, LaunchContext, Placement,
    common_wrapper, normalized_paths, strip_wrapper, validate_plan,
};
use crate::archive::portable_path;
use crate::sources::Source;

pub(super) const PROFILE: GameProfile = GameProfile {
    id: "cyberpunk",
    aliases: &["cyberpunk2077"],
    adapter: &Cyberpunk,
    name: "Cyberpunk 2077",
    nexus: Some("cyberpunk2077"),
    gamebanana: Some("8722"),
    preferred_source: Source::Nexus,
    installation_label: "Game installation folder",
    #[cfg(feature = "rune-ui")]
    presentation: GamePresentation {
        icon: include_bytes!("icon.png"),
        setup_note: Some(
            "Launch game opens the Steam-registered copy. The selected folder determines where DMM deploys mods.",
        ),
        destination_override: true,
    },
};

const STEAM_APP_ID: u32 = 1_091_500;
const RECOGNIZED_ROOTS: [&str; 6] = ["archive", "r6", "bin", "red4ext", "mods", "engine"];

struct Cyberpunk;

impl GameAdapter for Cyberpunk {
    fn detect(&self) -> Result<Vec<PathBuf>> {
        detect_steam_app(STEAM_APP_ID)
    }

    fn launch(&self, _context: LaunchContext<'_>) -> Result<LaunchCommand> {
        Ok(LaunchCommand::steam(STEAM_APP_ID))
    }

    fn plan(
        &self,
        files: &[PathBuf],
        slug: &str,
        override_dir: Option<&Path>,
    ) -> Result<InstallPlan> {
        let mut paths = normalized_paths(files)?;
        let wrapper = common_wrapper(
            paths
                .iter()
                .filter(|(_, path)| !documentation(path))
                .map(|(_, path)| path.as_str()),
        )
        .filter(|wrapper| {
            !RECOGNIZED_ROOTS
                .iter()
                .any(|root| root.eq_ignore_ascii_case(wrapper))
        });
        for (_, path) in &mut paths {
            let relative = strip_wrapper(path, wrapper.as_deref());
            if relative.len() != path.len() {
                path.drain(..path.len() - relative.len());
            }
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
                    RECOGNIZED_ROOTS
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
                    RECOGNIZED_ROOTS
                        .into_iter()
                        .find(|root| head.eq_ignore_ascii_case(root))
                        .map(|root| (root, rest))
                })
            } else {
                None
            };
            if rooted.is_none() && cet_root.is_none() && redmod_root.is_none() && documentation(&path)
            {
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
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ProtonDiagnostics {
    pub red4ext_directory: bool,
    pub cet_directory: bool,
    pub wine_dll_overrides: Option<String>,
    pub note: &'static str,
}

pub fn proton_diagnostics(root: &Path) -> Option<ProtonDiagnostics> {
    #[cfg(target_os = "linux")]
    {
        Some(ProtonDiagnostics {
            red4ext_directory: root.join("red4ext").is_dir(),
            cet_directory: root.join("bin/x64/plugins/cyber_engine_tweaks").is_dir(),
            wine_dll_overrides: std::env::var("WINEDLLOVERRIDES").ok(),
            note: "Shell overrides do not reflect Steam launch options. Consult each framework's Wine/Proton setup instructions. DMM does not change these settings.",
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{Path, PathBuf, strip_prefix_ci};
    use crate::games::{GameKind, plan};

    #[test]
    fn cyberpunk_preserves_game_root_and_strips_wrapper_once() {
        let files = [
            PathBuf::from("Release/archive/pc/mod/body.archive"),
            PathBuf::from("Release/r6/tweaks/body.yaml"),
        ];
        let plan = plan(GameKind::Cyberpunk, &files, "body", None).unwrap();
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
        let files = [
            "pack.archive",
            "pack.xl",
            "script.reds",
            "config.yaml",
            "settings.tweak",
        ]
        .map(PathBuf::from);
        let inferred = plan(GameKind::Cyberpunk, &files, "mod", None).unwrap();
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
        assert!(plan(GameKind::Cyberpunk, &[PathBuf::from("plugin.dll")], "mod", None).is_err());
        assert!(
            plan(
                GameKind::Cyberpunk,
                &[PathBuf::from("plugin.dll")],
                "mod",
                Some(Path::new("bin/x64/plugins"))
            )
            .is_ok()
        );
    }

    #[test]
    fn cyberpunk_detects_cet_and_redmod_package_layouts() {
        let cet = [
            PathBuf::from("Ellen/init.lua"),
            PathBuf::from("Ellen/scripts/main.lua"),
        ];
        let cet_plan = plan(GameKind::Cyberpunk, &cet, "ellen", None).unwrap();
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
        let plan = plan(GameKind::Cyberpunk, &redmod, "package", None).unwrap();
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
