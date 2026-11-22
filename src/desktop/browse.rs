pub(super) mod details;
mod explore;
mod filters;

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use dmm::library::{Library, ManagedMod};
use dmm::sources::{
    FilterCatalog, Maturity, ModDetails, ModFile, ProviderFilters, RemoteMod, SearchPage,
    SearchQuery, Sort, Source, TimeWindow,
};
use rune::{TextEdit, Tx};
use rune_kit::ImageZoom;

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
    pub(super) detail: Option<Detail>,
    install_into: TextEdit,
    pub(super) nxm: TextEdit,
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
            nxm: TextEdit::default(),
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
    selected_files: BTreeSet<u64>,
    missing_previous_files: bool,
    install_open: bool,
    full_image_open: bool,
    pub(super) zoom: ImageZoom,
}

impl Detail {
    pub(super) fn picked_files(&self) -> impl Iterator<Item = &ModFile> {
        self.info
            .files
            .iter()
            .filter(|file| self.selected_files.contains(&file.id))
    }

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
        let mut main = info.files.iter().filter(|file| file.primary);
        let primary = main.next().filter(|_| main.next().is_none());
        let selected_files = if info.files.len() == 1 {
            info.files.first()
        } else {
            primary
        }
        .map(|file| file.id)
        .into_iter()
        .collect();
        self.install_into = TextEdit::default();
        self.detail = Some(Detail {
            selected_files,
            missing_previous_files: false,
            info,
            image: 0,
            description,
            install_open: false,
            full_image_open: false,
            zoom: ImageZoom::new(),
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
        self.load_details(id, false);
    }

    pub(super) fn details_for_install(&mut self, id: RemoteMod) {
        self.load_details(id, true);
    }

    fn load_details(&mut self, id: RemoteMod, install: bool) {
        let screen = self.screen;
        let revision = self.detail_revision;
        let search_revision = self.browser.search_revision;
        let local_detail = self.local_detail.clone();
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
            Ok(Update::Details(
                screen,
                revision,
                search_revision,
                local_detail,
                install,
                result,
            ))
        });
    }

    pub(super) fn unsave(&mut self, id: RemoteMod) {
        self.change_bookmark(move |library| library.unsave(&id));
    }

    fn toggle_bookmark(&mut self, id: RemoteMod, name: String, image: Option<String>, saved: bool) {
        if saved {
            self.unsave(id);
        } else {
            self.change_bookmark(move |library| {
                library.save_favorite(&id, &name, image.as_deref())
            });
        }
    }

    fn change_bookmark(
        &mut self,
        change: impl FnOnce(&Library) -> anyhow::Result<()> + Send + 'static,
    ) {
        let target = self.snapshot.selected.as_ref().map(|i| i.name.clone());
        self.submit("Updating saved mods…", move |dir| {
            let library = super::open_library(&dir)?;
            change(&library)?;
            Ok(Update::Library(Snapshot::load(
                &library,
                target.as_deref(),
            )?))
        });
    }

    pub(super) fn matches_instance(&self, id: &RemoteMod) -> bool {
        self.snapshot.selected.is_some()
            && self
                .snapshot
                .sources
                .iter()
                .any(|o| o.source == id.source && o.game == id.game)
    }

    pub(super) fn installed_for(&self, id: &RemoteMod) -> Option<&ManagedMod> {
        self.snapshot
            .mods
            .iter()
            .find(|record| record.release().is_some_and(|release| release.id == *id))
    }

    pub(super) fn show_online_details(&mut self, info: ModDetails) {
        let installed_files = self.installed_for(&info.id).map(|record| {
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
                    .collect::<BTreeSet<_>>()
            }
        });
        self.browser.show_details(info);
        if let Some(ids) = installed_files
            && let Some(detail) = &mut self.browser.detail
        {
            detail.missing_previous_files = ids
                .iter()
                .any(|id| !detail.info.files.iter().any(|file| file.id == *id));
            detail.selected_files = if detail.missing_previous_files {
                BTreeSet::new()
            } else {
                ids
            };
        }
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
        let files: Vec<_> = detail.picked_files().cloned().collect();
        if files.is_empty() {
            return;
        }
        let info = detail.info.clone();
        let into = nonempty(&self.browser.install_into.text).map(PathBuf::from);
        let keep_disabled = self
            .installed_for(&info.id)
            .map_or(!self.preferences.enable_new_mods, |record| !record.enabled);
        if self.enqueue_install(target, info, files, into, keep_disabled) {
            details::close_install(self);
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

fn nonempty(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then_some(s)
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
        let mut state = State::new(PathBuf::from("unused"));
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
