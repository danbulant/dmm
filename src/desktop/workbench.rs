mod import;

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::Result;
use rune::tok::Theme;
use rune::{El, TextEdit, Tx, col, row};
use rune_kit::{Dialog, Kit, Placement, Select, SelectAppearance};

use dmm::library::{Inspection, Library, ManagedMod};
use dmm::sources::{RemoteMod, Source};

use super::components::{
    button, bytes, field, heading, muted, open_page, panel, source_name, styled_field,
};
use super::download_queue::Queue;
use super::mod_card::{Card, CardSource, byline, remote_key};
use super::{Origin, Screen, Size, Snapshot, State, Update, open_library};

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

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum LocalSort {
    #[default]
    Name,
    Size,
    State,
    DownloadDate,
}

impl LocalSort {
    const OPTIONS: [(Self, &'static str); 4] = [
        (Self::Name, "Name (A–Z)"),
        (Self::Size, "Size (largest)"),
        (Self::State, "Mod state"),
        (Self::DownloadDate, "Download date"),
    ];
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FilterMenu {
    Status,
    Site,
    Sort,
    Tag,
    Category,
    Character,
    Group,
}

pub(super) struct LocalForm {
    import_open: bool,
    pub input: TextEdit,
    pub slug: TextEdit,
    pub destination: TextEdit,
    pub replace_unmanaged: bool,
    search: TextEdit,
    tag_filter: Option<String>,
    category_filter: Option<String>,
    character_filter: Option<String>,
    group_filter: Option<String>,
    site_filter: Option<Source>,
    status: Status,
    sort: LocalSort,
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
            tag_filter: None,
            category_filter: None,
            character_filter: None,
            group_filter: None,
            site_filter: None,
            status: Status::Any,
            sort: LocalSort::default(),
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
    content = content.child(styled_field(
        "local-search",
        &s.local.search,
        field(&s.local.search, "Search installed and saved mods…", |s| {
            &mut s.local.search
        })
        .on_change(|s, _| refresh_filters(s)),
        k,
        t,
    ));
    content = content.child(
        row()
            .w_full()
            .flex_wrap()
            .gap(7.0)
            .child(filter_menu(s, k, layer, FilterMenu::Status))
            .child(filter_menu(s, k, layer, FilterMenu::Site))
            .child(record_filter_menu(s, k, layer, FilterMenu::Tag))
            .child(record_filter_menu(s, k, layer, FilterMenu::Category))
            .child(record_filter_menu(s, k, layer, FilterMenu::Character))
            .child(record_filter_menu(s, k, layer, FilterMenu::Group))
            .child(filter_menu(s, k, layer, FilterMenu::Sort)),
    );
    let rows = LocalPage::new(&s.local, &s.snapshot, s.available_sources(), &s.queue);
    let LocalPage {
        index: page,
        count: pages,
        ..
    } = rows;
    let mut grid = row().w_full().flex_wrap().gaps(12.0, 12.0);
    for entry in rows.visible() {
        grid = grid.child(match entry {
            Entry::Installed(instance, record) => installed_card(s, t, k, instance, record),
            Entry::Remote {
                id, name, image, ..
            } => remote_card(s, t, k, id, name, image),
        });
    }
    if pages == 0 {
        content = content.child(panel(t).child(muted(
            "No installed, saved, or queued mods match these filters.",
            t,
        )));
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
        FilterMenu::Sort => (
            "local-sort",
            "Sort local mods",
            LocalSort::OPTIONS.map(|(_, label)| label),
            LocalSort::OPTIONS
                .iter()
                .find(|(sort, _)| *sort == s.local.sort)
                .map(|(_, label)| *label),
        ),
        FilterMenu::Tag | FilterMenu::Category | FilterMenu::Character | FilterMenu::Group => {
            unreachable!("record filters use record_filter_menu")
        }
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
        .on_choose(move |s: &mut State, chosen, tx| {
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
                FilterMenu::Sort => {
                    if let Some((sort, _)) = LocalSort::OPTIONS
                        .iter()
                        .find(|(_, label)| *label == chosen)
                    {
                        s.local.sort = *sort;
                        set_page(s, 0, tx);
                    }
                    return;
                }
                FilterMenu::Tag
                | FilterMenu::Category
                | FilterMenu::Character
                | FilterMenu::Group => unreachable!(),
            }
            refresh_filters(s);
        })
        .build(k, layer)
        .border(1.0, k.c.border_base)
}

fn record_filter_menu(s: &State, k: &Kit, layer: &mut Vec<El>, menu: FilterMenu) -> El {
    let (key, label, any, current) = match menu {
        FilterMenu::Tag => (
            "local-tag-filter",
            "Tag",
            "Any tag",
            s.local.tag_filter.as_deref(),
        ),
        FilterMenu::Category => (
            "local-category-filter",
            "Category or character",
            "Any category or character",
            s.local.category_filter.as_deref(),
        ),
        FilterMenu::Character => (
            "local-character-filter",
            "Character",
            "Any character",
            s.local.character_filter.as_deref(),
        ),
        FilterMenu::Group => (
            "local-group-filter",
            "Local group",
            "Any group",
            s.local.group_filter.as_deref(),
        ),
        FilterMenu::Status | FilterMenu::Site | FilterMenu::Sort => unreachable!(),
    };
    let mut values = BTreeSet::new();
    for record in &s.snapshot.mods {
        match menu {
            FilterMenu::Tag => {
                if let Some(details) = record.details() {
                    values.extend(
                        details
                            .tags
                            .iter()
                            .filter(|tag| !tag.starts_with("Character: "))
                            .cloned(),
                    );
                }
            }
            FilterMenu::Category => {
                if let Some(category) = record
                    .details()
                    .and_then(|details| details.category.as_ref())
                {
                    values.insert(category.clone());
                }
            }
            FilterMenu::Character if record_source(record) == Source::GameBanana => {
                if let Some(details) = record.details() {
                    values.extend(
                        details
                            .tags
                            .iter()
                            .filter_map(|tag| tag.strip_prefix("Character: ").map(str::to_owned)),
                    );
                }
            }
            FilterMenu::Group => {
                if let Some(group) = &record.group {
                    values.insert(group.clone());
                }
            }
            FilterMenu::Character | FilterMenu::Status | FilterMenu::Site | FilterMenu::Sort => {}
        }
    }
    let mut options = vec![any.to_owned()];
    options.extend(values);
    Select::new(key, &options)
        .label(label)
        .appearance(SelectAppearance::Field)
        .placement(Placement::BottomStart)
        .width(140.0)
        .current(Some(current.unwrap_or(any)))
        .open(s.local.filter_menu == Some(menu))
        .on_toggle(move |s: &mut State| {
            s.local.filter_menu = (s.local.filter_menu != Some(menu)).then_some(menu)
        })
        .on_close(|s: &mut State| s.local.filter_menu = None)
        .on_choose(move |s: &mut State, chosen, _| {
            let value = (chosen != any).then(|| chosen.to_owned());
            match menu {
                FilterMenu::Tag => s.local.tag_filter = value,
                FilterMenu::Category => s.local.category_filter = value,
                FilterMenu::Character => s.local.character_filter = value,
                FilterMenu::Group => s.local.group_filter = value,
                FilterMenu::Status | FilterMenu::Site | FilterMenu::Sort => unreachable!(),
            }
            s.local.filter_menu = None;
            refresh_filters(s);
        })
        .build(k, layer)
        .border(1.0, k.c.border_base)
}

enum Entry<'a> {
    Installed(&'a str, &'a ManagedMod),
    Remote {
        id: &'a RemoteMod,
        name: &'a str,
        image: Option<&'a str>,
        queued: bool,
    },
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum EntryState {
    Enabled,
    Disabled,
    Queued,
    Bookmarked,
}

impl Entry<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Installed(_, record) => installed_title(record),
            Self::Remote { name, .. } => name,
        }
    }

    fn state(&self) -> EntryState {
        match self {
            Self::Installed(_, record) if record.enabled => EntryState::Enabled,
            Self::Installed(..) => EntryState::Disabled,
            Self::Remote { queued: true, .. } => EntryState::Queued,
            Self::Remote { queued: false, .. } => EntryState::Bookmarked,
        }
    }

    fn size(&self) -> Option<u64> {
        match self {
            Self::Installed(_, record) => record.installed_size,
            Self::Remote { .. } => None,
        }
    }

    fn downloaded_at(&self) -> Option<u64> {
        match self {
            Self::Installed(_, record) => record.downloaded_at_ms(),
            Self::Remote { .. } => None,
        }
    }
}

fn entries<'a>(
    form: &LocalForm,
    snapshot: &'a Snapshot,
    origins: &[Origin],
    queue: &'a Queue,
) -> Vec<Entry<'a>> {
    let mut entries = Vec::new();
    let q = form.search.text.trim().to_lowercase();
    let tag = form.tag_filter.as_deref();
    let category = form.category_filter.as_deref();
    let character = form.character_filter.as_deref();
    let group = form.group_filter.as_deref();
    let remote_names_only =
        tag.is_none() && category.is_none() && character.is_none() && group.is_none();
    if remote_names_only
        && matches!(form.status, Status::Any | Status::Saved)
        && let Some(instance) = &snapshot.selected
    {
        entries.extend(
            queue
                .pending_mods(&instance.name)
                .filter(|item| {
                    (form.status != Status::Saved || snapshot.is_saved(item.id))
                        && form.site_filter.is_none_or(|site| site == item.id.source)
                        && contains(item.name, &q)
                        && snapshot.installed_remote(item.id).is_none()
                })
                .map(|item| Entry::Remote {
                    id: item.id,
                    name: item.name,
                    image: item.image,
                    queued: true,
                }),
        );
    }
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
                && tag.is_none_or(|selected| {
                    tags.iter().any(|value| {
                        !value.starts_with("Character: ") && value.eq_ignore_ascii_case(selected)
                    })
                })
                && category.is_none_or(|selected| cat.eq_ignore_ascii_case(selected))
                && character.is_none_or(|selected| {
                    record_source(record) == Source::GameBanana
                        && tags.iter().any(|value| {
                            value
                                .strip_prefix("Character: ")
                                .is_some_and(|value| value.eq_ignore_ascii_case(selected))
                        })
                })
                && group.is_none_or(|selected| {
                    record
                        .group
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case(selected))
                })
            {
                entries.push(Entry::Installed(&instance.name, record));
            }
        }
    }
    if remote_names_only && matches!(form.status, Status::Any | Status::Saved) {
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
                        && !snapshot
                            .selected
                            .as_ref()
                            .is_some_and(|instance| queue.contains(&instance.name, &favorite.item))
                        && snapshot.installed_remote(&favorite.item).is_none()
                })
                .map(|item| Entry::Remote {
                    id: &item.item,
                    name: &item.name,
                    image: item.image.as_deref(),
                    queued: false,
                }),
        );
    }
    sort_entries(&mut entries, form.sort);
    entries
}

fn sort_entries(entries: &mut [Entry<'_>], sort: LocalSort) {
    match sort {
        LocalSort::Name => entries.sort_by_cached_key(|entry| entry.name().to_lowercase()),
        LocalSort::Size => {
            entries
                .sort_by_cached_key(|entry| (Reverse(entry.size()), entry.name().to_lowercase()));
        }
        LocalSort::State => {
            entries.sort_by_cached_key(|entry| (entry.state(), entry.name().to_lowercase()));
        }
        LocalSort::DownloadDate => entries.sort_by_cached_key(|entry| {
            (Reverse(entry.downloaded_at()), entry.name().to_lowercase())
        }),
    }
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

struct LocalPage<'a> {
    entries: Vec<Entry<'a>>,
    index: usize,
    count: usize,
}

impl<'a> LocalPage<'a> {
    fn new(form: &LocalForm, snapshot: &'a Snapshot, origins: &[Origin], queue: &'a Queue) -> Self {
        let entries = entries(form, snapshot, origins, queue);
        let count = entries.len().div_ceil(PAGE_SIZE);
        let index = form.page.min(count.saturating_sub(1));
        Self {
            entries,
            index,
            count,
        }
    }

    fn visible(self) -> impl Iterator<Item = Entry<'a>> {
        self.entries
            .into_iter()
            .skip(self.index * PAGE_SIZE)
            .take(PAGE_SIZE)
    }
}

pub(super) fn preview_requests<'a>(
    form: &LocalForm,
    snapshot: &'a Snapshot,
    origins: &[Origin],
    queue: &'a Queue,
) -> impl Iterator<Item = (&'a str, Size)> + use<'a> {
    LocalPage::new(form, snapshot, origins, queue)
        .visible()
        .filter_map(|entry| {
            let url = match entry {
                Entry::Installed(_, record) => installed_image(record),
                Entry::Remote { image, .. } => image,
            }?;
            Some((url, Size::Card))
        })
}

fn local_remote_key(id: &RemoteMod) -> String {
    format!("local-remote-{}", remote_key(id))
}

fn remote_card(
    s: &State,
    t: Theme,
    k: &Kit,
    id: &RemoteMod,
    name: &str,
    image: Option<&str>,
) -> El {
    Card {
        key: local_remote_key(id),
        name,
        image,
        byline: source_name(id.source).into(),
        metadata: String::new(),
        source: CardSource::Remote(id),
    }
    .view(s, t, k)
}

fn installed_card(s: &State, t: Theme, k: &Kit, instance: &str, managed: &ManagedMod) -> El {
    let details = managed.details();
    let metadata: Vec<_> = managed
        .group
        .as_deref()
        .into_iter()
        .chain(
            details
                .into_iter()
                .flat_map(|details| details.tags.iter().map(String::as_str)),
        )
        .take(3)
        .collect();
    Card {
        key: format!("installed-card-{instance}-{}", managed.slug),
        name: installed_title(managed),
        image: installed_image(managed),
        byline: byline(
            details.and_then(|d| d.author.as_deref()),
            details.and_then(|d| d.category.as_deref()),
            source_name(record_source(managed)),
        ),
        metadata: managed.installed_size.map_or_else(
            || metadata.join(" · "),
            |size| {
                if metadata.is_empty() {
                    bytes(size)
                } else {
                    format!("{} · {}", bytes(size), metadata.join(" · "))
                }
            },
        ),
        source: CardSource::Installed {
            instance,
            record: managed,
        },
    }
    .view(s, t, k)
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
            move |state, tx| open_page(state, tx, &url),
        ));
    }
    if let Some(release) = managed.release() {
        let id = release.id.clone();
        let queued = s.pending_install(&id).is_some();
        body = body.child(button(
            format!("installed:{instance}:{slug}:reinstall"),
            if queued {
                "In Downloads"
            } else {
                "Choose files to reinstall…"
            },
            t,
            false,
            queued || s.install_blocker(&id).is_none(),
            move |s, tx| s.request_install(&id, tx),
        ));
    }
    if !managed.enabled
        && s.snapshot
            .selected
            .as_ref()
            .is_some_and(|game| game.name == instance && !game.kind.is_xxmi())
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

pub(super) fn change_mod(
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
