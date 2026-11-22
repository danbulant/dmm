use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use rune::tok::Theme;
use rune::{El, FileDialogOptions, TextEdit, Tx, col, row};

use dmm::games::GameKind;
use dmm::library::{Inspection, InstallOptions, Library, ManagedMod};

use super::Screen;
use super::{Snapshot, State, Update, button, field, heading, muted, panel};

pub(super) struct LocalForm {
    import_open: bool,
    pub input: TextEdit,
    pub slug: TextEdit,
    pub destination: TextEdit,
    pub replace_unmanaged: bool,
    pub keep_disabled: bool,
    preview: Option<LocalPreview>,
    pending_preview: Option<(String, PathBuf, String, Option<PathBuf>)>,
    remove_confirm: Option<String>,
    installed_page: usize,
}

impl Default for LocalForm {
    fn default() -> Self {
        Self {
            import_open: false,
            input: TextEdit::default(),
            slug: TextEdit::default(),
            destination: TextEdit::default(),
            replace_unmanaged: false,
            keep_disabled: true,
            preview: None,
            pending_preview: None,
            remove_confirm: None,
            installed_page: 0,
        }
    }
}

struct LocalPreview {
    instance: String,
    input: PathBuf,
    slug: String,
    destination: Option<PathBuf>,
    inspection: Inspection,
}

impl LocalForm {
    pub(super) fn begin_preview(&mut self, instance: String) {
        self.pending_preview = Some((
            instance,
            PathBuf::from(self.input.text.trim()),
            self.slug.text.trim().to_owned(),
            nonempty_path(&self.destination.text),
        ));
        self.preview = None;
    }

    pub(super) fn apply_preview(&mut self, inspection: Inspection) {
        let Some((instance, input, slug, destination)) = self.pending_preview.take() else {
            return;
        };
        if input != PathBuf::from(self.input.text.trim())
            || slug != self.slug.text.trim()
            || destination != nonempty_path(&self.destination.text)
        {
            return;
        }
        self.preview = Some(LocalPreview {
            instance,
            input,
            slug,
            destination,
            inspection,
        });
    }

    fn preview_matches(&self) -> bool {
        self.preview.as_ref().is_some_and(|preview| {
            preview.input == PathBuf::from(self.input.text.trim())
                && preview.slug == self.slug.text.trim()
                && preview.destination == nonempty_path(&self.destination.text)
        })
    }
}

pub(super) fn local_mods(s: &State, t: Theme) -> El {
    const PAGE_SIZE: usize = 20;
    let mut title = row()
        .w_full()
        .items_center()
        .gap(8.0)
        .child(heading("Local mods", t).grow(1.0));
    if s.snapshot.selected.is_some() {
        title = title.child(
            button(
                "local:add",
                if s.local.import_open {
                    "Close import"
                } else {
                    "+ Add local mod"
                },
                t,
                s.local.import_open,
                true,
                |s, _| s.local.import_open = !s.local.import_open,
            )
            .expanded(s.local.import_open),
        );
    }
    let mut content = col().w_full().gap(12.0).child(title);
    if let Some(instance) = &s.snapshot.selected {
        content = content.child(
            muted(
                format!("{} · {}", instance.name, instance.root.display()),
                t,
            )
            .wrap(),
        );
        if s.local.import_open {
            content = content.child(local_import(s, t, &instance.name));
        }
        let pages = s.snapshot.mods.len().div_ceil(PAGE_SIZE).max(1);
        let page = s.local.installed_page.min(pages - 1);
        if pages > 1 {
            content = content.child(
                row()
                    .items_center()
                    .gap(8.0)
                    .child(muted(
                        format!("Installed · page {} of {pages}", page + 1),
                        t,
                    ))
                    .child(button(
                        "local-prev",
                        "Previous",
                        t,
                        false,
                        page > 0 && s.busy.is_none(),
                        move |s, _| s.local.installed_page = page - 1,
                    ))
                    .child(button(
                        "local-next",
                        "Next",
                        t,
                        false,
                        page + 1 < pages && s.busy.is_none(),
                        move |s, _| s.local.installed_page = page + 1,
                    )),
            );
        }
        for managed in s
            .snapshot
            .mods
            .iter()
            .skip(page * PAGE_SIZE)
            .take(PAGE_SIZE)
        {
            content = content.child(mod_row(s, t, &instance.name, managed));
        }
    } else {
        content = content.child(
            panel(t)
                .child(
                    muted(
                        "Choose a main installation in Game setup to import or enable mods.",
                        t,
                    )
                    .wrap(),
                )
                .child(button(
                    "local-setup",
                    "Game setup",
                    t,
                    false,
                    s.busy.is_none(),
                    |s, tx| s.set_screen(Screen::Setup, tx),
                )),
        );
    }
    let installed: HashSet<_> = s
        .snapshot
        .mods
        .iter()
        .filter_map(|managed| managed.release().map(|release| &release.id))
        .collect();
    let saved: Vec<_> = s
        .snapshot
        .favorites
        .iter()
        .filter(|id| {
            !installed.contains(*id)
                && s.available_sources()
                    .iter()
                    .any(|origin| origin.source == id.source && origin.game == id.game)
        })
        .collect();
    if s.snapshot.mods.is_empty() && saved.is_empty() {
        content = content.child(panel(t).child(muted("No local mods or saved bookmarks.", t)));
    }
    if !saved.is_empty() {
        content = content.child(heading("Saved bookmarks", t)).child(
            muted(
                "Open a bookmark to choose and import a file. Only installed mods can be enabled.",
                t,
            )
            .wrap(),
        );
        for id in saved {
            let key = format!("{}-{}-{}", id.source, id.game, id.id);
            let open_id = id.clone();
            let saved_id = id.clone();
            content = content.child(
                panel(t).child(
                    row()
                        .gap(8.0)
                        .items_center()
                        .w_full()
                        .child(
                            col()
                                .grow(1.0)
                                .shrink()
                                .child(heading(format!("{} · {}", id.source, id.id), t))
                                .child(muted(format!("{} · {}", id.source, id.game), t)),
                        )
                        .child(button(
                            format!("local-saved-open-{key}"),
                            "Details",
                            t,
                            false,
                            s.busy.is_none(),
                            move |s, _| s.details(open_id.clone()),
                        ))
                        .child(button(
                            format!("local-saved-remove-{key}"),
                            "Unsave",
                            t,
                            false,
                            s.busy.is_none(),
                            move |s, _| s.favorite(saved_id.clone(), false),
                        )),
                ),
            );
        }
    }
    content
}
fn local_import(s: &State, t: Theme, instance: &str) -> El {
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
            .child(button(
                "local:keep-disabled",
                if s.local.keep_disabled {
                    "Staged (disabled)"
                } else {
                    "Install enabled"
                },
                t,
                s.local.keep_disabled,
                true,
                |s, _| s.local.keep_disabled = !s.local.keep_disabled,
            ))
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
                let library = Library::open(&root)?;
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
                let keep_disabled = state.local.keep_disabled;
                let replace_unmanaged = state.local.replace_unmanaged;
                let instance = preview.instance;
                state.submit("Installing local mod", move |root| {
                    let mut library = Library::open(&root)?;
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

fn mod_row(s: &State, t: Theme, instance: &str, managed: &ManagedMod) -> El {
    let slug = managed.slug.clone();
    let file_count = managed.files().count();
    let target = instance.to_owned();
    let toggle_slug = slug.clone();
    let source_link = managed.release().and_then(|r| r.source_url.clone());
    let enabled = managed.enabled;
    let toggle = button(
        format!("mod:{slug}:toggle"),
        if enabled { "Disable" } else { "Enable" },
        t,
        enabled,
        s.busy.is_none(),
        move |state, _| {
            change_mod(
                state,
                &target,
                &toggle_slug,
                "Updating mod…",
                move |lib, game, slug| lib.toggle(game, slug, !enabled, false, false),
            );
        },
    );
    let mut body = col().gap(6.0).child(
        row()
            .gap(8.0)
            .child(heading(slug.clone(), t))
            .child(muted(if enabled { "Enabled" } else { "Disabled" }, t))
            .child(toggle),
    );
    if let Some(id) = managed.release().map(|release| release.id.clone()) {
        let detail_id = id.clone();
        let saved = s.snapshot.favorites.contains(&id);
        body = body.child(
            row()
                .gap(8.0)
                .child(button(
                    format!("mod:{slug}:details"),
                    "Details",
                    t,
                    false,
                    s.busy.is_none(),
                    move |s, _| s.details(detail_id.clone()),
                ))
                .child(button(
                    format!("mod:{slug}:favorite"),
                    if saved { "Unsave" } else { "Save" },
                    t,
                    saved,
                    s.busy.is_none(),
                    move |s, _| s.favorite(id.clone(), !saved),
                )),
        );
    }
    if !enabled
        && s.snapshot
            .selected
            .as_ref()
            .is_some_and(|game| game.kind != GameKind::Zzmi)
    {
        let target = instance.to_owned();
        let slug = slug.clone();
        body = body.child(button(
            format!("mod:{slug}:enable-with-backup"),
            "Enable and back up unmanaged files",
            t,
            false,
            s.busy.is_none(),
            move |state, _| {
                change_mod(
                    state,
                    &target,
                    &slug,
                    "Enabling mod with backup…",
                    |lib, game, slug| lib.toggle(game, slug, true, true, false),
                );
            },
        ));
    }
    if let Some(group) = &managed.group {
        body = body.child(muted(format!("Group: {group}"), t));
    }
    body = body.child(muted(format!("{file_count} managed files"), t));
    for (file, _) in managed.files().take(20) {
        body = body.child(muted(file, t));
    }
    if file_count > 20 {
        body = body.child(muted(format!("…and {} more files", file_count - 20), t));
    }
    if managed.versions.len() > 1 {
        let mut controls = row().gap(8.0).flex_wrap().child(muted("Rollback to:", t));
        for (index, version) in managed.versions.iter().enumerate() {
            if index == managed.current {
                continue;
            }
            let label = version
                .release
                .as_ref()
                .and_then(|r| r.version.as_deref())
                .map_or_else(|| format!("Version {}", index + 1), str::to_owned);
            let slug = slug.clone();
            let target = instance.to_owned();
            controls = controls.child(button(
                format!("mod:{slug}:rollback:{index}"),
                label,
                t,
                false,
                s.busy.is_none(),
                move |state, _| {
                    change_mod(
                        state,
                        &target,
                        &slug,
                        "Rolling back mod…",
                        move |lib, game, slug| lib.rollback(game, slug, index, false),
                    );
                },
            ));
        }
        body = body.child(controls);
    }
    if let Some(url) = source_link {
        body = body.child(button(
            format!("mod:{slug}:source"),
            "Open source page",
            t,
            false,
            true,
            move |state, tx| {
                if let Err(error) = tx.open_url(&url) {
                    state.notice = error.to_string();
                }
            },
        ));
    }
    let removing = s.local.remove_confirm.as_deref() == Some(slug.as_str());
    let instance = instance.to_owned();
    let remove = if removing { "Confirm remove" } else { "Remove" };
    let remove_slug = slug.clone();
    body = body.child(button(
        format!("mod:{slug}:remove"),
        remove,
        t,
        false,
        s.busy.is_none(),
        move |state, _| {
            if state.local.remove_confirm.as_deref() != Some(remove_slug.as_str()) {
                state.local.remove_confirm = Some(remove_slug.clone());
                return;
            }
            state.local.remove_confirm = None;
            change_mod(
                state,
                &instance,
                &remove_slug,
                "Removing mod…",
                Library::remove,
            );
        },
    ));
    panel(t).child(body)
}

fn change_mod(
    state: &mut State,
    game: &str,
    slug: &str,
    label: &'static str,
    change: impl FnOnce(&mut Library, &str, &str) -> Result<()> + Send + 'static,
) {
    if state
        .snapshot
        .selected
        .as_ref()
        .is_none_or(|selected| selected.name != game)
    {
        state.notice = "The selected game changed. Try again from Local mods.".into();
        return;
    }
    let game = game.to_owned();
    let slug = slug.to_owned();
    state.submit(label, move |root| {
        let mut library = Library::open(&root)?;
        change(&mut library, &game, &slug)?;
        Ok(Update::Library(Snapshot::load(&library, Some(&game))?))
    });
}

fn nonempty_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}
