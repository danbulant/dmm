pub(super) mod details;
mod explore;
mod filters;

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use dmm::library::ManagedMod;
use dmm::sources::{
    FilterCatalog, Maturity, ModDetails, ModFile, ProviderFilters, RemoteMod, SearchPage,
    SearchQuery, Sort, Source, TimeWindow,
};
use rune::{TextEdit, Tx};
use rune_kit::ImageZoom;

use super::preferences::BrowsePreferences;

use super::{Origin, Screen, Snapshot, State, Update};
pub(super) use explore::explore;

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
    pub(super) nxm: TextEdit,
    pub(super) preference_save_due: Option<Instant>,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            source: Source::Nexus,
            query: TextEdit::default(),
            sort: Sort::Newest,
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
            nxm: TextEdit::default(),
            preference_save_due: None,
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

pub(super) struct InstallDialog {
    pub(super) info: ModDetails,
    pub(super) selected_files: BTreeSet<u64>,
    pub(super) missing_previous_files: bool,
    destination: TextEdit,
}

impl InstallDialog {
    pub(super) fn picked_files(&self) -> impl Iterator<Item = &ModFile> {
        self.info
            .files
            .iter()
            .filter(|file| self.selected_files.contains(&file.id))
    }
}

pub(super) enum DetailsIntent {
    View { search_revision: Option<u64> },
    Install,
}

pub(super) enum DetailTarget {
    Online(ModDetails),
    Installed(String),
}

pub(super) struct Detail {
    pub(super) target: DetailTarget,
    pub(super) image: usize,
    description: String,
    full_image_open: bool,
    zoom: ImageZoom,
}

impl Detail {
    pub(super) fn online(info: ModDetails) -> Self {
        let description = Self::description(Some(&info));
        Self::new(DetailTarget::Online(info), description)
    }

    pub(super) fn installed(record: &ManagedMod) -> Self {
        Self::new(
            DetailTarget::Installed(record.slug.clone()),
            Self::description(record.details()),
        )
    }

    fn new(target: DetailTarget, description: String) -> Self {
        Self {
            target,
            description,
            image: 0,
            full_image_open: false,
            zoom: ImageZoom::new(),
        }
    }

    pub(super) fn info<'a>(&'a self, snapshot: &'a Snapshot) -> Option<&'a ModDetails> {
        match &self.target {
            DetailTarget::Online(info) => Some(info),
            DetailTarget::Installed(slug) => snapshot.installed(slug)?.details(),
        }
    }

    fn description(info: Option<&ModDetails>) -> String {
        let raw = info
            .and_then(|info| info.description.as_deref().or(info.summary.as_deref()))
            .unwrap_or_default();
        let message: String = raw.chars().take(5000).collect();
        if message.contains('<') {
            std::mem::take(&mut TextEdit::from_html(&message).text)
        } else {
            message
        }
    }

    pub(super) fn poll_zoom(&mut self, now: f64, tx: &mut Tx) -> bool {
        self.full_image_open && self.zoom.poll(now, tx)
    }
}

impl Browser {
    pub(super) fn reset_origin(&mut self) {
        self.sort = Sort::Newest;
        self.category = None;
        self.category_group = None;
        self.catalog = Catalog::Idle;
        self.pending_search = None;
        self.search_revision += 1;
        self.sort_menu = false;
        self.find_category = TextEdit::default();
        self.maturity = Maturity::Safe;
        self.provider_filters = ProviderFilters::default();
        if self.source == Source::GameBanana {
            self.provider_filters.content_ratings.push("-".to_owned());
        }
        self.author = TextEdit::default();
        self.language = TextEdit::default();
        self.tag = TextEdit::default();
        self.results = None;
        self.shown_query = None;
        self.searching_since = None;
        self.show_search_progress = false;
        self.show_filters = false;
    }

    pub(super) fn restore_preferences(&mut self, preferences: &BrowsePreferences) {
        self.sort = preferences.query.sort;
        self.category = preferences.query.category.clone();
        self.maturity = preferences.query.maturity;
        self.provider_filters = preferences.query.filters.clone();
        self.author = TextEdit::new(
            preferences
                .query
                .filters
                .author
                .as_deref()
                .unwrap_or_default(),
        );
        self.language = TextEdit::new(
            preferences
                .query
                .filters
                .language
                .as_deref()
                .unwrap_or_default(),
        );
        self.tag = TextEdit::new(preferences.query.filters.tag.as_deref().unwrap_or_default());
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
        self.shown_query.as_ref().is_none_or(|query| {
            (self.source != Source::Nexus
                || self.maturity == Maturity::All
                || query.maturity == self.maturity)
                && (self.source != Source::GameBanana
                    || query.filters.content_ratings == self.provider_filters.content_ratings)
        })
    }
}

impl State {
    pub(super) fn explore_on_open(&mut self) {
        if self.browser.results.is_none() {
            self.search();
        }
    }

    pub(super) fn search(&mut self) {
        self.store_browse_preferences();
        self.run_query(self.query_for_controls(), Duration::ZERO, false);
        self.drive_explore();
    }

    fn search_after_edit(&mut self, tx: &mut Tx) {
        self.store_browse_preferences();
        self.run_query(self.query_for_controls(), SEARCH_DEBOUNCE, false);
        tx.wake_after(SEARCH_DEBOUNCE);
    }

    pub(super) fn restore_browse_preferences(&mut self, origin: &Origin) {
        let Some(saved) = self
            .preferences
            .browse
            .iter()
            .find(|saved| {
                saved.game_kind == self.game_kind()
                    && saved.source == origin.source
                    && saved.provider_game == origin.game
            })
            .cloned()
        else {
            return;
        };
        self.browser.restore_preferences(&saved);
    }

    fn store_browse_preferences(&mut self) {
        let Some(origin) = self.origin().cloned() else {
            return;
        };
        let mut query = self.query_for_controls();
        query.text.clear();
        query.page = 1;
        let saved = BrowsePreferences {
            game_kind: self.game_kind(),
            source: origin.source,
            provider_game: origin.game,
            query,
        };
        let has_saved_settings = self.preferences.browse.iter().any(|entry| {
            entry.game_kind == saved.game_kind
                && entry.source == saved.source
                && entry.provider_game == saved.provider_game
        });
        if !has_saved_settings {
            let mut default_filters = ProviderFilters::default();
            if saved.source == Source::GameBanana {
                default_filters.content_ratings.push("-".to_owned());
            }
            let default_query = SearchQuery {
                text: String::new(),
                page: 1,
                category: None,
                sort: Sort::Newest,
                maturity: if saved.source == Source::Nexus {
                    Maturity::Safe
                } else {
                    Maturity::All
                },
                filters: default_filters,
            };
            if saved.query == default_query {
                return;
            }
        }
        let existing = self.preferences.browse.iter_mut().find(|entry| {
            entry.game_kind == saved.game_kind
                && entry.source == saved.source
                && entry.provider_game == saved.provider_game
        });
        if let Some(existing) = existing {
            if existing == &saved {
                return;
            }
            *existing = saved;
        } else {
            self.preferences.browse.push(saved);
        }
        self.browser.preference_save_due = Some(Instant::now() + Duration::from_millis(500));
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
            maturity: if self.browser.source == Source::Nexus {
                self.browser.maturity
            } else {
                Maturity::All
            },
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
            || self.detail.is_some()
        {
            return false;
        }
        let Some(origin) = self.origin().cloned() else {
            return false;
        };
        let needs_filter_labels = self.browser.category.is_some()
            || self.browser.provider_filters.release_type.is_some()
            || self
                .browser
                .provider_filters
                .content_ratings
                .iter()
                .any(|rating| rating != "-");
        if (self.browser.show_filters
            || (self.browser.results.is_some()
                && self.browser.pending_search.is_none()
                && needs_filter_labels))
            && matches!(self.browser.catalog, Catalog::Idle)
        {
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
            self.set_detail(None);
            self.browser.reset_origin();
            if let Some(origin) = self.origin().cloned() {
                self.restore_browse_preferences(&origin);
            }
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
        let preserve_unrated = self.browser.source == Source::GameBanana
            && self.browser.provider_filters.content_ratings.len() == 1
            && self.browser.provider_filters.content_ratings[0] == "-";
        self.browser.provider_filters = ProviderFilters::default();
        if preserve_unrated {
            self.browser
                .provider_filters
                .content_ratings
                .push("-".to_owned());
        }
        self.browser.category = None;
        self.browser.category_group = None;
        self.browser.find_category = TextEdit::default();
        self.browser.author = TextEdit::default();
        self.browser.language = TextEdit::default();
        self.browser.tag = TextEdit::default();
        if self.browser.source == Source::Nexus && self.browser.maturity == Maturity::Adult {
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
        let search_revision =
            (self.screen == Screen::Explore).then_some(self.browser.search_revision);
        self.load_details(id, DetailsIntent::View { search_revision });
    }

    fn loaded_install_details(&self, id: &RemoteMod) -> Option<&ModDetails> {
        match &self.detail.as_ref()?.target {
            DetailTarget::Online(info) if info.id == *id => Some(info),
            _ => None,
        }
    }

    pub(super) fn install_blocker(&self, id: &RemoteMod) -> Option<&'static str> {
        let loaded = self.loaded_install_details(id);
        if loaded.is_some_and(|info| info.files.is_empty()) {
            Some("No downloadable files from this source")
        } else if self.install_target(id).is_none() {
            Some("Select a game installation before installing")
        } else if !self.queue.available() {
            Some("The download queue is unavailable")
        } else if loaded.is_none() && self.details_busy.is_some() {
            Some("Loading mod details…")
        } else {
            None
        }
    }

    pub(super) fn request_install(&mut self, id: &RemoteMod, tx: &mut Tx) {
        if self.pending_install(id).is_some() {
            self.open_downloads(tx);
        } else if self.install_blocker(id).is_none() {
            if let Some(info) = self.loaded_install_details(id).cloned() {
                self.open_install_dialog(info);
            } else {
                self.load_details(id.clone(), DetailsIntent::Install);
            }
        }
    }

    fn load_details(&mut self, id: RemoteMod, intent: DetailsIntent) {
        let revision = self.detail_revision;
        let maturity = (self.screen == Screen::Explore && self.browser.source == Source::Nexus)
            .then_some(self.browser.maturity);
        let listed_rating = self.browser.results.as_ref().and_then(|page| {
            page.items
                .iter()
                .find(|item| item.id == id)
                .and_then(|item| item.adult)
        });
        self.submit_details(move |worker| {
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
            Ok(Update::Details { revision, intent, result })
        });
    }

    pub(super) fn apply_details(
        &mut self,
        revision: u64,
        intent: DetailsIntent,
        result: anyhow::Result<ModDetails>,
        tx: &mut Tx,
    ) {
        if revision != self.detail_revision
            || matches!(intent, DetailsIntent::View { search_revision: Some(search) }
                if search != self.browser.search_revision)
        {
            return;
        }
        let details = match result {
            Ok(details) => details,
            Err(error) => {
                self.notice = format!("{error:#}");
                return;
            }
        };
        match intent {
            DetailsIntent::View { .. } => {
                self.detail = Some(Detail::online(details));
                self.sync_previews();
                tx.scroll_to_instant("main-scroll", 0.0, 0.0);
            }
            DetailsIntent::Install => {
                if self.available_sources().iter().any(|origin| {
                    origin.source == details.id.source && origin.game == details.id.game
                }) {
                    self.open_install_dialog(details);
                }
            }
        }
    }

    pub(super) fn toggle_bookmark(
        &mut self,
        id: RemoteMod,
        name: String,
        image: Option<String>,
    ) {
        let saved = self.snapshot.is_saved(&id);
        self.submit("Updating saved mods…", move |dir| {
            let library = super::open_library(&dir)?;
            if saved {
                library.unsave(&id)?;
            } else {
                library.save_favorite(&id, &name, image.as_deref())?;
            }
            Ok(Update::Favorites(library.saved_favorites()?))
        });
    }

    pub(super) fn install_target(&self, id: &RemoteMod) -> Option<&str> {
        self.snapshot
            .selected
            .as_ref()
            .filter(|instance| instance.kind.source_game(id.source) == Some(id.game.as_str()))
            .map(|instance| instance.name.as_str())
    }

    fn installed_file_ids(&self, id: &RemoteMod) -> Option<BTreeSet<u64>> {
        self.snapshot.installed_remote(id).map(|record| {
            if record.releases().is_empty() {
                record
                    .release()
                    .into_iter()
                    .map(|release| release.file_id)
                    .collect()
            } else {
                record
                    .releases()
                    .iter()
                    .map(|release| release.file_id)
                    .collect()
            }
        })
    }

    fn open_install_dialog(&mut self, info: ModDetails) {
        let (selected_files, missing_previous_files) =
            if let Some(ids) = self.installed_file_ids(&info.id) {
                if ids.iter().all(|id| info.files.iter().any(|file| file.id == *id)) {
                    (ids, false)
                } else {
                    (BTreeSet::new(), true)
                }
            } else {
                let default = match info.files.as_slice() {
                    [only] => Some(only),
                    files => {
                        let mut primary = files.iter().filter(|file| file.primary);
                        primary.next().filter(|_| primary.next().is_none())
                    }
                };
                (default.map(|file| file.id).into_iter().collect(), false)
            };
        self.install_dialog = Some(InstallDialog {
            info,
            selected_files,
            missing_previous_files,
            destination: TextEdit::default(),
        });
    }

    fn install_selected(&mut self) {
        let Some(dialog) = &self.install_dialog else {
            return;
        };
        let Some(target) = self.install_target(&dialog.info.id).map(str::to_owned) else {
            return;
        };
        let files: Vec<_> = dialog.picked_files().cloned().collect();
        if files.is_empty() {
            return;
        }
        let info = dialog.info.clone();
        let into = nonempty(&dialog.destination.text).map(PathBuf::from);
        let keep_disabled = self
            .snapshot
            .installed_remote(&info.id)
            .map_or(!self.preferences.enable_new_mods, |record| !record.enabled);
        if self.enqueue_install(target, info, files, into, keep_disabled) {
            self.install_dialog = None;
        }
    }

    pub(super) fn redeem_link(&mut self) {
        let Some(uri) = nonempty(&self.browser.nxm.text).map(str::to_owned) else {
            return;
        };
        if self.enqueue_nexus(uri) {
            self.handoff_open = false;
            self.browser.nxm = TextEdit::default();
        }
    }

    fn gallery_step(&mut self, index: usize) {
        let count = self
            .detail
            .as_ref()
            .and_then(|detail| detail.info(&self.snapshot))
            .map_or(0, |info| info.images.len());
        if let Some(detail) = &mut self.detail
            && index < count
            && index != detail.image
        {
            detail.image = index;
            detail.zoom.reset();
            self.sync_previews();
        }
    }
}

fn nonempty(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use dmm::games::GameKind;
    use rune::{Env, Runtime};

    use super::super::preferences::Preferences;
    use super::*;

    #[test]
    fn install_dialog_survives_search_and_detail_results_but_not_navigation() {
        let mut state = State::new(PathBuf::from("unused"));
        let info = ModDetails {
            id: RemoteMod {
                source: Source::Nexus,
                game: "cyberpunk2077".into(),
                id: 42,
            },
            name: "Wardrobe".into(),
            author: None,
            category: None,
            tags: Vec::new(),
            summary: None,
            description: None,
            version: None,
            updated_at: None,
            created_at: None,
            url: None,
            adult: Some(false),
            images: Vec::new(),
            requirements: Vec::new(),
            files: vec![ModFile {
                id: 1,
                name: "wardrobe.zip".into(),
                version: None,
                updated_at: None,
                size: None,
                downloads: None,
                primary: true,
                category: None,
                description: None,
                url: None,
            }],
        };
        state.open_install_dialog(info.clone());
        assert!(state.detail.is_none());

        state.browser.query = TextEdit::new("new search");
        state.search();
        assert!(state.install_dialog.is_some());

        let revision = state.detail_revision;
        let intent = DetailsIntent::View { search_revision: Some(state.browser.search_revision) };
        state.apply_details(revision, intent, Ok(info.clone()), &mut Tx::default());
        assert!(state.detail.is_some());
        assert!(state.install_dialog.is_some());
        state.set_screen(Screen::Local, &mut Tx::default());
        state.apply_details(revision, DetailsIntent::Install, Ok(info.clone()), &mut Tx::default());
        assert!(state.install_dialog.is_none());
        state.set_screen(Screen::Explore, &mut Tx::default());
        state.open_install_dialog(info);
        state.apply_game(GameKind::Zzmi, Snapshot::default(), &mut Tx::default());
        assert!(state.install_dialog.is_none());
    }

    #[test]
    fn explore_restores_each_source_on_reopen_even_when_closed_before_debounce()
    -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut state = State::new(dir.path().to_owned());
        assert_eq!(state.query_for_controls().sort, Sort::Newest);
        state.browser.sort = Sort::Popularity;
        state.browser.maturity = Maturity::Adult;
        state.browser.author = TextEdit::new("creator");
        state.browser.query = TextEdit::new("temporary terms");
        state.set_publish_window(Some(TimeWindow::Month));
        state.change_source(Source::GameBanana, &mut Tx::default());
        assert_eq!(state.query_for_controls().sort, Sort::Newest);
        assert_eq!(state.query_for_controls().filters.content_ratings, ["-"]);
        state.browser.sort = Sort::Updated;
        state.browser.provider_filters.content_ratings = vec!["iv".into()];
        state.browser.provider_filters.release_type = Some("indie".into());
        state.category(Some("2700".into()));
        drop(state);

        let mut state = State::new(dir.path().to_owned());
        state.preferences = Preferences::load(dir.path())?;
        let nexus = state.origin().unwrap().clone();
        state.restore_browse_preferences(&nexus);
        let query = state.query_for_controls();
        assert_eq!(query.sort, Sort::Popularity);
        assert_eq!(query.maturity, Maturity::Adult);
        assert_eq!(query.filters.author.as_deref(), Some("creator"));
        assert_eq!(query.filters.published_within, Some(TimeWindow::Month));
        assert!(query.text.is_empty());

        state.change_source(Source::GameBanana, &mut Tx::default());
        let query = state.query_for_controls();
        assert_eq!(query.sort, Sort::Updated);
        assert_eq!(query.category.as_deref(), Some("2700"));
        assert_eq!(query.filters.content_ratings, ["iv"]);
        assert_eq!(query.filters.release_type.as_deref(), Some("indie"));
        assert!(query.filters.author.is_none());
        Ok(())
    }

    #[test]
    fn clearing_filters_queues_page_one_without_overwriting_sfw_or_all() {
        let dir = tempfile::tempdir().unwrap();
        for (before, after) in [
            (Maturity::Safe, Maturity::Safe),
            (Maturity::All, Maturity::All),
            (Maturity::Adult, Maturity::All),
        ] {
            let mut state = State::new(dir.path().to_owned());
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
                .as_ref()
                .expect("clear must schedule a query");
            assert_eq!(
                pending,
                &SearchQuery {
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
    fn gamebanana_queries_skip_nexus_maturity_and_hide_stale_rating_results() {
        let mut state = State::new(PathBuf::from("unused"));
        state.browser.source = Source::GameBanana;
        state.browser.maturity = Maturity::Safe;
        assert_eq!(state.query_for_controls().maturity, Maturity::All);

        state.browser.shown_query = Some(SearchQuery::default());
        state.browser.provider_filters.content_ratings = vec!["-".into()];
        assert!(!state.browser.may_show_previous_cards());

        state
            .browser
            .shown_query
            .as_mut()
            .unwrap()
            .filters
            .content_ratings = vec!["-".into()];
        assert!(state.browser.may_show_previous_cards());
    }

    #[test]
    fn gamebanana_unrated_is_default_and_survives_clear() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::new(dir.path().to_owned());
        state.browser.source = Source::GameBanana;
        state.browser.reset_origin();
        assert_eq!(
            state.browser.provider_filters.content_ratings,
            vec!["-".to_owned()]
        );

        state.browser.provider_filters.content_ratings = vec!["nu".into(), "iv".into()];
        state.clear_filters();
        assert!(state.browser.provider_filters.content_ratings.is_empty());
        state.clear_filters();
        assert!(state.browser.provider_filters.content_ratings.is_empty());

        state.browser.provider_filters.content_ratings = vec!["-".into()];
        state.clear_filters();
        assert_eq!(
            state.browser.provider_filters.content_ratings,
            vec!["-".to_owned()]
        );
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
