use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::{sync::mpsc::Receiver, time::Duration};

#[path = "../games/xxmi/setup_view.rs"]
mod xxmi_setup;

use dmm::games::{GameKind, validate_installation_root};
#[cfg(target_os = "linux")]
use dmm::games::xxmi::steam_xxmi::{InstallStage, SteamXxmi};
use rune::tok::Theme;
use rune::{El, FileDialogOptions, TextEdit, col, row};
#[cfg(target_os = "linux")]
use rune::Tx;
use rune_kit::{Kit, RadioGroup, RadioItem};

use super::components::{button, form_field, heading, muted, panel};
use super::installations::{self, Installation};
use super::{Snapshot, State, Update};

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
    #[cfg(target_os = "linux")]
    {
        body = body.child(super::nexus_handler::setup_panel(s, t));
    }
    #[cfg(not(target_os = "linux"))]
    {
        body = body.child(muted("Automatic nxm:// link registration is not available on this platform. Use Downloads to paste the link manually.", t).wrap());
    }
    if let Some(note) = kind.presentation().setup_note {
        body = body.child(muted(note, t).wrap());
    }
    if kind.is_xxmi() {
        body = xxmi_setup::view(body, s, t, k, any);
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
pub(super) fn overlays(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) {
    super::nexus_handler::overlay(s, t, k, layer);
    xxmi_setup::overlay(s, t, k, layer);
}

fn manual_form(s: &State, t: Theme, k: &Kit) -> El {
    let kind = s.game_kind();
    let place = kind.installation_label();
    panel(t).child(heading("Add installation manually", t))
        .child(form_field("setup:name", "Installation label", &s.setup.name, k, t, |s| &mut s.setup.name))
        .child(form_field("setup:root", place, &s.setup.root, k, t, |s| &mut s.setup.root))
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

fn register_game(s: &mut State, kind: GameKind) {
    let Some(root) = path(&s.setup.root) else {
        return;
    };
    let name = s.setup.name.text.trim().to_owned();
    let steam_roots = s.steam_games.iter().filter(|game| game.kind == kind)
        .map(|game| game.root.clone()).collect::<Vec<_>>();
    s.submit("Adding installation…", move |dir| {
        let root = root.canonicalize()?;
        if kind.is_xxmi() {
            validate_installation_root(kind, &root, &steam_roots)?;
        }
        let library = super::open_library(&dir)?;
        library.register(&name, kind, &root)?;
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
