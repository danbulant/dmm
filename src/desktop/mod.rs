mod backend;
mod browse;
mod components;
mod download_queue;
mod download_recovery;
mod game_setup;
mod installations;
mod mod_card;
mod preferences;
mod previews;
mod shell;
mod workbench;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::mpsc;
use std::time::Instant;

use anyhow::Result;
use dmm::acquisition::pending_pages;
use dmm::games::discovery::{DetectedGame, Discovery, discover};
use dmm::games::zzz::launch_xxmi;
#[cfg(target_os = "linux")]
use dmm::games::zzz::steam_xxmi::{InstallStage, Protontricks, SteamXxmi};
use dmm::games::{GameKind, GameProfile, launch_profile, profile};
use dmm::library::{Instance, LaunchSettings, Library, ManagedMod, SavedFavorite};
use dmm::sources::{RemoteMod, Source};
use rune::{App, AppSpec, Pixels, Tx, Waker};
use rune_kit::shell::PaneState;
use url::Url;

use backend::{Backend, Update, Worker, open_library};
use browse::Browser;
use components::icon;
use download_queue::Queue;
use game_setup::SetupForm;
#[cfg(target_os = "linux")]
use game_setup::SteamDialog;
use preferences::Preferences;
use previews::{Previews, Size};
use shell::view;
use workbench::LocalForm;

const START_SIZE: (f32, f32) = (1160.0, 780.0);

pub(crate) fn run(state_dir: PathBuf) {
    rune::set_app_id("com.dmm.ModManager");
    let mut spec = AppSpec::new(
        "DMM · Mod Manager",
        START_SIZE,
        || State::new(PathBuf::from(".dmm")),
        view,
    );
    spec.min_size = Some((880.0, 620.0));
    spec.setup = Some(State::setup);
    spec.poll = Some(State::poll);
    App::new(spec)
        .init(move || State::new(state_dir))
        .args(Vec::new())
        .run();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Explore,
    Local,
    Setup,
    Downloads,
}

#[derive(Clone, PartialEq, Eq)]
struct Origin {
    source: Source,
    game: String,
}

#[derive(Default)]
struct Snapshot {
    instances: Vec<Instance>,
    selected: Option<Instance>,
    profiles: BTreeMap<String, Result<GameProfile, String>>,
    mods: Vec<ManagedMod>,
    favorites: Vec<SavedFavorite>,
    nexus_pages: Vec<Url>,
    sources: Vec<Origin>,
    profile_issue: Option<String>,
    launch: LaunchSettings,
}

impl Snapshot {
    fn load(library: &Library, selected: Option<&str>) -> Result<Self> {
        let instances = library.instances()?;
        let selected = selected
            .and_then(|name| instances.iter().find(|i| i.name == name))
            .cloned();
        let mods = selected
            .as_ref()
            .map(|i| library.mods(&i.name))
            .transpose()?
            .unwrap_or_default();
        let profiles: BTreeMap<_, _> = instances
            .iter()
            .map(|instance| {
                (
                    instance.name.clone(),
                    instance.profile().map_err(|error| format!("{error:#}")),
                )
            })
            .collect();
        let launch = selected
            .as_ref()
            .map(|i| library.launch_settings(&i.name))
            .transpose()?
            .unwrap_or_default();
        let nexus_pages = if let Some(instance) = &selected {
            pending_pages(library, &instance.name)?
        } else {
            Vec::new()
        };
        let (sources, profile_issue) = match selected.as_ref().and_then(|i| profiles.get(&i.name)) {
            Some(Ok(game)) => (origins(game), None),
            Some(Err(error)) => (Vec::new(), Some(format!("Game profile: {error}"))),
            None => (Vec::new(), None),
        };
        Ok(Self {
            instances,
            selected,
            profiles,
            mods,
            favorites: library.saved_favorites()?,
            nexus_pages,
            sources,
            profile_issue,
            launch,
        })
    }

    fn is_saved(&self, id: &RemoteMod) -> bool {
        self.favorites.iter().any(|saved| &saved.item == id)
    }
}

fn origins(game: &GameProfile) -> Vec<Origin> {
    [Source::GameBanana, Source::Nexus]
        .into_iter()
        .filter_map(|source| {
            Some(Origin {
                source,
                game: game.source_game(source)?.to_owned(),
            })
        })
        .collect()
}

fn public_origins(kind: GameKind) -> Vec<Origin> {
    profile(kind).map(|game| origins(&game)).unwrap_or_default()
}

struct State {
    state_dir: PathBuf,
    worker: Option<Worker>,
    catalog_worker: Option<Worker>,
    details_worker: Option<Worker>,
    details_busy: Option<&'static str>,
    detail_revision: u64,
    previews: Option<Previews>,
    busy: Option<&'static str>,
    queue: Queue,
    handoff_open: bool,
    preferences: Preferences,
    catalog_busy: Option<&'static str>,
    notice: String,
    screen: Screen,
    snapshot: Snapshot,
    game_menu: bool,
    window_size: [f32; 2],
    sidebar: PaneState,
    detected: Vec<DetectedGame>,
    zzz_steam_roots: Vec<PathBuf>,
    #[cfg(target_os = "linux")]
    steam_xxmi: Vec<SteamXxmi>,
    #[cfg(target_os = "linux")]
    protontricks: Option<Protontricks>,
    discovery_notices: Vec<(GameKind, String)>,
    cyberpunk_icon: Option<Pixels>,
    zzmi_icon: Option<Pixels>,
    gamebanana_icon: Option<Pixels>,
    nexus_icon: Option<Pixels>,
    public_game: GameKind,
    public_sources: Vec<Origin>,
    browser: Browser,
    local: LocalForm,
    local_detail: Option<String>,
    setup: SetupForm,
}

impl State {
    fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            worker: None,
            catalog_worker: None,
            details_worker: None,
            details_busy: None,
            detail_revision: 0,
            previews: None,
            busy: None,
            queue: Queue::default(),
            handoff_open: false,
            preferences: Preferences::default(),
            catalog_busy: None,
            notice: String::new(),
            screen: Screen::Explore,
            snapshot: Snapshot::default(),
            game_menu: false,
            window_size: [START_SIZE.0, START_SIZE.1],
            sidebar: PaneState::new(245.0).range(180.0, 340.0),
            detected: Vec::new(),
            zzz_steam_roots: Vec::new(),
            #[cfg(target_os = "linux")]
            steam_xxmi: Vec::new(),
            #[cfg(target_os = "linux")]
            protontricks: None,
            discovery_notices: Vec::new(),
            cyberpunk_icon: icon(include_bytes!("icons/cyberpunk.png")),
            zzmi_icon: icon(include_bytes!("icons/zzmi.png")),
            gamebanana_icon: icon(include_bytes!("icons/gamebanana.png")),
            nexus_icon: icon(include_bytes!("icons/nexus.png")),
            public_game: GameKind::Cyberpunk,
            public_sources: public_origins(GameKind::Cyberpunk),
            browser: Browser::default(),
            local: LocalForm::default(),
            local_detail: None,
            setup: SetupForm::default(),
        }
    }

    fn setup(&mut self, wake: Waker) {
        match Worker::start("dmm-library", self.state_dir.clone(), wake.clone()) {
            Ok(worker) => self.worker = Some(worker),
            Err(error) => {
                self.notice = format!("Cannot start background worker: {error:#}");
                return;
            }
        }
        match Worker::start("dmm-catalog", self.state_dir.clone(), wake.clone()) {
            Ok(worker) => self.catalog_worker = Some(worker),
            Err(error) => {
                self.notice = format!("Cannot start catalog worker: {error:#}");
                return;
            }
        }
        match Worker::start("dmm-details", self.state_dir.clone(), wake.clone()) {
            Ok(worker) => self.details_worker = Some(worker),
            Err(error) => self.notice = format!("Cannot start mod details worker: {error:#}"),
        }
        match Preferences::load(&self.state_dir) {
            Ok(preferences) => {
                let game = preferences.game;
                self.preferences = preferences;
                self.public_game = game;
                self.public_sources = public_origins(game);
                self.browser.source = game.preferred_source();
                self.browser.reset_origin();
                if let Some(origin) = self.origin().cloned() {
                    self.restore_browse_preferences(&origin);
                }
            }
            Err(error) => self.notice = format!("{error:#}"),
        }
        self.previews = Previews::start(&self.state_dir, wake.clone()).ok();
        match self.queue.start(&self.state_dir, wake) {
            Ok(paths) => self.discover_games(paths),
            Err(error) => {
                self.detect_games();
                self.notice = format!("Cannot start downloads: {error:#}");
            }
        }
    }

    fn submit(
        &mut self,
        label: &'static str,
        job: impl FnOnce(PathBuf) -> Result<Update> + Send + 'static,
    ) {
        self.submit_job(label, move |worker| job(worker.state_dir.clone()));
    }

    fn submit_details(
        &mut self,
        job: impl FnOnce(&mut Backend) -> Result<Update> + Send + 'static,
    ) {
        if self.details_busy.is_some() {
            return;
        }
        if self
            .details_worker
            .as_ref()
            .is_some_and(|worker| worker.jobs.send(Box::new(job)).is_ok())
        {
            self.details_busy = Some("Loading mod details…");
            self.notice.clear();
        } else {
            self.notice = "The mod details worker is unavailable.".into();
        }
    }

    fn submit_job(
        &mut self,
        label: &'static str,
        job: impl FnOnce(&mut Backend) -> Result<Update> + Send + 'static,
    ) {
        if self.busy.is_some() {
            return;
        }
        let Some(worker) = &self.worker else {
            self.notice = "The background worker is unavailable. Restart DMM to try again.".into();
            return;
        };
        match worker.jobs.send(Box::new(job)) {
            Ok(()) => {
                self.busy = Some(label);
                self.notice.clear();
            }
            Err(_) => {
                self.notice = "The background worker stopped. Restart DMM to try again.".into()
            }
        }
    }

    fn submit_catalog(
        &mut self,
        label: &'static str,
        job: impl FnOnce(&mut Backend) -> Result<Update> + Send + 'static,
    ) -> bool {
        if self.catalog_busy.is_some() {
            return false;
        }
        if self
            .catalog_worker
            .as_ref()
            .is_some_and(|worker| worker.jobs.send(Box::new(job)).is_ok())
        {
            self.catalog_busy = Some(label);
            true
        } else {
            self.notice = "The catalog worker is unavailable. Restart DMM to try again.".into();
            false
        }
    }

    fn detect_games(&mut self) {
        self.discover_games(Vec::new());
    }

    fn discover_games(&mut self, recovery_paths: Vec<PathBuf>) {
        let kind = self.game_kind();
        self.submit("Detecting game installations…", move |dir| {
            let mut discovery = discover();
            let library = open_library(&dir)?;
            if let Err(error) = download_recovery::save(&library, &recovery_paths) {
                discovery
                    .notices
                    .push((kind, format!("Recovering unfinished downloads: {error:#}")));
            }
            let active = match installations::resolve(&library, &discovery.games, kind) {
                Ok(name) => name,
                Err(error) => {
                    discovery
                        .notices
                        .push((kind, format!("Automatic setup: {error:#}")));
                    None
                }
            };
            Ok(Update::Discovered(
                Snapshot::load(&library, active.as_deref())?,
                discovery,
            ))
        });
    }

    fn apply_discovery(&mut self, discovery: Discovery) {
        self.detected = discovery.games;
        self.zzz_steam_roots = discovery.zzz_steam_roots;
        self.discovery_notices = discovery.notices;
        #[cfg(target_os = "linux")]
        {
            self.steam_xxmi = discovery.steam_xxmi;
            self.protontricks = discovery.protontricks;
        }
    }

    fn use_installation(&mut self, path: PathBuf) {
        let kind = self.game_kind();
        self.submit("Selecting installation…", move |dir| {
            let library = open_library(&dir)?;
            let name = installations::select(&library, kind, &path)?;
            Ok(Update::Library(Snapshot::load(&library, Some(&name))?))
        });
    }

    fn launch_game(&mut self) {
        let Some(instance) = self.snapshot.selected.clone() else {
            return;
        };
        let LaunchSettings { xxmi, wine_prefix } = self.snapshot.launch.clone();
        self.submit("Launching game…", move |_| {
            let profile = instance.profile()?;
            launch_profile(
                &profile,
                &instance.root,
                xxmi.as_deref(),
                None,
                wine_prefix.as_deref(),
                &[],
            )?
            .spawn()?;
            Ok(Update::Message("Game launch requested".into()))
        });
    }

    fn open_xxmi(&mut self) {
        let root = self
            .snapshot
            .selected
            .as_ref()
            .map(|instance| instance.root.clone());
        let LaunchSettings { xxmi, wine_prefix } = self.snapshot.launch.clone();
        self.submit("Starting XXMI…", move |_| {
            launch_xxmi(root.as_deref(), xxmi.as_deref(), wine_prefix.as_deref())?.spawn()?;
            Ok(Update::Message("XXMI launch requested".into()))
        });
    }

    #[cfg(target_os = "linux")]
    fn install_steam_xxmi(&mut self, mut target: SteamXxmi) {
        let Some(runner) = self.protontricks.clone() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let (updates, progress) = mpsc::channel();
        self.submit(
            "Setting up XXMI and Visual C++ in Steam Proton…",
            move |_| {
                let result = target
                    .install(&runner, |stage| {
                        let _ = updates.send(stage);
                    })
                    .map(|()| {
                        target.installed = true;
                        target
                    });
                Ok(Update::SteamXxmiInstalled(result))
            },
        );
        self.setup.steam_dialog = Some(if self.busy.is_some() {
            SteamDialog::Running {
                stage: InstallStage::Preparing,
                progress,
            }
        } else {
            SteamDialog::Complete(Err(self.notice.clone()))
        });
    }

    #[cfg(target_os = "linux")]
    fn open_steam_xxmi(&mut self, target: SteamXxmi) {
        let Some(runner) = self.protontricks.clone() else {
            return;
        };
        self.submit("Starting XXMI through Proton…", move |_| {
            target.start_launcher(&runner)?;
            Ok(Update::Message("XXMI launch requested".into()))
        });
    }

    fn set_screen(&mut self, screen: Screen, tx: &mut Tx) {
        let previous = self.screen;
        self.screen = screen;
        self.detail_revision = self.detail_revision.wrapping_add(1);
        self.game_menu = false;
        self.local.close_menus();
        self.browser.close_menus();
        self.browser.detail = None;
        self.local_detail = None;
        self.sync_previews();
        tx.scroll_to_instant("main-scroll", 0.0, 0.0);
        if screen == Screen::Explore {
            if previous == Screen::Explore {
                self.explore_on_open();
            } else {
                self.search();
            }
        }
    }

    fn select_game(&mut self, kind: GameKind) {
        self.game_menu = false;
        if self.public_game == kind || self.busy.is_some() {
            return;
        }
        let detected = self.detected.clone();
        self.submit("Switching games…", move |dir| {
            let library = open_library(&dir)?;
            let active = installations::resolve(&library, &detected, kind)?;
            Ok(Update::GameChanged(
                kind,
                Snapshot::load(&library, active.as_deref())?,
            ))
        });
    }

    fn set_enable_new_mods(&mut self, enabled: bool) {
        self.preferences.enable_new_mods = enabled;
        self.save_preferences();
    }

    fn save_preferences(&mut self) {
        if let Err(error) = self.preferences.save(&self.state_dir) {
            self.notice = format!("Could not save desktop preferences: {error:#}");
        }
    }

    fn open_installed_details(&mut self, slug: &str, tx: &mut Tx) {
        let Some(record) = self.snapshot.mods.iter().find(|record| record.slug == slug) else {
            return;
        };
        self.browser.detail = None;
        if let Some(details) = record.details().cloned() {
            self.browser.show_details(details);
        }
        self.local_detail = Some(slug.to_owned());
        self.sync_previews();
        tx.scroll_to_instant("main-scroll", 0.0, 0.0);
    }

    fn change_origin(&mut self, tx: &mut Tx) {
        self.detail_revision = self.detail_revision.wrapping_add(1);
        let preferred = self.game_kind().preferred_source();
        self.browser.source = self
            .available_sources()
            .iter()
            .find(|o| o.source == preferred)
            .or_else(|| self.available_sources().first())
            .map_or(preferred, |o| o.source);
        self.browser.reset_origin();
        if let Some(origin) = self.origin().cloned() {
            self.restore_browse_preferences(&origin);
        }
        self.sync_previews();
        tx.scroll_to_instant("main-scroll", 0.0, 0.0);
        if self.screen == Screen::Explore {
            self.search();
        }
    }

    fn game_kind(&self) -> GameKind {
        self.public_game
    }

    fn available_sources(&self) -> &[Origin] {
        if self.snapshot.selected.is_some() {
            &self.snapshot.sources
        } else {
            &self.public_sources
        }
    }

    fn origin(&self) -> Option<&Origin> {
        self.available_sources()
            .iter()
            .find(|origin| origin.source == self.browser.source)
    }

    fn sync_previews(&mut self) {
        let Some(previews) = &mut self.previews else {
            return;
        };
        if matches!(
            self.screen,
            Screen::Explore | Screen::Local | Screen::Downloads
        ) && let Some(detail) = &self.browser.detail
        {
            if self.local_detail.is_some() {
                previews.show_saved(browse::details::preview_requests(detail, true));
            } else {
                previews.show(browse::details::preview_requests(detail, false));
            }
        } else if self.screen == Screen::Explore {
            let cards = self
                .browser
                .results
                .iter()
                .flat_map(|page| &page.items)
                .filter(|_| self.browser.may_show_previous_cards())
                .filter_map(|item| item.image.as_deref().map(|url| (url, Size::Card)));
            let icons = self.browser.category_icons().map(|url| (url, Size::Icon));
            previews.show(cards.chain(icons));
        } else if self.screen == Screen::Local {
            let sources = if self.snapshot.selected.is_some() {
                &self.snapshot.sources
            } else {
                &self.public_sources
            };
            previews.show_saved(workbench::preview_requests(
                &self.local,
                &self.snapshot,
                sources,
                &self.queue,
            ));
        } else {
            previews.show([]);
        }
    }
    fn apply_snapshot(&mut self, snapshot: Snapshot, tx: &mut Tx) {
        self.apply_game(self.public_game, snapshot, tx);
    }

    fn apply_game(&mut self, kind: GameKind, snapshot: Snapshot, tx: &mut Tx) {
        let old = self.snapshot.selected.as_ref().map(|i| &i.name);
        let next = snapshot.selected.as_ref().map(|i| &i.name);
        let changed = self.public_game != kind || old != next;
        let sources_changed = self.snapshot.sources != snapshot.sources;
        self.snapshot = snapshot;
        self.public_game = kind;
        if let Some(slug) = &self.local_detail {
            if let Some(record) = self
                .snapshot
                .mods
                .iter()
                .find(|record| !changed && &record.slug == slug)
            {
                let displayed = self.browser.detail.as_ref().map(|detail| &detail.info);
                if record.details() != displayed {
                    self.browser.detail = None;
                    if let Some(details) = record.details().cloned() {
                        self.browser.show_details(details);
                    }
                }
            } else {
                self.local_detail = None;
                self.browser.detail = None;
            }
        }
        if changed {
            self.public_sources = public_origins(kind);
            self.local = LocalForm::default();
            self.setup = SetupForm::default();
            self.change_origin(tx);
        } else if sources_changed {
            self.change_origin(tx);
        }
        self.sync_previews();
    }

    fn poll(&mut self, now: f64, tx: &mut Tx) -> bool {
        let mut changed = false;
        if let Some(due) = self.browser.preference_save_due {
            let remaining = due.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.browser.preference_save_due = None;
                self.save_preferences();
                changed = true;
            } else {
                tx.wake_after(remaining);
            }
        }
        changed |= self.previews.as_mut().is_some_and(Previews::poll);
        changed |= self
            .browser
            .detail
            .as_mut()
            .is_some_and(|detail| detail.poll_zoom(now, tx));
        let updates = [
            (Worker::receive(self.worker.as_ref(), &mut self.busy), true),
            (
                Worker::receive(self.catalog_worker.as_ref(), &mut self.catalog_busy),
                false,
            ),
            (
                Worker::receive(self.details_worker.as_ref(), &mut self.details_busy),
                false,
            ),
        ];
        for (result, local) in updates {
            let Some(result) = result else { continue };
            if local {
                self.notice.clear();
            }
            match result {
                Ok(Update::Discovered(snapshot, discovery)) => {
                    self.apply_discovery(discovery);
                    self.apply_snapshot(snapshot, tx);
                    if self.screen == Screen::Explore {
                        self.explore_on_open();
                    }
                    if self.screen == Screen::Setup {
                        self.notice = format!(
                            "Checked for {} installations.",
                            self.game_kind().display_name()
                        );
                    }
                }
                #[cfg(target_os = "linux")]
                Ok(Update::SteamXxmiInstalled(result)) => {
                    let result = result
                        .map(|target| {
                            if let Some(entry) = self
                                .steam_xxmi
                                .iter_mut()
                                .find(|entry| entry.compatdata == target.compatdata)
                            {
                                entry.installed = true;
                            }
                            target
                        })
                        .map_err(|error| format!("{error:#}"));
                    self.setup.steam_dialog = Some(SteamDialog::Complete(result));
                }
                Ok(Update::GameChanged(kind, snapshot)) => {
                    self.apply_game(kind, snapshot, tx);
                    self.preferences.game = kind;
                    self.save_preferences();
                }
                Ok(Update::Library(snapshot)) => self.apply_snapshot(snapshot, tx),
                Ok(Update::LocalPlan(inspection)) => self.local.apply_preview(inspection),
                Ok(Update::Message(message)) => self.notice = message,
                Ok(Update::Search(revision, query, result))
                    if revision == self.browser.search_revision =>
                {
                    match result {
                        Ok(page) => {
                            if let Some(origin) = self.origin().cloned() {
                                self.browser.remember_page(origin, query, page);
                                self.sync_previews();
                                if self.browser.take_page_scroll() {
                                    tx.scroll_to_instant("main-scroll", 0.0, 0.0);
                                }
                            }
                        }
                        Err(error) => self.notice = format!("{error:#}"),
                    }
                }
                Ok(Update::Filters(origin, result)) if self.origin() == Some(&origin) => {
                    match self.browser.accept_catalog(result) {
                        Ok(()) => self.sync_previews(),
                        Err(error) => self.notice = format!("{error:#}"),
                    }
                }
                Ok(Update::Details(
                    screen,
                    revision,
                    search_revision,
                    local_detail,
                    install,
                    result,
                )) if self.screen == screen
                    && self.detail_revision == revision
                    && (screen != Screen::Explore
                        || self.browser.search_revision == search_revision)
                    && self.local_detail == local_detail =>
                {
                    match result {
                        Ok(details) => {
                            self.local_detail = None;
                            self.show_online_details(details);
                            if install {
                                browse::details::open_install(self);
                            }
                            self.sync_previews();
                            tx.scroll_to_instant("main-scroll", 0.0, 0.0);
                        }
                        Err(error) => self.notice = format!("{error:#}"),
                    }
                }
                Ok(Update::Search(..) | Update::Filters(..) | Update::Details(..)) => {}
                Err(error) => self.notice = format!("{error:#}"),
            }
            changed = true;
        }
        changed |= self.poll_queue(tx);
        #[cfg(target_os = "linux")]
        {
            changed |= self
                .setup
                .poll_steam_dialog(self.busy.is_some(), &self.notice, tx);
        }
        changed
            | self.drive_explore()
            | self
                .browser
                .poll_search_progress(self.catalog_busy == Some("Searching mods…"), tx)
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if self.browser.preference_save_due.is_some()
            && let Err(error) = self.preferences.save(&self.state_dir)
        {
            eprintln!("Could not save desktop preferences while closing: {error:#}");
        }
    }
}
