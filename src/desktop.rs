mod browse;
mod download_queue;
mod download_recovery;
mod game_setup;
mod inputs;
mod installations;
mod preferences;
mod previews;
mod workbench;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use anyhow::Result;
use dmm::acquisition::{pending_pages, source_client};
#[cfg(target_os = "linux")]
use dmm::games::steam_xxmi::{self, InstallStage, Protontricks, SteamXxmi};
use dmm::games::{
    GameKind, GameProfile, detect, detect_zzz_game, launch_profile, profile, start_xxmi,
};
use dmm::library::{Inspection, Instance, LaunchSettings, Library, ManagedMod, SavedFavorite};
use dmm::sources::{
    FilterCatalog, ModDetails, RemoteMod, SearchPage, SearchQuery, Source, SourceClient,
};
use rune::tok::{self, Theme, type_};
use rune::{
    App, AppSpec, Cx, El, Pixels, ScrollSpec, SurfaceFit, TextEdit, TextStyle, Tx, Waker, col, el,
    oklch, row, text, text_input,
};
use rune_kit::shell::{PaneState, Sidebar, SidebarRow, SplitView, Toolbar};
use rune_kit::{Kit, Menu, MenuItem};
use url::Url;

use browse::Browser;
use download_queue::Queue;
use game_setup::SetupForm;
#[cfg(target_os = "linux")]
use game_setup::SteamDialog;
use preferences::Preferences;
use previews::{Previews, Size};
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

const SUPPORTED: [(GameKind, &str); 2] = [
    (GameKind::Cyberpunk, "Cyberpunk 2077"),
    (GameKind::Zzmi, "Zenless Zone Zero"),
];

#[derive(Clone)]
struct DetectedGame {
    kind: GameKind,
    root: PathBuf,
}

#[derive(Default)]
struct Discovery {
    games: Vec<DetectedGame>,
    zzz_steam_roots: Vec<PathBuf>,
    #[cfg(target_os = "linux")]
    steam_xxmi: Vec<SteamXxmi>,
    #[cfg(target_os = "linux")]
    protontricks: Option<Protontricks>,
    notices: Vec<(GameKind, String)>,
}

fn discover() -> Discovery {
    let mut found = Discovery::default();
    for (kind, label) in SUPPORTED {
        match detect(kind) {
            Ok(roots) => found
                .games
                .extend(roots.into_iter().map(|root| DetectedGame {
                    kind,
                    root: root.canonicalize().unwrap_or(root),
                })),
            Err(error) => found.notices.push((kind, format!("{label}: {error:#}"))),
        }
    }
    match detect_zzz_game() {
        Ok(roots) => {
            found.zzz_steam_roots = roots
                .into_iter()
                .map(|root| root.canonicalize().unwrap_or(root))
                .collect()
        }
        Err(error) => found
            .notices
            .push((GameKind::Zzmi, format!("Steam: {error:#}"))),
    }
    #[cfg(target_os = "linux")]
    if !found.zzz_steam_roots.is_empty() {
        match steam_xxmi::discover() {
            Ok(targets) => found.steam_xxmi = targets,
            Err(error) => found
                .notices
                .push((GameKind::Zzmi, format!("Steam Proton: {error:#}"))),
        }
        if !found.steam_xxmi.is_empty() {
            found.protontricks = Protontricks::find();
        }
    }
    found
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

enum Update {
    Library(Snapshot),
    LocalPlan(Inspection),
    Discovered(Snapshot, Discovery),
    #[cfg(target_os = "linux")]
    SteamXxmiInstalled(Result<SteamXxmi>),
    GameChanged(GameKind, Snapshot),
    Message(String),
    Search(u64, SearchQuery, Result<SearchPage>),
    Filters(Origin, Result<FilterCatalog>),
    Details(Screen, u64, u64, Option<String>, bool, Result<ModDetails>),
}

type Job = Box<dyn FnOnce(&mut Backend) -> Result<Update> + Send>;

struct Backend {
    state_dir: PathBuf,
    online: Option<SourceClient>,
}

impl Backend {
    fn online(&mut self) -> Result<&SourceClient> {
        if self.online.is_none() {
            self.online =
                Some(source_client()?.with_cache(&self.state_dir.join("source-cache.sqlite3")));
        }
        Ok(self.online.as_ref().expect("initialized client"))
    }
}

struct Worker {
    jobs: Sender<Job>,
    results: Receiver<Result<Update>>,
}

impl Worker {
    fn start(name: &str, state_dir: PathBuf, wake: Waker) -> Result<Self> {
        let (jobs, requests) = mpsc::channel::<Job>();
        let (responses, results) = mpsc::channel();
        thread::Builder::new().name(name.into()).spawn(move || {
            let mut backend = Backend {
                state_dir,
                online: None,
            };
            while let Ok(job) = requests.recv() {
                if responses.send(job(&mut backend)).is_err() {
                    break;
                }
                wake();
            }
        })?;
        Ok(Self { jobs, results })
    }

    fn receive(worker: Option<&Self>, busy: &mut Option<&'static str>) -> Option<Result<Update>> {
        match worker?.results.try_recv() {
            Ok(result) => {
                *busy = None;
                Some(result)
            }
            Err(TryRecvError::Disconnected) if busy.take().is_some() => Some(Err(anyhow::anyhow!(
                "The background worker stopped. Restart DMM to try again."
            ))),
            Err(_) => None,
        }
    }
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
            cyberpunk_icon: icon(include_bytes!("desktop/icons/cyberpunk.png")),
            zzmi_icon: icon(include_bytes!("desktop/icons/zzmi.png")),
            gamebanana_icon: icon(include_bytes!("desktop/icons/gamebanana.png")),
            nexus_icon: icon(include_bytes!("desktop/icons/nexus.png")),
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
                self.preferences = preferences;
                self.public_game = preferences.game;
                self.public_sources = public_origins(preferences.game);
                self.browser.source = if preferences.game == GameKind::Zzmi {
                    Source::GameBanana
                } else {
                    Source::Nexus
                };
                self.browser.reset_origin();
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
            start_xxmi(root.as_deref(), xxmi.as_deref(), wine_prefix.as_deref())?.spawn()?;
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
        let preferred = if self.game_kind() == GameKind::Zzmi {
            Source::GameBanana
        } else {
            Source::Nexus
        };
        self.browser.source = self
            .available_sources()
            .iter()
            .find(|o| o.source == preferred)
            .or_else(|| self.available_sources().first())
            .map_or(preferred, |o| o.source);
        self.browser.reset_origin();
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
        let mut changed = self.previews.as_mut().is_some_and(Previews::poll);
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
                        self.notice =
                            format!("Checked for {} installations.", game_name(self.game_kind()));
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

fn view(s: &State, cx: &mut Cx<State>) -> El {
    let dark = cx.system_dark();
    let t = tok::theme(dark);
    let mut k = Kit::neutral(dark);
    k.c.text_faint = k.c.text_muted;
    k.c.border_muted = control_border(dark);
    k.c.border_base = control_border(dark);
    k.c.std_border = control_border(dark);
    k.c.layer_01 = control_background(dark);
    k.c.icon_faint = k.c.icon_muted;
    let mut layer = Vec::new();
    let detail = if self::Screen::Setup == s.screen {
        None
    } else {
        s.browser.detail.as_ref()
    };
    let installed = s.local_detail.as_ref().and_then(|slug| {
        let instance = s.snapshot.selected.as_ref()?;
        let record = s.snapshot.mods.iter().find(|record| &record.slug == slug)?;
        Some((instance, record))
    });
    let content = if let Some((instance, record)) = installed {
        let mut page = col().w_full().gap(18.0);
        if let Some(detail) = detail {
            page = page.child(browse::details::body(s, detail, t));
        }
        page.child(workbench::installed_actions(s, t, &instance.name, record))
    } else if let Some(detail) = detail {
        browse::details::body(s, detail, t)
    } else {
        match s.screen {
            Screen::Explore => browse::explore(s, t, &k, &mut layer),
            Screen::Local => workbench::local_mods(s, t, &k, &mut layer),
            Screen::Setup => game_setup::view(s, t, &k),
            Screen::Downloads => download_queue::page(s, t, &k),
        }
    };
    let content = col().w_full().gap(16.0).padding(24.0).child(content);
    let mut main = col().grow(1.0).min_w(0.0).min_h(0.0).h_full();
    if let Some(detail) = detail {
        main = main.child(browse::details::header(s, detail, t, &k));
        browse::details::overlays(s, detail, t, &k, &mut layer, cx.is_reduced_motion());
    } else if let Some((_, record)) = installed {
        main = main.child(
            row()
                .w_full()
                .items_center()
                .gap(12.0)
                .padding(16.0)
                .fill(k.c.layer_01)
                .child(
                    button("details-back", "←", t, false, true, |s, tx| {
                        s.set_screen(Screen::Local, tx)
                    })
                    .label("Back to mods"),
                )
                .child(heading(workbench::installed_title(record), t)),
        );
    }
    let main = main.child(
        col()
            .key("main-scroll")
            .w_full()
            .grow(1.0)
            .shrink()
            .min_h(0.0)
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .child(content),
    );
    let shell = SplitView::new(main)
        .sidebar(
            &s.sidebar,
            |s: &mut State| &mut s.sidebar,
            sidebar(s, t, &k, &mut layer),
        )
        .toolbar(Toolbar::new().title("DMM · Mod Manager"))
        .titlebar_inset(cx.titlebar_inset())
        .reduced_motion(cx.is_reduced_motion())
        .build(&Kit::neutral(dark));
    #[cfg(target_os = "linux")]
    game_setup::overlays(s, t, &k, &mut layer);
    workbench::overlays(s, t, &k, &mut layer);
    download_queue::overlay(s, t, &k, &mut layer);
    rune_kit::root(shell, layer).on_layout(|s: &mut State, [_, _, width, height]| {
        s.window_size = [width, height];
    })
}

fn sidebar(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) -> El {
    let header = col()
        .w_full()
        .gap(5.0)
        .child(muted("Game", t))
        .child(game_select(s, t, k, layer));
    let navigation = [
        (Screen::Explore, "Explore"),
        (Screen::Local, "Local mods"),
        (Screen::Setup, "Game setup"),
    ]
    .into_iter()
    .map(|(screen, label)| {
        SidebarRow::new(label)
            .selected(s.screen == screen)
            .on_select(move |s: &mut State, tx| s.set_screen(screen, tx))
            .build(k)
            .key(format!("nav-{label}"))
    });
    Sidebar::new()
        .header(header)
        .child(col().key("navigation").gap(2.0).children(navigation))
        .footer(
            col()
                .gap(4.0)
                .child(download_queue::sidebar(s, t, k))
                .child(button(
                    "launch-game",
                    "Launch game",
                    t,
                    true,
                    s.snapshot.selected.is_some()
                        && s.game_kind() != GameKind::Generic
                        && !s.queue.running()
                        && s.busy.is_none(),
                    |s, _| s.launch_game(),
                ))
                .child(status(s, t)),
        )
        .build(k)
}

fn game_select(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) -> El {
    let name = game_name(s.public_game);
    let enabled = s.busy.is_none();
    let trigger = row()
        .w_full()
        .min_h(36.0)
        .padding(5.0)
        .gap(7.0)
        .items_center()
        .role("button")
        .label(format!("Game: {name}"))
        .focusable(enabled)
        .disabled(!enabled)
        .expanded(s.game_menu)
        .radius(8.0)
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_base)
        .child(game_icon(s, s.game_kind(), t))
        .child(
            text(name)
                .font(type_::LABEL)
                .color(t.text)
                .grow(1.0)
                .shrink()
                .truncate(),
        )
        .child(muted("▾", t));
    let trigger = if enabled {
        trigger
            .hover(t.hover)
            .on_press(|s: &mut State| s.game_menu = !s.game_menu)
    } else {
        trigger
    };
    let mut menu = Menu::new("game-select")
        .trigger(trigger)
        .open(s.game_menu)
        .min_width(288.0)
        .on_close(|s: &mut State| s.game_menu = false);
    if s.game_menu {
        for (kind, title) in SUPPORTED.into_iter().chain(
            s.snapshot
                .instances
                .iter()
                .any(|i| i.kind == GameKind::Generic)
                .then_some((GameKind::Generic, "Custom game")),
        ) {
            let detected = s.detected.iter().any(|game| game.kind == kind)
                || (kind == GameKind::Zzmi && !s.zzz_steam_roots.is_empty());
            let item = MenuItem::new(title)
                .lead(game_icon(s, kind, t))
                .checked(s.public_game == kind)
                .on_select(move |s: &mut State| s.select_game(kind));
            menu = menu.item(if detected {
                item.badge("Detected")
            } else {
                item
            });
        }
    }
    menu.build(k, layer)
}

fn game_name(kind: GameKind) -> &'static str {
    match kind {
        GameKind::Cyberpunk => SUPPORTED[0].1,
        GameKind::Zzmi => SUPPORTED[1].1,
        GameKind::Generic => "Generic game",
    }
}

fn icon(bytes: &[u8]) -> Option<Pixels> {
    let rgba = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let pixels = Pixels::new();
    pixels.set(rgba.width(), rgba.height(), rgba.into_raw());
    Some(pixels)
}

fn game_icon(s: &State, kind: GameKind, t: Theme) -> El {
    let pixels = match kind {
        GameKind::Cyberpunk => s.cyberpunk_icon.as_ref(),
        GameKind::Zzmi => s.zzmi_icon.as_ref(),
        GameKind::Generic => None,
    };
    let square = el()
        .size(24.0, 24.0)
        .clip_rounded(5.0)
        .fill(t.control_track)
        .center();
    if let Some(pixels) = pixels {
        square.pixels_fit(pixels, SurfaceFit::Fit)
    } else {
        square.child(muted("G", t))
    }
}

fn source_icon(s: &State, source: Source, side: f32) -> El {
    let pixels = match source {
        Source::Nexus => s.nexus_icon.as_ref(),
        Source::GameBanana => s.gamebanana_icon.as_ref(),
        Source::Local => None,
    };
    let frame = el().size(side, side).a11y_hidden();
    match pixels {
        Some(pixels) => frame.pixels_fit(pixels, SurfaceFit::Fit),
        None => frame,
    }
}

fn open_library(path: &Path) -> Result<Library> {
    Library::open_wait(path, std::time::Duration::from_secs(5))
}

fn status(s: &State, t: Theme) -> El {
    let label = match s.busy.or(s.details_busy).or(s.catalog_busy) {
        Some("Searching mods…") if !s.browser.show_search_progress => "Ready",
        Some(label) => label,
        None if s.notice.is_empty() => "Ready",
        None => &s.notice,
    };
    row()
        .key("status")
        .role("status")
        .min_h(32.0)
        .w_full()
        .px(6.0)
        .items_center()
        .child(muted(label, t).w_full().truncate())
        .help(label)
}

fn control_border(dark: bool) -> rune::Color {
    if dark {
        oklch(0.54, 0.022, 272.0)
    } else {
        oklch(0.55, 0.022, 272.0)
    }
}

fn control_background(dark: bool) -> rune::Color {
    if dark {
        oklch(0.255, 0.018, 272.0)
    } else {
        oklch(0.985, 0.008, 272.0)
    }
}

fn button(
    key: impl Into<String>,
    label: impl Into<String>,
    t: Theme,
    active: bool,
    enabled: bool,
    action: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    let label = label.into();
    let on = active && enabled;
    let mut item = row()
        .key(key.into())
        .role("button")
        .label(&label)
        .focusable(enabled)
        .disabled(!enabled)
        .min_h(32.0)
        .px(10.0)
        .center()
        .radius(8.0)
        .fill(if on { oklch(0.49, 0.16, 282.0) } else { t.card })
        .border(1.0, t.card_border)
        .child(
            text(label)
                .font(type_::LABEL)
                .color(if on {
                    oklch(0.99, 0.0, 282.0)
                } else if enabled {
                    t.text
                } else {
                    t.text_secondary
                })
                .truncate(),
        );
    if enabled {
        item = item.hover(t.hover).on_press_tx(action);
    }
    item
}

fn field(
    key: &str,
    placeholder: &str,
    edit: &TextEdit,
    t: Theme,
    accessor: impl for<'a> Fn(&'a mut State) -> &'a mut TextEdit + 'static,
) -> El {
    text_input(edit, type_::BODY)
        .key(key)
        .label(placeholder)
        .single_line()
        .grow(1.0)
        .shrink()
        .h(34.0)
        .padding(8.0)
        .radius(8.0)
        .fill(t.control_off)
        .border(
            1.0,
            if edit.focused {
                t.text
            } else {
                t.text_secondary
            },
        )
        .color(t.text)
        .placeholder(placeholder, t.text_secondary)
        .on_edit(accessor)
}

fn heading(label: impl Into<String>, t: Theme) -> El {
    text(label.into())
        .font(TextStyle::new(20.0, 670.0))
        .color(t.text)
        .wrap()
}

fn muted(label: impl Into<String>, t: Theme) -> El {
    text(label.into())
        .font(type_::LABEL)
        .color(t.text_secondary)
}

fn panel(t: Theme) -> El {
    col()
        .w_full()
        .gap(10.0)
        .padding(14.0)
        .radius(11.0)
        .fill(t.card)
        .border(1.0, t.card_border)
}
