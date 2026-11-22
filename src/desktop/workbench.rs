mod import;

use std::path::PathBuf;

use anyhow::Result;
use rune::tok::Theme;
use rune::{El, SurfaceFit, TextEdit, Tx, col, row};
use rune_kit::{Dialog, Kit, Placement, Select, SelectAppearance, Switch, TextInput};

use dmm::games::GameKind;
use dmm::library::{Inspection, Library, ManagedMod, SavedFavorite};
use dmm::sources::Source;

use super::{
    Origin, Screen, Size, Snapshot, State, Update, browse, button, heading, muted, open_library,
    panel,
};

const PAGE_SIZE: usize = 24;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Status {
    #[default]
    Any,
    Enabled,
    Disabled,
    Saved,
}

impl Status {
    const OPTIONS: [(Self, &'static str); 4] = [
        (Self::Any, "Any state"),
        (Self::Enabled, "Enabled"),
        (Self::Disabled, "Disabled"),
        (Self::Saved, "Saved only"),
    ];
}

const SITES: [(Option<Source>, &str); 4] = [
    (None, "Any site"),
    (Some(Source::Nexus), "Nexus Mods"),
    (Some(Source::GameBanana), "GameBanana"),
    (Some(Source::Local), "Local files"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum FilterMenu {
    Status,
    Site,
}

pub(super) struct LocalForm {
    import_open: bool,
    pub input: TextEdit,
    pub slug: TextEdit,
    pub destination: TextEdit,
    pub replace_unmanaged: bool,
    search: TextEdit,
    tag_filter: TextEdit,
    category_filter: TextEdit,
    character_filter: TextEdit,
    group_filter: TextEdit,
    site_filter: Option<Source>,
    status: Status,
    filter_menu: Option<FilterMenu>,
    page: usize,
    preview: Option<LocalPreview>,
    pending_preview: Option<(String, PathBuf, String, Option<PathBuf>)>,
    remove_confirm: Option<(String, String)>,
}

impl Default for LocalForm {
    fn default() -> Self {
        Self {
            import_open: false,
            input: TextEdit::default(),
            slug: TextEdit::default(),
            destination: TextEdit::default(),
            replace_unmanaged: false,
            search: TextEdit::default(),
            tag_filter: TextEdit::default(),
            category_filter: TextEdit::default(),
            character_filter: TextEdit::default(),
            group_filter: TextEdit::default(),
            site_filter: None,
            status: Status::Any,
            filter_menu: None,
            page: 0,
            preview: None,
            pending_preview: None,
            remove_confirm: None,
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
    pub(super) fn close_menus(&mut self) {
        self.filter_menu = None;
        self.remove_confirm = None;
    }

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

pub(super) fn local_mods(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) -> El {
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
            content = content.child(import::local_import(s, t, k, &instance.name));
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
    content = content.child(
        super::inputs::field(
            TextInput::new(&s.local.search)
                .placeholder("Search installed and saved mods…")
                .height(36.0)
                .width(None)
                .on_edit(|s: &mut State| &mut s.local.search)
                .on_change(|s, _| refresh_filters(s))
                .build(k),
            &s.local.search,
            k,
            t,
        )
        .key("local-search")
        .w_full(),
    );
    content = content.child(
        row()
            .w_full()
            .flex_wrap()
            .gap(7.0)
            .child(filter_menu(s, k, layer, FilterMenu::Status))
            .child(filter_menu(s, k, layer, FilterMenu::Site))
            .children(
                [
                    (
                        "local-tag-filter",
                        "Tag",
                        &s.local.tag_filter,
                        (|s: &mut State| &mut s.local.tag_filter)
                            as fn(&mut State) -> &mut TextEdit,
                    ),
                    (
                        "local-category-filter",
                        "Category",
                        &s.local.category_filter,
                        |s| &mut s.local.category_filter,
                    ),
                    (
                        "local-character-filter",
                        "Character",
                        &s.local.character_filter,
                        |s| &mut s.local.character_filter,
                    ),
                    (
                        "local-group-filter",
                        "Local group",
                        &s.local.group_filter,
                        |s| &mut s.local.group_filter,
                    ),
                ]
                .into_iter()
                .map(|(key, label, edit, access)| {
                    super::inputs::field(
                        TextInput::new(edit)
                            .label(label)
                            .placeholder(label)
                            .height(32.0)
                            .width(Some(130.0))
                            .on_edit(access)
                            .on_change(|s, _| refresh_filters(s))
                            .build(k),
                        edit,
                        k,
                        t,
                    )
                    .key(key)
                }),
            ),
    );
    let entries = entries(&s.local, &s.snapshot, s.available_sources());
    let pages = entries.len().div_ceil(PAGE_SIZE);
    let page = s.local.page.min(pages.saturating_sub(1));
    let mut grid = row().w_full().flex_wrap().gaps(12.0, 12.0);
    for entry in entries.into_iter().skip(page * PAGE_SIZE).take(PAGE_SIZE) {
        grid = grid.child(match entry {
            Entry::Installed(instance, record) => mod_card(s, t, k, instance, record),
            Entry::Saved(bookmark) => saved_card(s, t, bookmark),
        });
    }
    if pages == 0 {
        content = content
            .child(panel(t).child(muted("No installed or saved mods match these filters.", t)));
    } else {
        content = content.child(grid);
        if pages > 1 {
            content = content.child(
                row()
                    .gap(8.0)
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(muted(format!("Page {} of {pages}", page + 1), t))
                    .child(
                        row()
                            .gap(8.0)
                            .child(button(
                                "local-prev",
                                "Previous",
                                t,
                                false,
                                page > 0,
                                move |s, tx| set_page(s, page.saturating_sub(1), tx),
                            ))
                            .child(button(
                                "local-next",
                                "Next",
                                t,
                                false,
                                page + 1 < pages,
                                move |s, tx| set_page(s, page + 1, tx),
                            )),
                    ),
            );
        }
    }
    content
}

fn refresh_filters(s: &mut State) {
    s.local.page = 0;
    s.sync_previews();
}

fn set_page(s: &mut State, page: usize, tx: &mut Tx) {
    s.local.page = page;
    s.sync_previews();
    tx.scroll_to_instant("main-scroll", 0.0, 0.0);
}

fn filter_menu(s: &State, k: &Kit, layer: &mut Vec<El>, menu: FilterMenu) -> El {
    let (key, label, options, current) = match menu {
        FilterMenu::Status => (
            "local-state",
            "Deployment state",
            Status::OPTIONS.map(|(_, label)| label),
            Status::OPTIONS
                .iter()
                .find(|(status, _)| *status == s.local.status)
                .map(|(_, label)| *label),
        ),
        FilterMenu::Site => (
            "local-site",
            "Mod source",
            SITES.map(|(_, label)| label),
            SITES
                .iter()
                .find(|(source, _)| *source == s.local.site_filter)
                .map(|(_, label)| *label),
        ),
    };
    Select::new(key, &options)
        .label(label)
        .appearance(SelectAppearance::Field)
        .placement(Placement::BottomStart)
        .width(140.0)
        .current(current)
        .open(s.local.filter_menu == Some(menu))
        .on_toggle(move |s: &mut State| {
            s.local.filter_menu = (s.local.filter_menu != Some(menu)).then_some(menu)
        })
        .on_close(|s: &mut State| s.local.filter_menu = None)
        .on_choose(move |s: &mut State, chosen, _| {
            s.local.filter_menu = None;
            match menu {
                FilterMenu::Status => {
                    if let Some((status, _)) =
                        Status::OPTIONS.iter().find(|(_, label)| *label == chosen)
                    {
                        s.local.status = *status;
                    }
                }
                FilterMenu::Site => {
                    if let Some((source, _)) = SITES.iter().find(|(_, label)| *label == chosen) {
                        s.local.site_filter = *source;
                    }
                }
            }
            refresh_filters(s);
        })
        .build(k, layer)
        .border(1.0, k.c.border_base)
}

enum Entry<'a> {
    Installed(&'a str, &'a ManagedMod),
    Saved(&'a SavedFavorite),
}

fn entries<'a>(form: &LocalForm, snapshot: &'a Snapshot, origins: &[Origin]) -> Vec<Entry<'a>> {
    let mut entries = Vec::new();
    let q = form.search.text.trim().to_lowercase();
    let tag = form.tag_filter.text.trim().to_lowercase();
    let category = form.category_filter.text.trim().to_lowercase();
    let character = form.character_filter.text.trim().to_lowercase();
    let group = form.group_filter.text.trim().to_lowercase();
    if let Some(instance) = &snapshot.selected {
        for record in &snapshot.mods {
            let state_matches = match form.status {
                Status::Any => true,
                Status::Enabled => record.enabled,
                Status::Disabled => !record.enabled,
                Status::Saved => record
                    .release()
                    .is_some_and(|release| snapshot.is_saved(&release.id)),
            };
            if !state_matches
                || form
                    .site_filter
                    .is_some_and(|site| site != record_source(record))
            {
                continue;
            }
            let details = record.details();
            let release = record.release();
            let title = installed_title(record);
            let cat = details.and_then(|d| d.category.as_deref()).unwrap_or("");
            let tags = details.map(|d| d.tags.as_slice()).unwrap_or(&[]);
            if (contains(title, &q)
                || contains(&record.slug, &q)
                || release.is_some_and(|r| contains(&r.filename, &q))
                || details
                    .and_then(|d| d.author.as_deref())
                    .is_some_and(|author| contains(author, &q)))
                && (tag.is_empty() || tags.iter().any(|value| contains(value, &tag)))
                && contains(cat, &category)
                && (contains(cat, &character)
                    || tags
                        .iter()
                        .filter_map(|value| value.strip_prefix("Character: "))
                        .any(|value| contains(value, &character)))
                && contains(record.group.as_deref().unwrap_or(""), &group)
            {
                entries.push(Entry::Installed(&instance.name, record));
            }
        }
    }
    if matches!(form.status, Status::Any | Status::Saved)
        && [&tag, &category, &character, &group]
            .iter()
            .all(|value| value.is_empty())
    {
        entries.extend(
            snapshot
                .favorites
                .iter()
                .filter(|favorite| {
                    origins.iter().any(|origin| {
                        origin.source == favorite.item.source && origin.game == favorite.item.game
                    }) && form
                        .site_filter
                        .is_none_or(|site| site == favorite.item.source)
                        && contains(&favorite.name, &q)
                        && !snapshot.mods.iter().any(|record| {
                            record
                                .release()
                                .is_some_and(|release| release.id == favorite.item)
                        })
                })
                .map(Entry::Saved),
        );
    }
    entries
}

fn contains(value: &str, query: &str) -> bool {
    query.is_empty() || value.to_lowercase().contains(query)
}

fn record_source(record: &ManagedMod) -> Source {
    record
        .release()
        .map_or(Source::Local, |release| release.id.source)
}

pub(super) fn installed_title(record: &ManagedMod) -> &str {
    record
        .details()
        .map(|details| details.name.as_str())
        .or_else(|| record.release().map(|release| release.filename.as_str()))
        .unwrap_or(&record.slug)
}

fn installed_image(record: &ManagedMod) -> Option<&str> {
    let image = record.details()?.images.first()?;
    Some(image.thumbnail.as_deref().unwrap_or(&image.url))
}

pub(super) fn preview_requests<'a>(
    form: &LocalForm,
    snapshot: &'a Snapshot,
    origins: &[Origin],
) -> impl Iterator<Item = (&'a str, Size)> + use<'a> {
    let entries = entries(form, snapshot, origins);
    let page = form
        .page
        .min(entries.len().div_ceil(PAGE_SIZE).saturating_sub(1));
    entries
        .into_iter()
        .skip(page * PAGE_SIZE)
        .take(PAGE_SIZE)
        .filter_map(|entry| {
            let url = match entry {
                Entry::Installed(_, record) => installed_image(record),
                Entry::Saved(bookmark) => bookmark.image.as_deref(),
            }?;
            Some((url, Size::Card))
        })
}

fn saved_card(s: &State, t: Theme, bookmark: &SavedFavorite) -> El {
    let id = bookmark.item.clone();
    let key = format!("{}-{}-{}", id.source, id.game, id.id);
    let image = bookmark.image.as_deref();
    let detail_id = id.clone();
    let install_id = id.clone();
    let saved_id = id.clone();
    col()
        .key(format!("saved-card-{key}"))
        .w(248.0)
        .padding(9.0)
        .gap(7.0)
        .radius(12.0)
        .fill(t.card)
        .border(1.0, t.card_border)
        .child(super::browse::preview(s, image, 112.0, SurfaceFit::Fill, t))
        .child(heading(&bookmark.name, t).truncate().help(&bookmark.name))
        .child(muted(format!("Saved · {} · {}", id.source, id.game), t).truncate())
        .child(
            row()
                .gap(6.0)
                .flex_wrap()
                .child(button(
                    format!("local-saved-open-{key}"),
                    "Details",
                    t,
                    false,
                    s.details_busy.is_none(),
                    move |s, _| s.details(detail_id.clone()),
                ))
                .child(button(
                    format!("local-saved-install-{key}"),
                    "Install",
                    t,
                    false,
                    s.details_busy.is_none() && s.queue.available() && s.matches_instance(&id),
                    move |s, _| s.details_for_install(install_id.clone()),
                ))
                .child(button(
                    format!("local-saved-remove-{key}"),
                    "Unsave",
                    t,
                    false,
                    s.busy.is_none(),
                    move |s, _| s.unsave(saved_id.clone()),
                )),
        )
}

fn mod_card(s: &State, t: Theme, k: &Kit, instance: &str, managed: &ManagedMod) -> El {
    let slug = managed.slug.as_str();
    let details = managed.details();
    let release = managed.release();
    let name = installed_title(managed);
    let image = installed_image(managed);
    let source = record_source(managed);
    let mut card = col()
        .key(format!("installed-card-{instance}-{slug}"))
        .w(248.0)
        .padding(9.0)
        .gap(7.0)
        .radius(12.0)
        .fill(t.card)
        .border(1.0, t.card_border)
        .child(super::browse::preview(s, image, 112.0, SurfaceFit::Fill, t))
        .child(heading(name, t).truncate().help(name))
        .child(muted(browse::source_name(source), t).truncate());
    if let Some(category) = details.and_then(|details| details.category.as_deref()) {
        card = card.child(muted(category, t).truncate());
    } else if let Some(release) = release {
        card = card.child(muted(&release.filename, t).truncate());
    }
    let tags = details
        .map(|details| details.tags.as_slice())
        .unwrap_or(&[]);
    if !tags.is_empty() {
        let labels = tags
            .iter()
            .take(3)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" · ");
        let more = tags.len().saturating_sub(3);
        let label = if more == 0 {
            labels
        } else {
            format!("{labels} · +{more}")
        };
        card = card.child(muted(label, t).truncate());
    }
    if let Some(group) = &managed.group {
        card = card.child(muted(format!("Group: {group}"), t).truncate());
    }
    let open_slug = managed.slug.clone();
    card = card.child(button(
        format!("mod:{instance}:{slug}:details"),
        "Details and files",
        t,
        false,
        true,
        move |s, tx| s.open_installed_details(&open_slug, tx),
    ));
    if let Some(id) = release.map(|release| release.id.clone())
        && s.snapshot.is_saved(&id)
    {
        card = card.child(button(
            format!("mod:{instance}:{slug}:unsave"),
            "Unsave",
            t,
            false,
            s.busy.is_none(),
            move |s, _| s.unsave(id.clone()),
        ));
    }
    let toggle_slug = managed.slug.clone();
    let toggle_game = instance.to_owned();
    let enabled = managed.enabled;
    card.child(
        row()
            .w_full()
            .items_center()
            .justify_between()
            .child(muted(if enabled { "Enabled" } else { "Disabled" }, t))
            .child(
                Switch::new(enabled)
                    .label(format!("Enable {name}"))
                    .hide_label()
                    .disabled(s.busy.is_some())
                    .on_toggle(move |state: &mut State| {
                        change_mod(
                            state,
                            &toggle_game,
                            &toggle_slug,
                            "Updating mod…",
                            move |lib, game, slug| lib.toggle(game, slug, !enabled, false, false),
                        );
                    })
                    .build(k)
                    .key(format!("mod:{instance}:{slug}:enabled")),
            ),
    )
}

pub(super) fn installed_actions(s: &State, t: Theme, instance: &str, managed: &ManagedMod) -> El {
    let slug = managed.slug.clone();
    let mut body = col()
        .w_full()
        .gap(10.0)
        .child(heading("Installed files and actions", t))
        .child(muted(
            format!(
                "{} · {}",
                instance,
                if managed.enabled {
                    "Enabled"
                } else {
                    "Disabled"
                }
            ),
            t,
        ));
    if managed.details().is_none()
        && let Some(url) = managed
            .release()
            .and_then(|release| release.source_url.clone())
    {
        body = body.child(button(
            format!("installed:{instance}:{slug}:source"),
            "Open source page",
            t,
            false,
            true,
            move |state, tx| browse::open_page(state, tx, &url),
        ));
    }
    if let Some(release) = managed.release() {
        let id = release.id.clone();
        body = body.child(button(
            format!("installed:{instance}:{slug}:reinstall"),
            "Choose files to reinstall…",
            t,
            false,
            s.details_busy.is_none() && s.queue.available() && s.matches_instance(&id),
            move |s, _| s.details_for_install(id.clone()),
        ));
    }
    if !managed.enabled
        && s.snapshot
            .selected
            .as_ref()
            .is_some_and(|game| game.name == instance && game.kind != GameKind::Zzmi)
    {
        let target = instance.to_owned();
        let backup_slug = slug.clone();
        body = body.child(button(
            format!("installed:{instance}:{slug}:enable-backup"),
            "Enable and back up unmanaged files",
            t,
            false,
            s.busy.is_none(),
            move |state, _| {
                change_mod(
                    state,
                    &target,
                    &backup_slug,
                    "Enabling mod with backup…",
                    |lib, game, slug| lib.toggle(game, slug, true, true, false),
                )
            },
        ));
    }
    if let Some(group) = &managed.group {
        body = body.child(muted(format!("Local group: {group}"), t));
    }
    let files: Vec<_> = managed.files().collect();
    body = body.child(heading(format!("Managed files ({})", files.len()), t));
    for (file, _) in files {
        body = body.child(muted(file, t).wrap());
    }
    if managed.versions.len() > 1 {
        body = body.child(heading("Version history · roll back to", t));
        for (index, version) in managed.versions.iter().enumerate() {
            if index == managed.current {
                continue;
            }
            let label = version
                .release
                .as_ref()
                .and_then(|release| release.version.as_deref())
                .map_or_else(|| format!("Version {}", index + 1), str::to_owned);
            let target = instance.to_owned();
            let rollback_slug = slug.clone();
            body = body.child(button(
                format!("installed:{instance}:{slug}:rollback:{index}"),
                &label,
                t,
                false,
                s.busy.is_none(),
                move |state, _| {
                    change_mod(
                        state,
                        &target,
                        &rollback_slug,
                        "Rolling back mod…",
                        move |lib, game, slug| lib.rollback(game, slug, index, false),
                    )
                },
            ));
        }
    }
    let target = instance.to_owned();
    body.child(button(
        format!("installed:{instance}:{slug}:remove"),
        "Remove installed mod…",
        t,
        false,
        s.busy.is_none(),
        move |s, _| {
            s.local.remove_confirm = Some((target.clone(), slug.clone()));
        },
    ))
}

pub(super) fn overlays(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) {
    let Some((game, slug)) = &s.local.remove_confirm else {
        return;
    };
    let game = game.clone();
    let slug = slug.clone();
    let name = s
        .snapshot
        .mods
        .iter()
        .find(|record| record.slug == slug)
        .map_or(slug.as_str(), installed_title);
    Dialog::new("remove-installed", "Remove installed mod?")
        .open(true).fit().width(440.0)
        .on_close(|s: &mut State| s.local.remove_confirm = None)
        .child(col().w_full().gap(12.0).px(16.0).pb(16.0)
            .child(muted(format!("Remove {name}? Its managed files and rollback history will be removed. This cannot be undone."), t).wrap())
            .child(row().gap(8.0).justify_end()
                .child(button("remove-cancel", "Cancel", t, false, true, |s, _| s.local.remove_confirm = None))
                .child(button("remove-confirm", "Remove", t, true, s.busy.is_none(), move |s, _| {
                    s.local.remove_confirm = None;
                    change_mod(s, &game, &slug, "Removing mod…", Library::remove);
                }))))
        .build(k, layer);
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
        let mut library = open_library(&root)?;
        change(&mut library, &game, &slug)?;
        Ok(Update::Library(Snapshot::load(&library, Some(&game))?))
    });
}

fn nonempty_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}
