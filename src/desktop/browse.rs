pub(super) mod details;
mod filters;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use dmm::acquisition::{RemoteOutcome, RemoteSelection, accept_nxm, install_remote};
use dmm::games::GameKind;
use dmm::library::{InstallOptions, Library};
use dmm::sources::{
    FilterCatalog, Maturity, ModDetails, ProviderFilters, RemoteMod, SearchPage, SearchQuery,
    SearchResult, Sort, Source, TimeWindow,
};
use rune::tok::{Theme, type_};
use rune::{El, SurfaceFit, TextEdit, TextStyle, Tx, col, el, oklch, row, text};
use rune_kit::{ImageZoom, Kit, TextInput};
use url::Url;

use super::{Origin, Screen, Snapshot, State, Update, button, field, heading, muted, panel};

const PAGE_CACHE_LIMIT: usize = 24;
const PAGE_TTL: Duration = Duration::from_secs(300);
const SEARCH_INDICATOR_DELAY: Duration = Duration::from_millis(180);
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(275);

pub(super) struct Browser {
    pub(super) source: Source,
    query: TextEdit,
    sort: Sort,
    sort_menu: bool,
    category: Option<String>,
    category_group: Option<String>,
    show_filters: bool,
    find_category: TextEdit,
    maturity: Maturity,
    provider_filters: ProviderFilters,
    author: TextEdit,
    language: TextEdit,
    tag: TextEdit,
    catalog: Catalog,
    pending_search: Option<(Instant, SearchQuery)>,
    page_cache: VecDeque<CachedPage>,
    scroll_to_results: bool,
    searching_since: Option<Instant>,
    pub(super) show_search_progress: bool,
    pub(super) search_revision: u64,
    pub(super) results: Option<Rc<SearchPage>>,
    pub(super) shown_query: Option<SearchQuery>,
    pub(super) detail: Option<Detail>,
    install_name: TextEdit,
    install_into: TextEdit,
    pub(super) handoff: Option<Url>,
    pub(super) nxm: TextEdit,
    show_nxm: bool,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            source: Source::Nexus,
            query: TextEdit::default(),
            sort: Sort::Relevance,
            sort_menu: false,
            category: None,
            category_group: None,
            show_filters: false,
            find_category: TextEdit::default(),
            maturity: Maturity::Safe,
            provider_filters: ProviderFilters::default(),
            author: TextEdit::default(),
            language: TextEdit::default(),
            tag: TextEdit::default(),
            catalog: Catalog::Idle,
            pending_search: None,
            page_cache: VecDeque::new(),
            scroll_to_results: false,
            searching_since: None,
            show_search_progress: false,
            search_revision: 0,
            results: None,
            shown_query: None,
            detail: None,
            handoff: None,
            nxm: TextEdit::default(),
            show_nxm: false,
            install_name: TextEdit::default(),
            install_into: TextEdit::default(),
        }
    }
}

struct CachedPage {
    origin: Origin,
    query: SearchQuery,
    page: Rc<SearchPage>,
    expires: Instant,
}

enum Catalog {
    Idle,
    Loading,
    Ready(FilterCatalog),
    Failed,
}

pub(super) struct Detail {
    pub(super) info: ModDetails,
    pub(super) image: usize,
    description: String,
    selected_file: Option<u64>,
    stage: bool,
    install_open: bool,
    full_image_open: bool,
    pub(super) zoom: ImageZoom,
}

impl Detail {
    pub(super) fn poll_zoom(&mut self, now: f64, tx: &mut Tx) -> bool {
        self.full_image_open && self.zoom.poll(now, tx)
    }
}

impl Browser {
    pub(super) fn reset_origin(&mut self) {
        self.category = None;
        self.category_group = None;
        self.catalog = Catalog::Idle;
        self.pending_search = None;
        self.search_revision += 1;
        self.sort_menu = false;
        self.find_category = TextEdit::default();
        self.provider_filters = ProviderFilters::default();
        self.author = TextEdit::default();
        self.language = TextEdit::default();
        self.tag = TextEdit::default();
        self.results = None;
        self.shown_query = None;
        self.searching_since = None;
        self.show_search_progress = false;
        self.detail = None;
        self.show_filters = false;
    }

    fn catalog(&self) -> Option<&FilterCatalog> {
        match &self.catalog {
            Catalog::Ready(catalog) => Some(catalog),
            Catalog::Idle | Catalog::Loading | Catalog::Failed => None,
        }
    }

    pub(super) fn close_menus(&mut self) {
        self.sort_menu = false;
    }

    pub(super) fn accept_catalog(
        &mut self,
        result: anyhow::Result<FilterCatalog>,
    ) -> anyhow::Result<()> {
        match result {
            Ok(catalog) => {
                self.catalog = Catalog::Ready(catalog);
                Ok(())
            }
            Err(error) => {
                self.catalog = Catalog::Failed;
                Err(error)
            }
        }
    }

    pub(super) fn category_icons(&self) -> impl Iterator<Item = &str> {
        if self.show_filters {
            self.catalog()
        } else {
            None
        }
        .into_iter()
        .flat_map(|catalog| {
            catalog
                .categories
                .iter()
                .filter(|category| {
                    category.parent.is_none()
                        || (!self.find_category.text.trim().is_empty()
                            || category.parent.as_ref() == self.category_group.as_ref())
                })
                .filter_map(|category| category.icon.as_deref())
        })
    }

    pub(super) fn show_details(&mut self, info: ModDetails) {
        let raw = info
            .description
            .as_deref()
            .or(info.summary.as_deref())
            .unwrap_or_default();
        let message: String = raw.chars().take(5000).collect();
        let description = if message.contains('<') {
            std::mem::take(&mut TextEdit::from_html(&message).text)
        } else {
            message
        };
        self.install_name = TextEdit::new(format!("{}-{}", info.id.source, info.id.id));
        self.install_into = TextEdit::default();
        self.detail = Some(Detail {
            selected_file: (info.files.len() == 1).then(|| info.files[0].id),
            info,
            image: 0,
            description,
            install_open: false,
            full_image_open: false,
            zoom: ImageZoom::new(),
            stage: true,
        });
    }
}

impl Browser {
    fn cached_page(&mut self, origin: &Origin, query: &SearchQuery) -> Option<Rc<SearchPage>> {
        let now = Instant::now();
        self.page_cache.retain(|entry| entry.expires > now);
        let index = self
            .page_cache
            .iter()
            .position(|entry| &entry.origin == origin && &entry.query == query)?;
        let entry = self.page_cache.remove(index)?;
        let page = Rc::clone(&entry.page);
        self.page_cache.push_front(entry);
        Some(page)
    }

    pub(super) fn remember_page(&mut self, origin: Origin, query: SearchQuery, page: SearchPage) {
        if let (Some(total), Some(pages)) = (page.total, page.total_pages) {
            for entry in &mut self.page_cache {
                if entry.origin == origin
                    && (entry.page.total, entry.page.total_pages) != (Some(total), Some(pages))
                {
                    let mut compared = query.clone();
                    compared.page = entry.query.page;
                    if compared == entry.query {
                        let previous = Rc::make_mut(&mut entry.page);
                        previous.total = Some(total);
                        previous.total_pages = Some(pages);
                    }
                }
            }
        }
        let page = Rc::new(page);
        self.page_cache
            .retain(|entry| entry.origin != origin || entry.query != query);
        self.page_cache.push_front(CachedPage {
            origin,
            query: query.clone(),
            page: Rc::clone(&page),
            expires: Instant::now() + PAGE_TTL,
        });
        self.page_cache.truncate(PAGE_CACHE_LIMIT);
        self.results = Some(page);
        self.shown_query = Some(query);
    }

    pub(super) fn take_page_scroll(&mut self) -> bool {
        std::mem::take(&mut self.scroll_to_results)
    }

    pub(super) fn poll_search_progress(&mut self, searching: bool, tx: &mut Tx) -> bool {
        let elapsed = self
            .searching_since
            .filter(|_| searching)
            .map(|start| start.elapsed());
        let in_progress = elapsed.is_some() || self.pending_search.is_some();
        let visible = in_progress
            && (self.show_search_progress
                || elapsed.is_some_and(|time| time >= SEARCH_INDICATOR_DELAY));
        if !visible && let Some(elapsed) = elapsed {
            tx.wake_after(SEARCH_INDICATOR_DELAY.saturating_sub(elapsed));
        }
        let changed = self.show_search_progress != visible;
        self.show_search_progress = visible;
        changed
    }

    pub(super) fn may_show_previous_cards(&self) -> bool {
        self.shown_query
            .as_ref()
            .is_none_or(|query| self.maturity == Maturity::All || query.maturity == self.maturity)
    }
}

impl State {
    pub(super) fn explore_on_open(&mut self) {
        if self.browser.results.is_none() {
            self.search();
        }
    }

    pub(super) fn search(&mut self) {
        self.run_query(self.query_for_controls(), Duration::ZERO, false);
        self.drive_explore();
    }

    fn search_after_edit(&mut self, tx: &mut Tx) {
        self.run_query(self.query_for_controls(), SEARCH_DEBOUNCE, false);
        tx.wake_after(SEARCH_DEBOUNCE);
    }

    fn query_for_controls(&self) -> SearchQuery {
        let mut filters = self.browser.provider_filters.clone();
        if self.browser.source == Source::Nexus {
            filters.author = nonempty(&self.browser.author.text).map(str::to_owned);
            filters.language = nonempty(&self.browser.language.text).map(str::to_owned);
            filters.tag = nonempty(&self.browser.tag.text).map(str::to_owned);
        }
        SearchQuery {
            text: self.browser.query.text.trim().to_owned(),
            page: 1,
            category: self.browser.category.clone(),
            sort: self.browser.sort,
            maturity: self.browser.maturity,
            filters,
        }
    }

    fn run_query(&mut self, query: SearchQuery, delay: Duration, page_navigation: bool) -> bool {
        let Some(origin) = self.origin().cloned() else {
            return false;
        };
        self.browser.search_revision += 1;
        self.browser.scroll_to_results = page_navigation;
        if let Some(page) = self.browser.cached_page(&origin, &query) {
            self.browser.pending_search = None;
            self.browser.results = Some(page);
            self.browser.shown_query = Some(query);
            self.browser.show_search_progress = false;
            self.browser.searching_since = None;
            self.sync_previews();
            return true;
        }
        self.browser.pending_search = Some((Instant::now() + delay, query));
        if !self.browser.may_show_previous_cards() {
            self.sync_previews();
        }
        false
    }

    pub(super) fn drive_explore(&mut self) -> bool {
        if self.catalog_busy.is_some()
            || self.screen != Screen::Explore
            || self.browser.detail.is_some()
        {
            return false;
        }
        let Some(origin) = self.origin().cloned() else {
            return false;
        };
        if self.browser.show_filters && matches!(self.browser.catalog, Catalog::Idle) {
            let submitted = self.submit_catalog("Loading provider filters…", move |worker| {
                let result = worker
                    .online()
                    .and_then(|client| client.filter_catalog(origin.source, &origin.game));
                Ok(Update::Filters(origin, result))
            });
            self.browser.catalog = if submitted {
                Catalog::Loading
            } else {
                Catalog::Failed
            };
            submitted
        } else if self
            .browser
            .pending_search
            .as_ref()
            .is_some_and(|(due, _)| *due <= Instant::now())
            && let Some((_, query)) = self.browser.pending_search.take()
        {
            let revision = self.browser.search_revision;
            let submitted = self.submit_catalog("Searching mods…", move |worker| {
                let result = worker
                    .online()
                    .and_then(|client| client.search(origin.source, &origin.game, &query));
                Ok(Update::Search(revision, query, result))
            });
            if submitted {
                self.browser.searching_since = Some(Instant::now());
            }
            submitted
        } else {
            false
        }
    }

    pub(super) fn page(&mut self, next: bool, tx: &mut Tx) {
        let Some(mut query) = self.browser.shown_query.clone() else {
            return;
        };
        query.page = if next {
            query.page.saturating_add(1)
        } else {
            query.page.saturating_sub(1).max(1)
        };
        if self.run_query(query, Duration::ZERO, true) && self.browser.take_page_scroll() {
            tx.scroll_to_instant("main-scroll", 0.0, 0.0);
        }
        self.drive_explore();
    }

    fn change_source(&mut self, source: Source, tx: &mut Tx) {
        if self.browser.source != source {
            self.browser.source = source;
            self.browser.reset_origin();
            self.sync_previews();
            tx.scroll_to_instant("main-scroll", 0.0, 0.0);
            self.search();
        }
    }

    fn filters(&mut self) {
        self.browser.show_filters = !self.browser.show_filters;
        if self.browser.show_filters && matches!(self.browser.catalog, Catalog::Failed) {
            self.browser.catalog = Catalog::Idle;
        }
        self.sync_previews();
        self.drive_explore();
    }

    fn clear_filters(&mut self) {
        self.browser.provider_filters = ProviderFilters::default();
        self.browser.category = None;
        self.browser.category_group = None;
        self.browser.find_category = TextEdit::default();
        self.browser.author = TextEdit::default();
        self.browser.language = TextEdit::default();
        self.browser.tag = TextEdit::default();
        if self.browser.maturity == Maturity::Adult {
            self.browser.maturity = Maturity::All;
        }
        self.sync_previews();
        self.search();
    }

    fn set_publish_window(&mut self, window: Option<TimeWindow>) {
        if self.browser.provider_filters.published_within != window {
            self.browser.provider_filters.published_within = window;
            self.search();
        }
    }

    fn category(&mut self, category: Option<String>) {
        self.browser.category = category;
        self.search();
    }

    pub(super) fn details(&mut self, id: RemoteMod) {
        self.load_details(id, false);
    }

    pub(super) fn details_for_install(&mut self, id: RemoteMod) {
        self.load_details(id, true);
    }

    fn load_details(&mut self, id: RemoteMod, install: bool) {
        let maturity = (self.screen == Screen::Explore).then_some(self.browser.maturity);
        let revision = maturity.map(|_| self.browser.search_revision);
        let listed_rating = self.browser.results.as_ref().and_then(|page| {
            page.items
                .iter()
                .find(|item| item.id == id)
                .and_then(|item| item.adult)
        });
        self.submit_job("Loading mod details…", move |worker| {
            let result = worker
                .online()
                .and_then(|client| client.details(&id))
                .and_then(|detail| {
                    let adult = detail.adult.or(listed_rating);
                    let matches = match maturity {
                        Some(Maturity::Safe) => adult == Some(false),
                        Some(Maturity::Adult) => adult == Some(true),
                        Some(Maturity::All) | None => true,
                    };
                    anyhow::ensure!(
                        matches,
                        "This mod's content rating no longer matches the Explore filter."
                    );
                    Ok(detail)
                });
            Ok(Update::Details(revision, install, result))
        });
    }

    pub(super) fn favorite(&mut self, id: RemoteMod, save: bool) {
        let target = self.snapshot.selected.as_ref().map(|i| i.name.clone());
        self.submit("Updating saved mods…", move |dir| {
            let library = Library::open(&dir)?;
            library.favorite(&id, save)?;
            Ok(Update::Library(Snapshot::load(
                &library,
                target.as_deref(),
            )?))
        });
    }

    fn matches_instance(&self, id: &RemoteMod) -> bool {
        self.snapshot.selected.is_some()
            && self
                .snapshot
                .sources
                .iter()
                .any(|o| o.source == id.source && o.game == id.game)
    }

    fn install_selected(&mut self) {
        let Some(detail) = &self.browser.detail else {
            return;
        };
        if !self.matches_instance(&detail.info.id) {
            return;
        }
        let Some(target) = self.snapshot.selected.as_ref().map(|i| i.name.clone()) else {
            return;
        };
        let Some(file) = detail
            .info
            .files
            .iter()
            .find(|file| Some(file.id) == detail.selected_file)
            .cloned()
        else {
            return;
        };
        let id = detail.info.id.clone();
        let slug = self.browser.install_name.text.trim().to_owned();
        let source_url = detail.info.url.clone();
        let into = nonempty(&self.browser.install_into.text).map(PathBuf::from);
        let keep_disabled = detail.stage;
        self.submit_online("Preparing selected file…", move |dir, client| {
            let mut library = Library::open(&dir)?;
            let selection = RemoteSelection {
                id: &id,
                file: &file,
                source_url: source_url.as_deref(),
            };
            let options = InstallOptions {
                destination: into.as_deref(),
                keep_disabled,
                ..Default::default()
            };
            match install_remote(&mut library, client, &target, &slug, selection, options)? {
                RemoteOutcome::Installed => Ok(Update::RemoteInstalled(Snapshot::load(
                    &library,
                    Some(&target),
                )?)),
                RemoteOutcome::Browser(url) => Ok(Update::Handoff(url)),
            }
        });
    }

    fn redeem_link(&mut self) {
        let Some(uri) = nonempty(&self.browser.nxm.text).map(str::to_owned) else {
            return;
        };
        let target = self.snapshot.selected.as_ref().map(|i| i.name.clone());
        self.submit_online("Receiving Nexus download…", move |dir, client| {
            let mut library = Library::open(&dir)?;
            accept_nxm(&mut library, client, &uri)?;
            Ok(Update::RemoteInstalled(Snapshot::load(
                &library,
                target.as_deref(),
            )?))
        });
    }

    fn gallery_step(&mut self, index: usize) {
        if let Some(detail) = &mut self.browser.detail
            && index < detail.info.images.len()
            && index != detail.image
        {
            detail.image = index;
            detail.zoom.reset();
            self.sync_previews();
        }
    }
}

pub(super) fn explore(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) -> El {
    let game = match s.game_kind() {
        GameKind::Cyberpunk => "CYBERPUNK 2077",
        GameKind::Zzmi => "ZENLESS ZONE ZERO",
        GameKind::Generic => "CUSTOM GAME",
    };
    let sources = row().gap(8.0).items_center().children(
        s.available_sources()
            .iter()
            .map(|origin| source_button(s, origin.source, t)),
    );
    let mut view = col().w_full().gap(14.0).child(
        row()
            .key("explore-heading")
            .w_full()
            .justify_between()
            .items_center()
            .gap(12.0)
            .child(
                col()
                    .gap(4.0)
                    .grow(1.0)
                    .shrink()
                    .child(muted(game, t))
                    .child(heading("Explore mods", t)),
            )
            .child(sources),
    );
    if let Some(issue) = &s.snapshot.profile_issue {
        return view.child(panel(t).child(muted(issue, t).wrap()));
    }
    if s.available_sources().is_empty() {
        return view.child(
            panel(t).child(
                muted(
                    "This game has no online source. Open Local mods to import files.",
                    t,
                )
                .wrap(),
            ),
        );
    }
    let query = search_field("query", "Search mods…", &s.browser.query, k, |s| {
        &mut s.browser.query
    });
    let controls = row()
        .w_full()
        .items_center()
        .gap(8.0)
        .child(query)
        .child(button("search", "Search", t, true, true, |s, _| s.search()));
    view = view.child(controls).child(filters::toolbar(s, t, k, layer));
    if s.browser.show_filters {
        view = view.child(filters::options(s, t, k));
    }
    if s.browser.show_nxm || s.browser.handoff.is_some() {
        view = view.child(handoff(s, t));
    }
    if let Some(page) = &s.browser.results {
        view.child(results(s, page, t))
    } else {
        let message = if s.browser.show_search_progress {
            "Searching the provider catalog…"
        } else {
            "Search by name, or leave the box empty to browse."
        };
        view.child(panel(t).child(muted(message, t).wrap()))
    }
}

fn search_field(
    key: &str,
    label: &str,
    edit: &TextEdit,
    k: &Kit,
    access: fn(&mut State) -> &mut TextEdit,
) -> El {
    TextInput::new(edit)
        .placeholder(label)
        .height(34.0)
        .width(None)
        .on_edit(access)
        .on_change(State::search_after_edit)
        .on_submit(|s: &mut State, _| s.search())
        .build(k)
        .border(
            1.0,
            if edit.focused {
                k.c.border_focus
            } else {
                k.c.border_base
            },
        )
        .key(key)
        .grow(1.0)
        .shrink()
}

fn source_button(s: &State, source: Source, t: Theme) -> El {
    let selected = s.browser.source == source;
    row()
        .key(format!("source-{source}"))
        .role("button")
        .label(source_name(source))
        .focusable(true)
        .focus_selected(selected)
        .items_center()
        .gap(7.0)
        .px(10.0)
        .min_h(34.0)
        .radius(7.0)
        .fill(if selected {
            oklch(0.49, 0.16, 282.0)
        } else {
            t.card
        })
        .border(1.0, t.separator)
        .hover(t.hover)
        .child(super::source_icon(s, source, 16.0))
        .child(
            text(source_name(source))
                .font(type_::LABEL)
                .color(if selected {
                    oklch(0.99, 0.0, 282.0)
                } else {
                    t.text
                }),
        )
        .on_press_tx(move |s: &mut State, tx| s.change_source(source, tx))
}
fn results(s: &State, page: &SearchPage, t: Theme) -> El {
    let enabled = s.catalog_busy.is_none() && s.browser.pending_search.is_none();
    let position = page.total_pages.map_or_else(
        || format!("Page {}", page.page),
        |pages| format!("Page {} of {}", page.page, pages.max(1)),
    );
    let summary = match page.total {
        Some(total) => format!("{position}  ·  {total} results"),
        None => position,
    };
    let nav = row()
        .w_full()
        .justify_between()
        .items_center()
        .child(muted(summary, t))
        .child(
            row()
                .gap(6.0)
                .child(button(
                    "page-prev",
                    "Previous",
                    t,
                    false,
                    enabled && page.page > 1,
                    |s, tx| s.page(false, tx),
                ))
                .child(button(
                    "page-next",
                    "Next",
                    t,
                    false,
                    enabled && page.has_more,
                    |s, tx| s.page(true, tx),
                )),
        );
    let body = if page.items.is_empty() {
        let message = if page.has_more {
            "No matches in this part of the catalog. Next continues the search."
        } else {
            "No matching mods on this page."
        };
        panel(t).child(muted(message, t))
    } else {
        let mut grid = row().w_full().flex_wrap().gaps(12.0, 12.0);
        for item in &page.items {
            grid = grid.child(if s.browser.may_show_previous_cards() {
                card(s, item, t)
            } else {
                el().w(248.0).h(210.0).radius(12.0).fill(t.card)
            });
        }
        grid
    };
    let dim = s.browser.show_search_progress;
    col()
        .key("explore-results")
        .w_full()
        .gap(14.0)
        .child(
            body.key("results-body")
                .opacity(if dim { 0.48 } else { 1.0 }),
        )
        .child(nav)
}

fn card(s: &State, item: &SearchResult, t: Theme) -> El {
    let key = id_key(&item.id);
    let id = item.id.clone();
    let enabled = s.busy.is_none();
    let saved = s.snapshot.favorites.contains(&id);
    let installed = s
        .snapshot
        .mods
        .iter()
        .find(|record| record.release().is_some_and(|release| release.id == id));
    let mut state = Vec::new();
    if let Some(record) = installed {
        state.push(if record.enabled {
            "Enabled"
        } else {
            "Disabled"
        });
    }
    if saved {
        state.push("Saved");
    }
    if item.adult == Some(true) {
        state.push("NSFW");
    }
    let mut meta = Vec::new();
    let sort = s
        .browser
        .shown_query
        .as_ref()
        .map_or(Sort::Relevance, |query| query.sort);
    match sort {
        Sort::Popularity => {
            let count = match item.id.source {
                Source::Nexus => item.endorsements.map(|n| format!("{n} endorsements")),
                Source::GameBanana => item.likes.map(|n| format!("{n} likes")),
                Source::Local => None,
            };
            if let Some(value) = count {
                meta.push(value);
            }
        }
        Sort::Newest => {
            if let Some(date) = item.published_at.as_deref().and_then(short_date) {
                meta.push(format!("Published {date}"));
            }
        }
        Sort::Updated => {
            if let Some(date) = item.updated_at.as_deref().and_then(short_date) {
                meta.push(format!("Updated {date}"));
            }
        }
        Sort::Relevance => {}
    }
    if let Some(downloads) = item.downloads {
        meta.push(format!("{downloads} downloads"));
    }
    if sort != Sort::Updated
        && let Some(updated) = item.updated_at.as_deref().and_then(short_date)
    {
        meta.push(format!("Updated {updated}"));
    }
    let mut card = col()
        .key(format!("result-{key}"))
        .role("button")
        .label(&item.name)
        .focusable(enabled)
        .disabled(!enabled)
        .w(248.0)
        .padding(9.0)
        .gap(7.0)
        .radius(12.0)
        .fill(t.card)
        .hover_group()
        .border(1.0, t.card_border)
        .child(preview(
            s,
            item.image.as_deref(),
            128.0,
            SurfaceFit::Fill,
            t,
        ))
        .child(
            text(&item.name)
                .font(TextStyle::new(14.0, 680.0))
                .color(t.text)
                .w_full()
                .truncate(),
        )
        .child(
            muted(
                item.author
                    .as_deref()
                    .or(item.category.as_deref())
                    .unwrap_or("Mod"),
                t,
            )
            .w_full()
            .truncate(),
        );
    if !state.is_empty() {
        card = card.child(muted(state.join("  ·  "), t).w_full().truncate());
    }
    if !meta.is_empty() {
        card = card.child(muted(meta.join("  ·  "), t).w_full().truncate());
    }
    let card_id = item.id.clone();
    let actions = row()
        .pin(None, Some(16.0), Some(16.0), None)
        .group_opacity(0.0, 1.0)
        .gap(6.0)
        .child(button(
            format!("result-save-{key}"),
            if saved { "Unsave" } else { "Save" },
            t,
            saved,
            enabled,
            move |s, _| s.favorite(id.clone(), !saved),
        ))
        .child(button(
            format!("result-install-{key}"),
            "Install",
            t,
            false,
            enabled && s.matches_instance(&item.id),
            {
                let id = item.id.clone();
                move |s, _| s.details_for_install(id.clone())
            },
        ));
    card = card.child(actions);
    if enabled {
        card = card
            .hover(t.hover)
            .on_press(move |s: &mut State| s.details(card_id.clone()));
    }
    card
}

fn short_date(value: &str) -> Option<&str> {
    let date = value.get(..10)?;
    (date.as_bytes().get(4) == Some(&b'-') && date.as_bytes().get(7) == Some(&b'-')).then_some(date)
}

fn handoff(s: &State, t: Theme) -> El {
    let enabled = s.busy.is_none();
    let mut view = panel(t).child(text("Nexus browser download").font(type_::TITLE).color(t.text))
        .child(muted("Complete the pending request with Mod Manager Download, then paste the nxm:// link. Uses the same NEXUS_API_KEY as the CLI.", t).wrap());
    if let Some(url) = s.browser.handoff.clone() {
        view = view.child(button(
            "open-handoff",
            "Open Nexus file page",
            t,
            true,
            enabled,
            move |s, tx| open_page(s, tx, url.as_str()),
        ));
    }
    view.child(
        row()
            .w_full()
            .gap(8.0)
            .child(
                field("nxm-uri", "Paste nxm:// link", &s.browser.nxm, t, |s| {
                    &mut s.browser.nxm
                })
                .on_submit(|s: &mut State, _| s.redeem_link()),
            )
            .child(button(
                "receive-nxm",
                "Receive",
                t,
                false,
                enabled && nonempty(&s.browser.nxm.text).is_some(),
                |s, _| s.redeem_link(),
            )),
    )
}

fn preview(s: &State, url: Option<&str>, height: f32, fit: SurfaceFit, t: Theme) -> El {
    if let Some(previews) = &s.previews {
        previews.view(url, height, fit, t)
    } else {
        el().w_full()
            .h(height)
            .fill(t.control_off)
            .center()
            .child(muted("Preview unavailable", t))
    }
}

fn page_url(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

pub(super) fn open_page(s: &mut State, tx: &mut Tx, raw: &str) {
    let Some(url) = page_url(raw) else {
        s.notice = "Source link is not a valid HTTP or HTTPS URL.".into();
        return;
    };
    if let Err(error) = tx.open_url(url.as_str()) {
        s.notice = error.to_string();
    }
}

fn nonempty(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then_some(s)
}

fn id_key(id: &RemoteMod) -> String {
    format!("{}-{}-{}", id.source, id.game, id.id)
}

fn source_name(source: Source) -> &'static str {
    match source {
        Source::Nexus => "Nexus Mods",
        Source::GameBanana => "GameBanana",
        Source::Local => "Local file",
    }
}

fn sort_name(sort: Sort) -> &'static str {
    match sort {
        Sort::Relevance => "Relevance",
        Sort::Popularity => "Popular",
        Sort::Newest => "Newest",
        Sort::Updated => "Recently updated",
    }
}

#[cfg(test)]
mod tests {
    use rune::{Env, Runtime};

    use super::*;

    #[test]
    fn clearing_filters_queues_page_one_without_overwriting_sfw_or_all() {
        for (before, after) in [
            (Maturity::Safe, Maturity::Safe),
            (Maturity::All, Maturity::All),
            (Maturity::Adult, Maturity::All),
        ] {
            let mut state = State::new(PathBuf::from("unused"));
            state.catalog_busy = Some("Searching mods…");
            state.browser.maturity = before;
            state.browser.query = TextEdit::new("suit");
            state.browser.sort = Sort::Popularity;
            state.browser.category = Some("3".into());
            state.browser.author = TextEdit::new("vendor");
            state.browser.provider_filters.published_within = Some(TimeWindow::Month);
            state.clear_filters();
            let (_, pending) = state
                .browser
                .pending_search
                .expect("clear must schedule a query");
            assert_eq!(
                pending,
                SearchQuery {
                    text: "suit".into(),
                    page: 1,
                    maturity: after,
                    sort: Sort::Popularity,
                    ..SearchQuery::default()
                }
            );
        }
    }

    #[test]
    fn slow_search_dims_in_place_and_a_cache_hit_restores_full_opacity() {
        let mut state = State::new(PathBuf::from("unused"));
        let query = state.query_for_controls();
        state.browser.remember_page(
            state.origin().unwrap().clone(),
            query.clone(),
            SearchPage {
                items: vec![],
                page: 1,
                total: Some(40),
                total_pages: Some(2),
                has_more: true,
            },
        );
        let mut runtime = Runtime::new(
            state,
            super::super::view,
            (960.0, 620.0),
            Env::new(true, 1.0),
        );
        runtime.frame(0.0);
        let pager = runtime.presented("page-next").unwrap().0;
        runtime.update(|s, tx| {
            s.browser.searching_since = Some(Instant::now() - SEARCH_INDICATOR_DELAY);
            s.browser.poll_search_progress(true, tx);
        });
        for _ in 0..30 {
            runtime.frame(1.0 / 60.0);
        }
        assert!(runtime.presented("results-body").unwrap().1 < 0.6);
        assert_eq!(runtime.presented("page-next").unwrap().0, pager);
        runtime.update(move |s, _| {
            s.run_query(query.clone(), Duration::ZERO, false);
        });
        for _ in 0..30 {
            runtime.frame(1.0 / 60.0);
        }
        assert!(runtime.presented("results-body").unwrap().1 > 0.95);
        assert_eq!(runtime.presented("page-next").unwrap().0, pager);
    }
}
