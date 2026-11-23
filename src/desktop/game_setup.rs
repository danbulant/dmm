use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::{fs, path::Path, sync::mpsc::Receiver, time::Duration};

use dmm::games::GameKind;
#[cfg(target_os = "linux")]
use dmm::games::xxmi::steam_xxmi::{InstallStage, SteamXxmi};
use dmm::games::xxmi::validate_installation_root;
use dmm::library::LaunchSettings;
use rune::tok::Theme;
use rune::{El, FileDialogOptions, TextEdit, col, row};
#[cfg(target_os = "linux")]
use rune::{Tx, el};
#[cfg(target_os = "linux")]
use rune_kit::{Dialog, Loader};
use rune_kit::{Kit, RadioGroup, RadioItem};

use super::components::{button, form_field, heading, muted, open_page, panel};
use super::installations::{self, Installation};
use super::{Snapshot, State, Update, XxmiLaunch};

#[derive(Default)]
pub(super) struct SetupForm {
    manual: bool,
    launch: bool,
    #[cfg(target_os = "linux")]
    pub(super) steam_dialog: Option<SteamDialog>,
    #[cfg(target_os = "linux")]
    pub(super) nexus_handler_dialog: Option<super::nexus_handler::HandlerDialog>,
    name: TextEdit,
    root: TextEdit,
    descriptor: TextEdit,
    xxmi: TextEdit,
    wine_prefix: TextEdit,
}

#[cfg(target_os = "linux")]
pub(super) enum SteamDialog {
    Confirm(SteamXxmi),
    Running {
        stage: InstallStage,
        progress: Receiver<InstallStage>,
    },
    Complete(Result<SteamXxmi, String>),
}

#[cfg(target_os = "linux")]
impl SetupForm {
    pub(super) fn poll_steam_dialog(&mut self, busy: bool, notice: &str, tx: &mut Tx) -> bool {
        let Some(SteamDialog::Running { stage, progress }) = &mut self.steam_dialog else {
            return false;
        };
        if !busy {
            self.steam_dialog = Some(SteamDialog::Complete(Err(notice.to_owned())));
            return true;
        }
        tx.wake_after(Duration::from_millis(100));
        if let Some(next) = progress.try_iter().last() {
            *stage = next;
            true
        } else {
            false
        }
    }
}

pub(super) fn view(s: &State, t: Theme, k: &Kit) -> El {
    let kind = s.game_kind();
    let title = kind.display_name();
    let ready = s.busy.is_none();
    let mut body = col().gap(14.0).child(
        row()
            .w_full()
            .items_center()
            .gap(8.0)
            .child(heading(format!("{title} setup"), t).grow(1.0).shrink())
            .child(button("setup:detect", "Rescan", t, false, ready, |s, _| {
                s.detect_games()
            }))
            .child(
                button(
                    "setup:add",
                    if s.setup.manual {
                        "Cancel"
                    } else {
                        "+ Add manually"
                    },
                    t,
                    s.setup.manual,
                    ready,
                    move |s, _| {
                        s.setup.manual = !s.setup.manual;
                        if s.setup.manual && s.setup.name.text.is_empty() {
                            s.setup.name = TextEdit::new(kind.to_string());
                        }
                    },
                )
                .expanded(s.setup.manual),
            ),
    );
    let mut radios = RadioGroup::new()
        .label("Main installation")
        .disabled(!ready);
    let mut any = false;
    for item in installations::candidates(&s.snapshot.instances, &s.detected, kind) {
        any = true;
        let root = item.root().to_owned();
        let checked = s
            .snapshot
            .selected
            .as_ref()
            .is_some_and(|current| current.root == root);
        let label = match item {
            Installation::Registered { instance, detected } => {
                if detected {
                    format!("{} · Detected", instance.name)
                } else {
                    instance.name.clone()
                }
            }
            Installation::Detected(_) if kind.is_xxmi() => {
                format!("Detected {title} Mods folder")
            }
            Installation::Detected(_) => format!("Detected {title}"),
        };
        radios = radios.item(
            RadioItem::new(label, checked)
                .description(root.display().to_string())
                .on_select(move |s: &mut State| {
                    if !checked {
                        s.use_installation(root.clone());
                    }
                }),
        );
    }
    if any {
        body = body.child(radios.build(k));
        if s.snapshot.selected.is_none() && ready {
            body = body.child(
                muted(
                    "Choose the main installation to use for this game's mods and launch settings.",
                    t,
                )
                .wrap(),
            );
        }
    } else if ready {
        body = body.child(
            muted(
                if s.steam_games.iter().any(|game| game.kind == kind) {
                    "Steam game detected. Install its importer in XXMI, then rescan to find the Mods folder."
                } else {
                    "No usable installations found. Add a folder manually or rescan."
                },
                t,
            )
            .wrap(),
        );
    }
    if let Some(instance) = &s.snapshot.selected {
        let profile = instance
            .profile
            .as_ref()
            .map_or_else(|| "Built-in profile".into(), |p| p.display().to_string());
        body = body.child(muted(format!("Profile: {profile}"), t).wrap());
        if let Some(Err(error)) = s.snapshot.profiles.get(&instance.name) {
            body = body.child(muted(format!("Profile error: {error}"), t).wrap());
        }
    }
    #[cfg(target_os = "linux")]
    {
        body = body.child(super::nexus_handler::setup_panel(s, t));
    }
    #[cfg(not(target_os = "linux"))]
    {
        body = body.child(muted("Automatic nxm:// link registration is not available on this platform. Use Downloads to paste the link manually.", t).wrap());
    }
    match kind {
        GameKind::Cyberpunk => {
            body = body.child(muted("Launch game opens the Steam-registered copy. The selected folder determines where DMM deploys mods.", t).wrap());
        }
        GameKind::Zzmi | GameKind::Efmi => {
            for game in s.steam_games.iter().filter(|game| game.kind == kind) {
                body = body.child(muted(format!("Steam game: {}", game.root.display()), t).wrap());
            }
            #[cfg(target_os = "linux")]
            if s.steam_xxmi.iter().any(|target| target.kind() == kind) {
                body = body.child(steam_setup(s, t, kind));
            }
            if !any {
                body = body.child(button("setup:xxmi-guide", "Set up XXMI ↗", t, true, true,
                    |s, tx| open_page(s, tx, "https://github.com/SpectrumQT/XXMI-Launcher#installation")))
                    .child(muted(if cfg!(target_os = "windows") {
                        format!("Run the official installer, install the {title} importer in XXMI, then rescan.")
                    } else {
                        format!("Use the official portable release with Wine 9.22+ and the Visual C++ runtime. Install the {title} importer in XXMI, then rescan. Use the same Wine environment for launches.")
                    }, t).wrap());
            }
            if s.snapshot.selected.is_some() {
                body = body.child(launch_settings(s, t, k));
                body = body.child(button(
                    "setup:start-xxmi",
                    s.xxmi_launch.label("Start XXMI for this installation"),
                    t,
                    false,
                    ready && !s.xxmi_launch.active(),
                    |s, _| s.open_xxmi(),
                ));
            }
            if matches!(s.xxmi_launch, XxmiLaunch::Running(..))
                && cfg!(any(target_os = "linux", target_os = "windows"))
            {
                body = body.child(button(
                    "setup:close-xxmi",
                    "Close XXMI",
                    t,
                    false,
                    ready,
                    |s, _| s.close_xxmi(),
                ));
            }
            if let XxmiLaunch::Failed(error) = &s.xxmi_launch {
                body = body.child(muted(error.clone(), t).wrap());
            }
        }
        GameKind::Generic => {
            body =
                body.child(muted("Launch a custom game with a command using the CLI.", t).wrap());
        }
    }
    if s.setup.manual {
        body = body.child(manual_form(s, t, k));
    }
    body.children(
        s.discovery_notices
            .iter()
            .filter(|(game, _)| *game == kind)
            .map(|(_, notice)| muted(notice, t).wrap()),
    )
}

#[cfg(target_os = "linux")]
fn steam_setup(s: &State, t: Theme, kind: GameKind) -> El {
    let mut body = panel(t).child(heading("Steam Proton setup", t));
    if s.protontricks.is_none() {
        body = body.child(muted("Install Protontricks, then rescan. DMM uses it to select Steam’s Proton runtime rather than creating a Wine prefix.", t).wrap())
            .child(button("setup:protontricks", "Get Protontricks ↗", t, false, true, |s, tx| {
                open_page(s, tx, "https://github.com/Matoking/protontricks#installation")
            }));
    }
    let available = s.busy.is_none() && s.protontricks.is_some();
    let title = kind.display_name();
    let has_launcher = s.steam_xxmi.iter().any(|target| target.kind() == kind && target.installed);
    if !has_launcher
        && let Some(target) = s.steam_xxmi.iter().find(|target| target.kind() != kind && target.installed)
    {
        let existing = target.clone();
        body = body.child(button(
            "setup:steam-reuse", s.xxmi_launch.label("Start existing XXMI"), t, true,
            available && !s.xxmi_launch.active(),
            move |s, _| s.open_steam_xxmi(existing.clone()),
        )).child(muted(format!("Install the {title} importer in XXMI, then rescan. Reusing this launcher runs the importer in the existing Proton prefix."), t).wrap());
        for (index, game) in s.steam_xxmi.iter().enumerate().filter(|(_, game)| game.kind() == kind) {
            body = body.child(xxmi_game_folder(index, game, &target, t));
        }
    }
    for (index, target) in s.steam_xxmi.iter().enumerate().filter(|(_, target)| target.kind() == kind) {
        let mut game = col().gap(8.0)
            .child(muted(format!("Proton prefix: {}", target.compatdata.join("pfx").display()), t).wrap());
        game = game.child(xxmi_game_folder(index, target, target, t));
        if target.installed {
            let target = target.clone();
            game = game.child(button(format!("setup:steam-start:{index}"),
                s.xxmi_launch.label("Start XXMI"), t, true,
                available && !s.xxmi_launch.active(),
                move |s, _| s.open_steam_xxmi(target.clone())))
                .child(muted(format!("Install the {title} importer in XXMI, then rescan to find its Mods folder."), t).wrap());
        } else if !target.initialized {
            game = game.child(muted(format!("Launch {title} once in Steam with Proton to create its prefix, then close it and rescan."), t).wrap());
        } else {
            let target = target.clone();
            game = game.child(button(
                format!("setup:steam-install:{index}"),
                "Set up XXMI for Steam…",
                t,
                true,
                available,
                move |s, _| s.setup.steam_dialog = Some(SteamDialog::Confirm(target.clone())),
            ));
        }
        body = body.child(game);
    }
    body
}

#[cfg(target_os = "linux")]
fn xxmi_game_folder(index: usize, game: &SteamXxmi, launcher: &SteamXxmi, t: Theme) -> El {
    let executables: &[&str] = match game.kind() {
        GameKind::Efmi => &["Endfield.exe"],
        GameKind::Zzmi => &[
            "ZenlessZoneZero.exe",
            "games/ZenlessZoneZero Game/ZenlessZoneZero.exe",
        ],
        GameKind::Cyberpunk | GameKind::Generic => return col(),
    };
    let Some(folder) = executables.iter().find_map(|exe| {
        let path = game.game.join(exe);
        path.parent().filter(|_| path.is_file()).map(Path::to_owned)
    }) else {
        return muted("XXMI game folder unavailable: the game executable was not found.", t).wrap();
    };
    let reused = launcher.kind() != game.kind();
    let label = if reused {
        "Game Folder (existing XXMI)"
    } else {
        "Game Folder (XXMI)"
    };
    let Some(path) = windows_game_folder(&launcher.compatdata.join("pfx"), &folder) else {
        return muted(format!("{label}: no Proton drive maps to {}", folder.display()), t).wrap();
    };
    col().gap(4.0)
        .child(muted(format!("{label}: {path}"), t).wrap().selectable(true))
        .child(button(
            format!("setup:steam-copy:{index}:{reused}"),
            "Copy game folder",
            t,
            false,
            true,
            move |_: &mut State, tx| tx.copy(path.clone()),
        ))
}

#[cfg(target_os = "linux")]
fn windows_game_folder(prefix: &Path, folder: &Path) -> Option<String> {
    let folder = folder.canonicalize().ok()?;
    let mappings = fs::read_dir(prefix.join("dosdevices")).ok()?;
    mappings
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let [letter, b':'] = name.as_encoded_bytes() else {
                return None;
            };
            if !letter.is_ascii_alphabetic() {
                return None;
            }
            let root = entry.path().canonicalize().ok()?;
            let relative = folder.strip_prefix(root).ok()?;
            Some((relative, letter.to_ascii_uppercase()))
        })
        .min_by(|(a, drive_a), (b, drive_b)| {
            a.as_os_str()
                .len()
                .cmp(&b.as_os_str().len())
                .then(drive_a.cmp(drive_b))
        })
        .map(|(relative, drive)| {
            format!(
                "{}:\\{}",
                char::from(drive),
                relative.to_string_lossy().replace('/', "\\")
            )
        })
}

#[cfg(target_os = "linux")]
pub(super) fn overlays(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) {
    super::nexus_handler::overlay(s, t, k, layer);
    let Some(dialog) = &s.setup.steam_dialog else {
        return;
    };
    let (title, body) = match dialog {
        SteamDialog::Confirm(target) => {
            let target = target.clone();
            ("Set up XXMI for Steam", col().gap(12.0)
                .child(muted(format!("Close {} and XXMI first. DMM will download the official portable launcher and install the x64 Visual C++ runtime in this Proton prefix:", target.kind().display_name()), t).wrap())
                .child(muted(target.compatdata.join("pfx").display().to_string(), t).wrap())
                .child(row().gap(8.0).flex_wrap().justify_end()
                    .child(button("xxmi-setup-cancel", "Cancel", t, false, true, close_steam_setup_tx))
                    .child(button("xxmi-setup-install", "Install", t, true, s.busy.is_none(),
                        move |s, _| s.install_steam_xxmi(target.clone())))))
        }
        SteamDialog::Running { stage, .. } => {
            let label = match stage {
                InstallStage::Preparing => "Checking the Proton prefix…",
                InstallStage::DownloadingLauncher => "Downloading and verifying the XXMI launcher…",
                InstallStage::UnpackingLauncher => "Extracting the XXMI launcher…",
                InstallStage::DownloadingRuntime => "Downloading and verifying Visual C++…",
                InstallStage::InstallingRuntime => "Installing Visual C++ in Proton…",
                InstallStage::SavingLauncher => "Saving the portable launcher…",
            };
            let panel = col()
                .key("xxmi-setup-dialog")
                .role("dialog")
                .label("Installing XXMI")
                .w_full()
                .max_w(500.0)
                .padding(16.0)
                .gap(14.0)
                .radius(8.0)
                .fill(k.c.layer_01)
                .modal(false, |_: &mut State| {})
                .child(heading("Installing XXMI", t))
                .child(
                    row()
                        .key("xxmi-setup-progress")
                        .role("status")
                        .gap(8.0)
                        .items_center()
                        .child(Loader::new().build(k))
                        .child(muted(label, t).wrap()),
                )
                .child(muted("Keep DMM open until setup finishes.", t).wrap());
            layer.push(
                el().key("xxmi-setup-busy-scrim")
                    .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                    .fill(k.c.scrim)
                    .a11y_hidden(),
            );
            layer.push(
                el().key("xxmi-setup-layer")
                    .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                    .padding(16.0)
                    .center()
                    .child(k.elevate(panel, &k.elevation.overlay)),
            );
            return;
        }
        SteamDialog::Complete(Ok(target)) => {
            let target = target.clone();
            ("XXMI installed", col().gap(12.0)
                .child(muted(format!("Start XXMI, install the {} importer, then rescan in Game setup.", target.kind().display_name()), t).wrap())
                .child(row().gap(8.0).justify_end()
                    .child(button("xxmi-setup-done", "Done", t, false, true, close_steam_setup_tx))
                    .child(button("xxmi-setup-start", s.xxmi_launch.label("Start XXMI"), t, true,
                        s.busy.is_none() && !s.xxmi_launch.active() && s.protontricks.is_some(),
                        move |s, _| {
                            s.setup.steam_dialog = None;
                            s.open_steam_xxmi(target.clone());
                        }))))
        }
        SteamDialog::Complete(Err(error)) => (
            "XXMI setup failed",
            col()
                .gap(12.0)
                .child(muted(error, t).wrap())
                .child(row().justify_end().child(button(
                    "xxmi-setup-done",
                    "Close",
                    t,
                    true,
                    true,
                    close_steam_setup_tx,
                ))),
        ),
    };
    Dialog::new("xxmi-setup", title)
        .open(true)
        .fit()
        .width(500.0)
        .click_outside(false)
        .on_close(|s: &mut State| s.setup.steam_dialog = None)
        .child(body.px(16.0).pb(16.0))
        .build(k, layer);
}

#[cfg(target_os = "linux")]
fn close_steam_setup_tx(s: &mut State, _: &mut Tx) {
    s.setup.steam_dialog = None;
}

fn manual_form(s: &State, t: Theme, k: &Kit) -> El {
    let kind = s.game_kind();
    let place = match kind {
        GameKind::Zzmi => "ZZMI Mods folder",
        GameKind::Efmi => "EFMI Mods folder",
        GameKind::Cyberpunk | GameKind::Generic => "Game installation folder",
    };
    panel(t).child(heading("Add installation manually", t))
        .child(form_field("setup:name", "Installation label", &s.setup.name, k, t, |s| &mut s.setup.name))
        .child(form_field("setup:root", place, &s.setup.root, k, t, |s| &mut s.setup.root))
        .child(form_field("setup:descriptor", "Optional game profile TOML", &s.setup.descriptor, k, t, |s| &mut s.setup.descriptor))
        .child(row().gap(8.0).items_center().flex_wrap()
            .child(button("setup:pick-root", "Choose folder…", t, false, s.busy.is_none(), move |_, tx| {
                tx.open_folder(FileDialogOptions { title: format!("Choose {place}"), ..Default::default() }, move |s: &mut State, result, _| {
                    if s.game_kind() != kind || !s.setup.manual { return; }
                    match result {
                        Ok(Some(path)) => s.setup.root = TextEdit::new(path.display().to_string()),
                        Ok(None) => {},
                        Err(error) => s.notice = format!("Folder chooser failed: {error}. Enter the path manually."),
                    }
                });
            }))
            .child(button("setup:register", "Add and use installation", t, true,
                s.busy.is_none() && !s.setup.name.text.trim().is_empty() && path(&s.setup.root).is_some(),
                move |s, _| register_game(s, kind))))
}

fn launch_settings(s: &State, t: Theme, k: &Kit) -> El {
    let mut body = col().gap(8.0).child(
        button(
            "setup:launch-settings",
            "XXMI launch settings",
            t,
            false,
            true,
            |s, _| {
                s.setup.launch = !s.setup.launch;
                if s.setup.launch {
                    s.setup.xxmi = edit_path(&s.snapshot.launch.xxmi);
                    s.setup.wine_prefix = edit_path(&s.snapshot.launch.wine_prefix);
                }
            },
        )
        .expanded(s.setup.launch),
    );
    if !s.setup.launch {
        return body;
    }
    body = body.child(form_field(
        "setup:xxmi",
        "XXMI Launcher.exe (blank: detect)",
        &s.setup.xxmi,
        k,
        t,
        |s| &mut s.setup.xxmi,
    ));
    if cfg!(target_os = "linux") {
        body = body.child(form_field(
            "setup:wine-prefix",
            "WINEPREFIX (blank: detect)",
            &s.setup.wine_prefix,
            k,
            t,
            |s| &mut s.setup.wine_prefix,
        ));
    }
    body.child(button(
        "setup:save-launch",
        "Save launch settings",
        t,
        true,
        s.busy.is_none(),
        |s, _| {
            let Some(name) = s.snapshot.selected.as_ref().map(|i| i.name.clone()) else {
                return;
            };
            let settings = LaunchSettings {
                xxmi: path(&s.setup.xxmi),
                wine_prefix: path(&s.setup.wine_prefix),
            };
            s.submit("Saving launch settings…", move |dir| {
                let library = super::open_library(&dir)?;
                library.set_launch_settings(&name, &settings)?;
                Ok(Update::Library(Snapshot::load(&library, Some(&name))?))
            });
        },
    ))
}

fn register_game(s: &mut State, kind: GameKind) {
    let Some(root) = path(&s.setup.root) else {
        return;
    };
    let name = s.setup.name.text.trim().to_owned();
    let descriptor = path(&s.setup.descriptor);
    let steam_roots = s.steam_games.iter().filter(|game| game.kind == kind)
        .map(|game| game.root.clone()).collect::<Vec<_>>();
    s.submit("Adding installation…", move |dir| {
        let root = root.canonicalize()?;
        validate_installation_root(kind, &root, &steam_roots)?;
        let library = super::open_library(&dir)?;
        library.register(&name, kind, &root, descriptor.as_deref())?;
        library.use_installation(&name)?;
        Ok(Update::Library(Snapshot::load(&library, Some(&name))?))
    });
}

fn path(edit: &TextEdit) -> Option<PathBuf> {
    let text = edit.text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

fn edit_path(path: &Option<PathBuf>) -> TextEdit {
    TextEdit::new(
        path.as_ref()
            .map_or_else(String::new, |p| p.display().to_string()),
    )
}
