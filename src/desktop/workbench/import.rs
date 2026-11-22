use std::path::PathBuf;

use dmm::library::InstallOptions;
use rune::tok::Theme;
use rune::{El, FileDialogOptions, TextEdit, Tx, col, row};
use rune_kit::{Kit, Switch};

use super::{Snapshot, State, Update, button, heading, muted, nonempty_path, panel};
use crate::desktop::{field, open_library};

pub(super) fn local_import(s: &State, t: Theme, k: &Kit, instance: &str) -> El {
    let mut body = col().gap(8.0).child(heading("Install from local files", t));
    body = body.child(field(
        "local:path",
        "Archive or folder path",
        &s.local.input,
        t,
        |s| &mut s.local.input,
    ));
    body = body.child(field("local:slug", "Mod name", &s.local.slug, t, |s| {
        &mut s.local.slug
    }));
    body = body.child(field(
        "local:destination",
        "Optional destination override",
        &s.local.destination,
        t,
        |s| &mut s.local.destination,
    ));
    body = body.child(
        row()
            .gap(8.0)
            .child(button(
                "local:pick-file",
                "Choose archive",
                t,
                false,
                true,
                |_, tx: &mut Tx| {
                    tx.open_file(
                        FileDialogOptions {
                            title: "Choose mod archive".into(),
                            ..Default::default()
                        },
                        |s: &mut State, result, _| match result {
                            Ok(Some(path)) => {
                                s.local.input = TextEdit::new(path.display().to_string())
                            }
                            Ok(None) => {}
                            Err(error) => {
                                s.notice = format!(
                                    "File chooser failed: {error}. Enter the path manually."
                                )
                            }
                        },
                    );
                },
            ))
            .child(button(
                "local:pick-folder",
                "Choose folder",
                t,
                false,
                true,
                |_, tx: &mut Tx| {
                    tx.open_folder(
                        FileDialogOptions {
                            title: "Choose mod folder".into(),
                            ..Default::default()
                        },
                        |s: &mut State, result, _| match result {
                            Ok(Some(path)) => {
                                s.local.input = TextEdit::new(path.display().to_string())
                            }
                            Ok(None) => {}
                            Err(error) => {
                                s.notice = format!(
                                    "Folder chooser failed: {error}. Enter the path manually."
                                )
                            }
                        },
                    );
                },
            ))
            .child(
                Switch::new(s.preferences.enable_new_mods)
                    .label("Enable after installing")
                    .disabled(s.busy.is_some())
                    .on_toggle(|s: &mut State| {
                        s.set_enable_new_mods(!s.preferences.enable_new_mods);
                    })
                    .build(k),
            )
            .child(button(
                "local:replace-unmanaged",
                if s.local.replace_unmanaged {
                    "Replace unmanaged: yes"
                } else {
                    "Replace unmanaged: no"
                },
                t,
                s.local.replace_unmanaged,
                true,
                |s, _| s.local.replace_unmanaged = !s.local.replace_unmanaged,
            )),
    );
    body = body.child(button(
        "local:preview",
        "Preview install",
        t,
        false,
        s.busy.is_none(),
        move |state, _| {
            let Some(instance) = state.snapshot.selected.as_ref().map(|i| i.name.clone()) else {
                return;
            };
            let input = PathBuf::from(state.local.input.text.trim());
            let slug = state.local.slug.text.trim().to_owned();
            let destination = nonempty_path(&state.local.destination.text);
            if input.as_os_str().is_empty() || slug.is_empty() {
                state.notice = "Choose a local archive or folder and enter a mod name.".into();
                return;
            }
            state.local.begin_preview(instance.clone());
            state.submit("Inspecting local mod", move |root| {
                let library = open_library(&root)?;
                Ok(Update::LocalPlan(library.inspect(
                    &instance,
                    &slug,
                    &input,
                    destination.as_deref(),
                )?))
            });
        },
    ));
    if let Some(preview) = s
        .local
        .preview
        .as_ref()
        .filter(|preview| preview.instance == instance && s.local.preview_matches())
    {
        body = body.child(muted(
            format!(
                "Placement: {:?} · {} planned files",
                preview.inspection.plan.placement,
                preview.inspection.plan.files.len()
            ),
            t,
        ));
        for mapping in preview.inspection.plan.files.iter().take(40) {
            body = body.child(muted(
                format!(
                    "{} → {}",
                    mapping.source.display(),
                    mapping.destination.display()
                ),
                t,
            ));
        }
        if preview.inspection.plan.files.len() > 40 {
            body = body.child(muted(
                format!(
                    "…and {} more files",
                    preview.inspection.plan.files.len() - 40
                ),
                t,
            ));
        }
        for warning in &preview.inspection.plan.warnings {
            body = body.child(muted(format!("Warning: {warning}"), t));
        }
        if preview.inspection.conflicts.is_empty() {
            body = body.child(muted("No file conflicts detected.", t));
        } else {
            body = body.child(muted(
                format!("{} conflicts:", preview.inspection.conflicts.len()),
                t,
            ));
            for conflict in &preview.inspection.conflicts {
                body = body.child(muted(
                    format!(
                        "{} (owned by {})",
                        conflict.path,
                        conflict.owner.as_deref().unwrap_or("unmanaged files")
                    ),
                    t,
                ));
            }
        }
        let safe = preview
            .inspection
            .conflicts
            .iter()
            .all(|conflict| conflict.owner.is_none())
            && (preview.inspection.conflicts.is_empty() || s.local.replace_unmanaged);
        let instance = instance.to_owned();
        body = body.child(button(
            "local:install",
            "Install preview",
            t,
            true,
            safe && s.busy.is_none(),
            move |state, _| {
                if state
                    .snapshot
                    .selected
                    .as_ref()
                    .map(|current| current.name.as_str())
                    != Some(instance.as_str())
                {
                    state.notice = "Selected game changed; preview the install again.".into();
                    return;
                }
                if !state.local.preview_matches() {
                    state.notice =
                        "Inputs changed since preview; preview the install again.".into();
                    return;
                }
                let Some(preview) = state.local.preview.take() else {
                    return;
                };
                if preview.instance != instance {
                    state.notice = "Selected game changed; preview the install again.".into();
                    return;
                }
                if preview
                    .inspection
                    .conflicts
                    .iter()
                    .any(|conflict| conflict.owner.is_some())
                    || (!preview.inspection.conflicts.is_empty() && !state.local.replace_unmanaged)
                {
                    state.notice =
                        "Resolve conflicts and explicitly allow replacing unmanaged files.".into();
                    return;
                }
                let keep_disabled = !state.preferences.enable_new_mods;
                let replace_unmanaged = state.local.replace_unmanaged;
                let instance = preview.instance;
                state.submit("Installing local mod", move |root| {
                    let mut library = open_library(&root)?;
                    library.install(
                        &instance,
                        &preview.slug,
                        &preview.input,
                        None,
                        InstallOptions {
                            destination: preview.destination.as_deref(),
                            keep_disabled,
                            replace_unmanaged,
                            ..Default::default()
                        },
                    )?;
                    Ok(Update::Library(Snapshot::load(&library, Some(&instance))?))
                });
            },
        ));
    }
    panel(t).child(body)
}
