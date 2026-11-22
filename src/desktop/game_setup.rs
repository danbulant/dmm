use std::path::PathBuf;

use dmm::games::{GameKind, is_zzmi_mods};
use dmm::library::{LaunchSettings, Library};
use rune::tok::Theme;
use rune::{El, FileDialogOptions, TextEdit, col, row};
use rune_kit::{Kit, RadioGroup, RadioItem};

use super::installations::{self, Installation};
use super::{Snapshot, State, Update, button, field, game_name, heading, muted, panel};

#[derive(Default)]
pub(super) struct SetupForm {
    manual: bool,
    launch: bool,
    name: TextEdit,
    root: TextEdit,
    descriptor: TextEdit,
    xxmi: TextEdit,
    wine_prefix: TextEdit,
}

pub(super) fn view(s: &State, t: Theme, k: &Kit) -> El {
    let kind = s.game_kind();
    let title = game_name(kind);
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
            Installation::Detected(_) if kind == GameKind::Zzmi => {
                "Detected ZZMI Mods folder".into()
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
                "No usable installations found. Add a folder manually or rescan.",
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
    match kind {
        GameKind::Cyberpunk => {
            body = body.child(muted("Launch game opens the Steam-registered copy. The selected folder determines where DMM deploys mods.", t).wrap());
        }
        GameKind::Zzmi => {
            body = body.child(
                muted(
                    "Mods go in the ZZMI importer's Mods folder, not the vanilla game folder.",
                    t,
                )
                .wrap(),
            );
            for root in &s.zzz_steam_roots {
                body = body.child(muted(format!("Steam game: {}", root.display()), t).wrap());
            }
            if !any {
                body = body.child(button("setup:xxmi-guide", "Set up XXMI ↗", t, true, true,
                    |s, tx| super::browse::open_page(s, tx, "https://github.com/SpectrumQT/XXMI-Launcher#installation")))
                    .child(muted(if cfg!(target_os = "windows") {
                        "Run the official installer, install the Zenless Zone Zero importer in XXMI, then rescan."
                    } else {
                        "Use the official portable release with Wine 9.22+ and the Visual C++ runtime. Install the Zenless Zone Zero importer in XXMI, then rescan. Use the same Wine environment for launches."
                    }, t).wrap());
            }
            if s.snapshot.selected.is_some() {
                body = body.child(launch_settings(s, t));
            }
        }
        GameKind::Generic => {
            body =
                body.child(muted("Launch a custom game with a command using the CLI.", t).wrap());
        }
    }
    if s.setup.manual {
        body = body.child(manual_form(s, t));
    }
    body.children(
        s.discovery_notices
            .iter()
            .filter(|(game, _)| *game == kind)
            .map(|(_, notice)| muted(notice, t).wrap()),
    )
}

fn manual_form(s: &State, t: Theme) -> El {
    let kind = s.game_kind();
    let place = if kind == GameKind::Zzmi {
        "ZZMI Mods folder"
    } else {
        "Game installation folder"
    };
    panel(t).child(heading("Add installation manually", t))
        .child(field("setup:name", "Installation label", &s.setup.name, t, |s| &mut s.setup.name))
        .child(field("setup:root", place, &s.setup.root, t, |s| &mut s.setup.root))
        .child(field("setup:descriptor", "Optional game profile TOML", &s.setup.descriptor, t, |s| &mut s.setup.descriptor))
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

fn launch_settings(s: &State, t: Theme) -> El {
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
    body = body.child(field(
        "setup:xxmi",
        "XXMI Launcher.exe (blank: detect)",
        &s.setup.xxmi,
        t,
        |s| &mut s.setup.xxmi,
    ));
    if cfg!(target_os = "linux") {
        body = body.child(field(
            "setup:wine-prefix",
            "WINEPREFIX (blank: detect)",
            &s.setup.wine_prefix,
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
                let library = Library::open(&dir)?;
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
    let steam_roots = if kind == GameKind::Zzmi {
        s.zzz_steam_roots.clone()
    } else {
        Vec::new()
    };
    s.submit("Adding installation…", move |dir| {
        let root = root.canonicalize()?;
        if steam_roots.iter().any(|steam| root.starts_with(steam)) {
            anyhow::ensure!(
                is_zzmi_mods(&root),
                "Choose the ZZMI Mods folder, not the Steam game installation."
            );
        }
        let library = Library::open(&dir)?;
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
