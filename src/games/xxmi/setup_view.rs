#[cfg(target_os = "linux")]
use dmm::games::GameKind;
#[cfg(target_os = "linux")]
use dmm::games::xxmi::steam_xxmi::{InstallStage, SteamXxmi};
use dmm::library::LaunchSettings;
use rune::tok::Theme;
use rune::{El, col};
#[cfg(target_os = "linux")]
use rune::{Tx, el, row};
use rune_kit::Kit;
#[cfg(target_os = "linux")]
use rune_kit::{Dialog, Loader};
#[cfg(target_os = "linux")]
use super::SteamDialog;

use super::super::components::{button, form_field, heading, muted, open_page, panel};
use super::super::{Snapshot, State, Update, XxmiLaunch, open_library};
use super::{edit_path, path};

pub(super) fn view(mut body: El, s: &State, t: Theme, k: &Kit, any: bool) -> El {
    let kind = s.game_kind();
    let title = kind.display_name();
    let ready = s.busy.is_none();
    for game in s.steam_games.iter().filter(|game| game.kind == kind) {
        body = body.child(muted(format!("Steam game: {}", game.root.display()), t).wrap());
    }
    #[cfg(target_os = "linux")]
    let steam_setup_available = s.steam_xxmi.iter().any(|target| target.kind() == kind);
    #[cfg(not(target_os = "linux"))]
    let steam_setup_available = false;
    #[cfg(target_os = "linux")]
    if steam_setup_available {
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
        if !steam_setup_available {
            body = body.child(button(
                "setup:start-xxmi",
                s.xxmi_launch.label("Start XXMI"),
                t,
                false,
                ready && !s.xxmi_launch.active(),
                |s, _| s.open_xxmi(),
            ));
        }
    }
    if !steam_setup_available {
        body = body.child(xxmi_feedback(s, t));
    }
    body
}

fn xxmi_feedback(s: &State, t: Theme) -> El {
    let mut body = col().gap(6.0);
    if matches!(s.xxmi_launch, XxmiLaunch::Running(..))
        && cfg!(any(target_os = "linux", target_os = "windows"))
    {
        body = body.child(button(
            "setup:close-xxmi",
            if s.busy == Some("Closing XXMI…") { "Closing XXMI…" } else { "Close XXMI" },
            t,
            false,
            s.busy.is_none(),
            |s, _| s.close_xxmi(),
        ));
    }
    if let XxmiLaunch::Failed(error) = &s.xxmi_launch {
        body = body.child(muted(error.clone(), t).wrap());
    }
    body
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
    let selected = s.snapshot.selected.as_ref();
    let launcher = s.steam_xxmi.iter().filter(|target| target.installed).min_by_key(|target| {
        (
            !selected.is_some_and(|instance| instance.root.starts_with(&target.compatdata)),
            target.kind() != kind,
        )
    });
    let prefer_selected = selected.is_some_and(|instance| {
        s.snapshot.launch.xxmi.is_some()
            || s.snapshot.launch.wine_prefix.is_some()
            || launcher.is_none_or(|target| !instance.root.starts_with(&target.compatdata))
    });
    if let Some(target) = launcher.filter(|_| !prefer_selected) {
        let reused = target.kind() != kind;
        let launcher = target.clone();
        let label = if reused { "Start existing XXMI" } else { "Start XXMI" };
        body = body.child(button(
            "setup:steam-start", s.xxmi_launch.label(label), t, true,
            available && !s.xxmi_launch.active(),
            move |s, _| s.open_steam_xxmi(launcher.clone()),
        )).child(muted(format!("Install the {title} importer in XXMI, then rescan to find its Mods folder."), t).wrap());
        if reused {
            for (index, game) in s.steam_xxmi.iter().enumerate().filter(|(_, game)| game.kind() == kind) {
                body = body.child(xxmi_game_folder(index, game, target, t));
            }
        }
    } else if selected.is_some() || s.xxmi_launch.active() {
        body = body.child(button(
            "setup:steam-start", s.xxmi_launch.label("Start XXMI"), t, true,
            s.busy.is_none() && !s.xxmi_launch.active(),
            |s, _| s.open_xxmi(),
        ));
    }
    body = body.child(xxmi_feedback(s, t));
    for (index, target) in s.steam_xxmi.iter().enumerate().filter(|(_, target)| target.kind() == kind) {
        let mut game = col().gap(8.0)
            .child(muted(format!("Proton prefix: {}", target.compatdata.join("pfx").display()), t).wrap());
        game = game.child(xxmi_game_folder(index, target, target, t));
        if let Some(option) = target.steam_play_option() {
            game = game
                .child(muted(option.instructions[0], t).wrap())
                .child(button(
                    format!("setup:copy-steam-play:{index}"),
                    "Copy Steam launch option",
                    t,
                    false,
                    true,
                    move |s, tx| {
                        tx.copy(option.command.clone());
                        s.notice = "Steam launch option copied.".into();
                    },
                ))
                .child(muted(option.instructions[1], t).wrap());
        }
        if !target.installed && !target.initialized {
            game = game.child(muted(format!("Launch {title} once in Steam with Proton to create its prefix, then close it and rescan."), t).wrap());
        } else if !target.installed {
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
    let reused = launcher.kind() != game.kind();
    let label = if reused {
        "Game Folder (existing XXMI)"
    } else {
        "Game Folder (XXMI)"
    };
    let path = match game.windows_game_folder(launcher) {
        Ok(path) => path,
        Err(error) => return muted(format!("{label}: {error}"), t).wrap(),
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
pub(super) fn overlay(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) {
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
                let library = open_library(&dir)?;
                library.set_launch_settings(&name, &settings)?;
                Ok(Update::Library(Snapshot::load(&library, Some(&name))?))
            });
        },
    ))
}

