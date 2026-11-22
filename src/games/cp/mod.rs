use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use super::discovery::detect_steam_app;
use super::{
    FileMapping, GameKind, GameProfile, InstallPlan, LaunchCommand, Placement, common_wrapper,
    normalized_paths, profile, strip_wrapper, validate_plan,
};
use crate::archive::portable_path;

pub(super) const PROFILE: &str = include_str!("profile.toml");

pub(super) fn detect() -> Result<Vec<PathBuf>> {
    let app_id = profile(GameKind::Cyberpunk)?
        .settings
        .steam_app_id
        .context("Cyberpunk profile has no Steam AppID")?;
    detect_steam_app(app_id)
}

pub(super) fn launch(profile: &GameProfile) -> Result<LaunchCommand> {
    let url = profile
        .settings
        .steam_url
        .as_deref()
        .context("Cyberpunk profile has no Steam URL")?;
    Ok(LaunchCommand {
        program: OsString::from(url),
        args: Vec::new(),
        cwd: None,
        env: Vec::new(),
        url: true,
    })
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
    use super::{GameKind, Path, PathBuf, profile, strip_prefix_ci};
    use crate::games::plan;

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
}

pub(super) fn plan(
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
