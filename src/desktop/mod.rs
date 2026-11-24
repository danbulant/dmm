mod backend;
mod browse;
mod components;
mod download_queue;
mod download_recovery;
mod game_setup;
mod installations;
mod mod_card;
#[cfg(target_os = "linux")]
mod nexus_handler;
mod preferences;
mod previews;
mod shell;
mod workbench;
#[path = "../games/xxmi/launch_state.rs"]
mod launch_state;

#[cfg(target_os = "linux")]
pub(crate) fn forward_nexus_handler(state_dir: &std::path::Path, uri: &str) -> Result<()> {
    nexus_handler::forward_from_cli(state_dir, uri)
}

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Result, ensure};
use dmm::acquisition::pending_pages;
use dmm::games::discovery::{DetectedGame, Discovery, discover};
use dmm::games::xxmi::game_process::{RunningGames, running_games};
#[cfg(target_os = "linux")]
use dmm::games::xxmi::steam_xxmi::{Protontricks, SteamXxmi};
use dmm::games::{GameKind, launch};
use dmm::library::{Instance, LaunchSettings, Library, ManagedMod, RemoteModState, SavedFavorite};
use dmm::sources::{RemoteMod, Source};
use rune::{App, AppSpec, Pixels, Tx, Waker};
use rune_kit::shell::PaneState;
use url::Url;

use backend::{Backend, Update, Worker, open_library};
use browse::{Browser, Detail, DetailTarget, InstallDialog};
use components::icon;
use download_queue::Queue;
use game_setup::SetupForm;
#[cfg(target_os = "linux")]
use game_setup::SteamDialog;
use launch_state::{XxmiGameStart, XxmiLaunch};
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
        || unreachable!("the state directory is supplied by App::init"),
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
    mods: Vec<ManagedMod>,
    remote_mods: Vec<RemoteModState>,
    favorites: Vec<SavedFavorite>,
    nexus_pages: Vec<Url>,
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
            .map(|i| library.mods_with_sizes(&i.name))
            .transpose()?
            .unwrap_or_default();
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
        Ok(Self {
            instances,
            selected,
            mods,
            remote_mods: library.remote_mod_states()?,
            favorites: library.saved_favorites()?,
            nexus_pages,
            launch,
        })
    }

    fn is_saved(&self, id: &RemoteMod) -> bool {
        self.favorites.iter().any(|saved| &saved.item == id)
    }

    fn installed(&self, slug: &str) -> Option<&ManagedMod> {
        self.mods.iter().find(|record| record.slug == slug)
    }

    fn installed_remote(&self, id: &RemoteMod) -> Option<&ManagedMod> {
        self.mods
            .iter()
            .find(|record| record.release().is_some_and(|release| release.id == *id))
    }
}

fn public_origins(game: GameKind) -> Vec<Origin> {
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

struct State {
    state_dir: PathBuf,
    worker: Option<Worker>,
    catalog_worker: Option<Worker>,
    details_worker: Option<Worker>,
    details_busy: Option<&'static str>,
    detail_revision: u64,
    previews: Option<Previews>,
    busy: Option<&'static str>,
    xxmi_launch: XxmiLaunch,
    running_games: RunningGames,
    next_game_check: Option<Instant>,
    xxmi_game_start: Option<XxmiGameStart>,
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
    steam_games: Vec<DetectedGame>,
    #[cfg(target_os = "linux")]
    steam_xxmi: Vec<SteamXxmi>,
    #[cfg(target_os = "linux")]
    protontricks: Option<Protontricks>,
    discovery_notices: Vec<(GameKind, String)>,
    cyberpunk_icon: Option<Pixels>,
    zzmi_icon: Option<Pixels>,
    efmi_icon: Option<Pixels>,
    gamebanana_icon: Option<Pixels>,
    nexus_icon: Option<Pixels>,
    public_game: GameKind,
    public_sources: Vec<Origin>,
    browser: Browser,
    local: LocalForm,
    detail: Option<Detail>,
    install_dialog: Option<InstallDialog>,
    setup: SetupForm,
    #[cfg(target_os = "linux")]
    nexus_listener: Option<nexus_handler::Listener>,
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
            xxmi_launch: XxmiLaunch::Idle,
            running_games: RunningGames::default(),
            next_game_check: None,
            xxmi_game_start: None,
            #[cfg(target_os = "linux")]
            nexus_listener: None,
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
            steam_games: Vec::new(),
            #[cfg(target_os = "linux")]
            steam_xxmi: Vec::new(),
            #[cfg(target_os = "linux")]
            protontricks: None,
            discovery_notices: Vec::new(),
            cyberpunk_icon: icon(include_bytes!("icons/cyberpunk.png")),
            zzmi_icon: icon(include_bytes!("icons/zzmi.png")),
            efmi_icon: icon(include_bytes!("icons/efmi.jpg")),
            gamebanana_icon: icon(include_bytes!("icons/gamebanana.png")),
            nexus_icon: icon(include_bytes!("icons/nexus.png")),
            public_game: GameKind::Cyberpunk,
            public_sources: public_origins(GameKind::Cyberpunk),
            browser: Browser::default(),
            local: LocalForm::default(),
            detail: None,
            install_dialog: None,
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
        match self.queue.start(&self.state_dir, wake.clone()) {
            Ok(paths) => {
                self.discover_games(paths);
                #[cfg(target_os = "linux")]
                match nexus_handler::Listener::bind(&self.state_dir) {
                    Ok(listener) => self.nexus_listener = Some(listener),
                    Err(error) => {
                        self.notice = format!("Nexus link handling unavailable: {error:#}")
                    }
                }
            }
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

    fn can_toggle_mod(&self, instance: &str) -> bool {
        self.busy.is_none()
            && !self
                .queue
                .active()
                .is_some_and(|(target, applying)| target == instance && applying)
    }

    fn set_mod_enabled(
        &mut self,
        instance: String,
        slug: String,
        enabled: bool,
        download: Option<RemoteMod>,
    ) {
        if !self.can_toggle_mod(&instance) {
            return;
        }
        let selected = self.snapshot.selected.as_ref().map(|game| game.name.clone());
        self.submit("Updating mod…", move |dir| {
            let mut library = open_library(&dir)?;
            if let Some(id) = download {
                let current = library.managed_mod(&instance, &slug)?;
                ensure!(
                    current.release().is_some_and(|release| release.id == id),
                    "This download no longer matches the installed mod"
                );
            }
            let whole_folder = library.instance(&instance)?.kind.is_xxmi();
            library.toggle(&instance, &slug, enabled, false, false)?;
            if whole_folder {
                Ok(Update::ModRenamed {
                    instance,
                    slug,
                    enabled,
                })
            } else {
                Ok(Update::Library(Snapshot::load(
                    &library,
                    selected.as_deref(),
                )?))
            }
        });
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
        self.steam_games = discovery.steam_games;
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
        if instance.kind.is_xxmi()
            && (self.xxmi_game_start.is_some() || running_games().contains(instance.kind))
        {
            self.next_game_check = None;
            return;
        }
        let LaunchSettings { xxmi, wine_prefix } = self.snapshot.launch.clone();
        self.submit("Launching game…", move |_| {
            let command = launch(
                instance.kind,
                &instance.root,
                xxmi.as_deref(),
                None,
                wine_prefix.as_deref(),
                &[],
            );
            if instance.kind.is_xxmi() && cfg!(target_os = "linux") {
                return Ok(Update::XxmiGameStarted(instance.kind, command.and_then(|command| command.spawn_process())));
            }
            command?.spawn()?;
            Ok(Update::Message("Game launch requested".into()))
        });
    }

    fn set_screen(&mut self, screen: Screen, tx: &mut Tx) {
        let previous = self.screen;
        self.screen = screen;
        self.set_detail(None);
        self.game_menu = false;
        self.local.close_menus();
        self.browser.close_menus();
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

    fn set_detail(&mut self, detail: Option<Detail>) {
        self.detail_revision = self.detail_revision.wrapping_add(1);
        self.detail = detail;
        self.install_dialog = None;
    }

    fn open_installed_details(&mut self, slug: &str, tx: &mut Tx) {
        let Some(record) = self.snapshot.installed(slug) else {
            return;
        };
        self.set_detail(Some(Detail::installed(record)));
        self.sync_previews();
        tx.scroll_to_instant("main-scroll", 0.0, 0.0);
    }

    fn change_origin(&mut self, tx: &mut Tx) {
        self.set_detail(None);
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
        &self.public_sources
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
        let downloads = self
            .queue
            .preview_urls(self.screen == Screen::Downloads)
            .map(|url| (url, Size::Thumbnail));
        if let Some(detail) = &self.detail {
            let saved = matches!(detail.target, DetailTarget::Installed(_));
            let requests = detail
                .info(&self.snapshot)
                .into_iter()
                .flat_map(|info| browse::details::preview_requests(info, detail.image, saved));
            if saved {
                previews.show_saved(downloads.chain(requests));
            } else {
                previews.show(downloads.chain(requests));
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
            previews.show(downloads.chain(cards).chain(icons));
        } else if self.screen == Screen::Local {
            previews.show_saved(downloads.chain(workbench::preview_requests(
                &self.local,
                &self.snapshot,
                &self.public_sources,
                &self.queue,
            )));
        } else {
            previews.show_saved(downloads);
        }
    }
    fn apply_snapshot(&mut self, snapshot: Snapshot, tx: &mut Tx) {
        self.apply_game(self.public_game, snapshot, tx);
    }

    fn apply_game(&mut self, kind: GameKind, snapshot: Snapshot, tx: &mut Tx) {
        let old = self.snapshot.selected.as_ref().map(|i| &i.name);
        let next = snapshot.selected.as_ref().map(|i| &i.name);
        let changed = self.public_game != kind || old != next;
        if !changed
            && let Some(Detail {
                target: DetailTarget::Installed(slug),
                ..
            }) = &self.detail
        {
            match snapshot.installed(slug) {
                Some(record)
                    if record.details()
                        != self.snapshot.installed(slug).and_then(ManagedMod::details) =>
                {
                    self.detail = Some(Detail::installed(record));
                }
                Some(_) => {}
                None => self.set_detail(None),
            }
        }
        self.snapshot = snapshot;
        self.public_game = kind;
        if changed {
            self.public_sources = public_origins(kind);
            self.local = LocalForm::default();
            self.setup = SetupForm::default();
            self.change_origin(tx);
        } else {
            self.sync_previews();
        }
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
        #[cfg(target_os = "linux")]
        if let Some(listener) = self.nexus_listener.take() {
            changed |= listener.poll(|uri| self.enqueue_nexus(uri));
            self.nexus_listener = Some(listener);
            tx.wake_after(std::time::Duration::from_millis(250));
        }
        changed |= self
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
                Ok(Update::Favorites(favorites)) => {
                    self.snapshot.favorites = favorites;
                    self.sync_previews();
                }
                Ok(Update::ModRenamed {
                    instance,
                    slug,
                    enabled,
                }) => {
                    if self
                        .snapshot
                        .selected
                        .as_ref()
                        .is_some_and(|game| game.name == instance)
                        && let Some(record) = self
                            .snapshot
                            .mods
                            .iter_mut()
                            .find(|record| record.slug == slug)
                    {
                        record.enabled = enabled;
                        self.sync_previews();
                    }
                    if let Some(record) = self
                        .snapshot
                        .remote_mods
                        .iter_mut()
                        .find(|record| record.instance == instance && record.slug == slug)
                    {
                        record.enabled = enabled;
                    }
                }
                Ok(Update::LocalPlan(inspection)) => self.local.apply_preview(inspection),
                Ok(Update::Message(message)) => self.notice = message,
                Ok(Update::XxmiStarted(child, exe)) => self.xxmi_launch = XxmiLaunch::Running(child, exe),
                Ok(Update::XxmiGameStarted(kind, result)) => match result {
                    Ok(launcher) => {
                        self.xxmi_game_start = Some(XxmiGameStart::new(kind, launcher));
                        self.next_game_check = None;
                    }
                    Err(error) => self.notice = format!("Could not launch {}: {error:#}", kind.display_name()),
                },
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
                Ok(Update::Details {
                    revision,
                    intent,
                    result,
                }) => {
                    self.apply_details(revision, intent, result, tx);
                }
                Ok(Update::Search(..) | Update::Filters(..)) => {}
                Err(error) if local && matches!(self.xxmi_launch, XxmiLaunch::Starting) => {
                    self.xxmi_launch = XxmiLaunch::Failed(format!("Could not start XXMI: {error:#}"));
                }
                Err(error) => self.notice = format!("{error:#}"),
            }
            changed = true;
        }
        changed |= self.xxmi_launch.poll(tx);
        changed |= self.poll_games(tx);
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
