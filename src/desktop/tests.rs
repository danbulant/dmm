use std::rc::Rc;

use dmm::sources::{ModFile, ModImage, Requirement, SearchResult};
use rune::{Env, Runtime};
#[cfg(target_os = "linux")]
use rune::{Key, KeyEvent, Mods};

use super::*;

#[test]
fn explore_scrolls_to_the_last_result_at_minimum_window_size() {
    let mut state = State::new(PathBuf::from("unused"));
    state.browser.results = Some(Rc::new(SearchPage {
        items: (1..=20)
            .map(|id| SearchResult {
                id: RemoteMod {
                    source: Source::Nexus,
                    game: "cyberpunk2077".into(),
                    id,
                },
                name: format!("Mod {id}"),
                author: None,
                category: None,
                summary: None,
                image: None,
                version: None,
                updated_at: None,
                published_at: None,
                url: None,
                downloads: None,
                endorsements: None,
                likes: None,
                adult: Some(false),
            })
            .collect(),
        page: 1,
        total: Some(20),
        total_pages: Some(1),
        has_more: false,
    }));
    let mut runtime = Runtime::new(
        state,
        view,
        (880.0, 620.0),
        Env {
            system_dark: true,
            scale: 1.0,
        },
    );
    runtime.frame(0.0);
    let last = "result-nexus-cyberpunk2077-20";
    let viewport = runtime
        .presented("main-scroll")
        .expect("scroll viewport is visible")
        .0;
    let bottom = viewport[1] + viewport[3];
    assert!(bottom <= 620.0, "viewport must not grow past the window");
    let before = runtime.presented(last).expect("last result is laid out").0;
    assert!(
        before[1] >= bottom,
        "last result should start below the viewport"
    );
    runtime.wheel(
        viewport[0] + viewport[2] / 2.0,
        viewport[1] + viewport[3] / 2.0,
        -12_000.0,
    );
    for _ in 0..90 {
        runtime.frame(1.0 / 60.0);
    }
    let after = runtime
        .presented(last)
        .expect("last result remains laid out")
        .0;
    assert!(
        after[1] >= viewport[1] && after[1] + after[3] <= bottom,
        "scrolling must reveal the entire last result: viewport {viewport:?}, before {before:?}, after {after:?}, at bottom {:?}",
        runtime.scroll_at_bottom("main-scroll")
    );
    assert_eq!(runtime.scroll_at_bottom("main-scroll"), Some(true));
}

#[test]
fn game_and_sort_menus_overlay_without_moving_the_shell() {
    for (trigger, menu) in [
        ("game-select-trigger", "game-select-menu"),
        ("sort-trigger", "listbox"),
    ] {
        let mut runtime = Runtime::new(
            State::new(PathBuf::from("unused")),
            view,
            (880.0, 620.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        runtime.frame(0.0);
        let before = runtime
            .presented("nav-Explore")
            .expect("sidebar row is visible")
            .0;
        let (x, y) = runtime.find(trigger).expect("dropdown trigger is visible");
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        for _ in 0..30 {
            runtime.frame(1.0 / 60.0);
        }
        assert!(runtime.presented(menu).is_some(), "{trigger} did not open");
        assert_eq!(runtime.presented("nav-Explore").unwrap().0, before);
    }
}

#[test]
fn an_in_flight_search_cannot_replace_newer_filter_results() {
    let mut state = State::new(PathBuf::from("unused"));
    let (jobs, requests) = mpsc::channel();
    let (responses, results) = mpsc::channel();
    state.catalog_worker = Some(Worker { jobs, results });
    state.search();
    drop(requests.try_recv().expect("first request dispatched"));
    let stale = state.browser.search_revision;
    state.search();
    state.search();
    let latest = state.browser.search_revision;
    assert!(
        requests.try_recv().is_err(),
        "don't enqueue obsolete intermediary requests"
    );

    let page = |total| SearchPage {
        items: Vec::new(),
        page: 1,
        total: Some(total),
        total_pages: Some(1),
        has_more: false,
    };
    responses
        .send(Ok(Update::Search(
            stale,
            SearchQuery::default(),
            Ok(page(999)),
        )))
        .unwrap();
    state.poll(0.0, &mut Tx::default());
    assert!(
        state.browser.results.is_none(),
        "stale result must never appear"
    );
    drop(
        requests
            .try_recv()
            .expect("latest request dispatched after the old one"),
    );
    assert!(
        requests.try_recv().is_err(),
        "only the latest request should have been dispatched"
    );
    responses
        .send(Ok(Update::Search(
            latest,
            SearchQuery::default(),
            Ok(page(12)),
        )))
        .unwrap();
    state.poll(0.0, &mut Tx::default());
    assert_eq!(state.browser.results.unwrap().total, Some(12));
}

#[test]
fn paging_keeps_the_current_grid_and_restores_cached_pages_immediately() {
    let mut state = State::new(PathBuf::from("unused"));
    let (jobs, requests) = mpsc::channel();
    let (responses, results) = mpsc::channel();
    state.catalog_worker = Some(Worker { jobs, results });
    let origin = state.origin().unwrap().clone();
    let first = SearchQuery {
        page: 1,
        ..SearchQuery::default()
    };
    let result = |page| SearchPage {
        items: Vec::new(),
        page,
        total: Some(35),
        total_pages: Some(2),
        has_more: page == 1,
    };
    state
        .browser
        .remember_page(origin, first.clone(), result(1));
    state.page(true, &mut Tx::default());
    drop(requests.try_recv().expect("second page has no cache"));
    assert_eq!(state.browser.results.as_ref().unwrap().page, 1);
    let revision = state.browser.search_revision;
    responses
        .send(Ok(Update::Search(
            revision,
            SearchQuery { page: 2, ..first },
            Ok(result(2)),
        )))
        .unwrap();
    state.poll(0.0, &mut Tx::default());
    assert_eq!(state.browser.results.as_ref().unwrap().page, 2);
    state.page(false, &mut Tx::default());
    assert_eq!(state.browser.results.as_ref().unwrap().page, 1);
    assert!(
        requests.try_recv().is_err(),
        "back navigation must not enqueue a cached result"
    );
}

fn details_fixture() -> State {
    let mut state = State::new(PathBuf::from("unused"));
    state.snapshot.selected = Some(Instance {
        name: "cyberpunk".into(),
        kind: GameKind::Cyberpunk,
        root: PathBuf::from("unused-game"),
        profile: None,
    });
    state.snapshot.sources = public_origins(GameKind::Cyberpunk);
    let file = |id, name: &str| ModFile {
        id,
        name: name.to_owned(),
        version: Some("2.0".into()),
        updated_at: Some("2026-11-20T10:00:00Z".into()),
        size: Some(335_544_320),
        downloads: Some(2405),
        primary: id == 1,
        category: None,
        description: None,
        url: None,
    };
    state.browser.show_details(ModDetails {
        id: RemoteMod {
            source: Source::Nexus,
            game: "cyberpunk2077".into(),
            id: 42,
        },
        name: "Night City wardrobe".into(),
        author: Some("Judy".into()),
        category: Some("Clothing".into()),
        tags: vec!["Outfits".into()],
        summary: None,
        description: Some(
            "About this wardrobe:\nAn example with two downloadable files.\n".repeat(20),
        ),
        version: Some("2.0".into()),
        updated_at: None,
        created_at: None,
        url: Some("https://www.nexusmods.com/cyberpunk2077/mods/42".into()),
        adult: Some(false),
        images: (1..=2)
            .map(|i| ModImage {
                url: format!("https://staticdelivery.nexusmods.com/{i}.jpg"),
                thumbnail: Some(format!(
                    "https://staticdelivery.nexusmods.com/{i}-small.jpg"
                )),
            })
            .collect(),
        requirements: vec![Requirement {
            name: "ArchiveXL".into(),
            notes: Some("Required for additional clothing.".into()),
            url: Some("https://www.nexusmods.com/cyberpunk2077/mods/4198".into()),
            thumbnail: None,
        }],
        files: vec![file(1, "wardrobe.zip"), file(2, "optional-patch.zip")],
    });
    state
}

#[test]
fn details_keeps_install_header_and_opens_file_and_image_overlays() {
    let mut state = details_fixture();
    let dir = tempfile::tempdir().unwrap();
    let mut dispatched = state.queue.fixture(dir.path()).unwrap();
    let mut runtime = Runtime::new(
        state,
        view,
        (960.0, 620.0),
        Env {
            scale: 1.0,
            system_dark: true,
        },
    );
    runtime.frame(0.0);
    let click = |runtime: &mut Runtime<State>, key| {
        let (x, y) = runtime
            .find(key)
            .unwrap_or_else(|| panic!("{key} not found"));
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(1.0 / 60.0);
    };
    click(&mut runtime, "gallery-thumb-1");
    click(&mut runtime, "gallery-main");
    let full = runtime.presented("full-image").expect("expanded gallery").0;
    assert_eq!(full, [0.0, 0.0, 960.0, 620.0]);
    click(&mut runtime, "close-full-image");
    let header = runtime.presented("detail-header").unwrap().0;
    runtime.update(|_, tx| tx.scroll_to_instant("main-scroll", 0.0, 400.0));
    runtime.frame(1.0 / 60.0);
    click(&mut runtime, "requirement-0");
    assert_eq!(
        runtime.take_url_requests(),
        ["https://www.nexusmods.com/cyberpunk2077/mods/4198"]
    );
    let (x, y) = runtime.find("main-scroll").unwrap();
    runtime.wheel(x, y, -1200.0);
    runtime.frame(1.0 / 60.0);
    assert_eq!(runtime.presented("detail-header").unwrap().0, header);
    click(&mut runtime, "details-install");
    assert!(runtime.presented("mod-install-dialog").is_some());
    click(&mut runtime, "file-2");
    runtime.update(|state, _| {
        let selected: Vec<_> = state
            .browser
            .detail
            .as_ref()
            .unwrap()
            .picked_files()
            .map(|file| file.id)
            .collect();
        assert_eq!(
            selected,
            [1, 2],
            "the optional file must not deselect the main file"
        );
    });
    assert!(runtime.presented("remote-install").is_some());
    click(&mut runtime, "remote-install");
    for _ in 0..24 {
        runtime.frame(1.0 / 60.0);
    }
    assert!(runtime.presented("mod-install-dialog").is_none());
    assert!(
        dispatched(),
        "install must be submitted to the independent download worker"
    );
    assert!(
        runtime.state.busy.is_none(),
        "install must not take the library worker"
    );
    assert!(
        runtime.presented("download-sidebar").unwrap().0[1]
            < runtime.presented("launch-game").unwrap().0[1]
    );
    click(&mut runtime, "nav-Downloads");
    assert!(runtime.presented("queue-entry-0").is_some());
    click(&mut runtime, "nav-Explore");
    assert!(runtime.presented("explore-heading").is_some());
}

#[test]
fn expanding_a_cached_thumbnail_shows_a_zoomable_image_before_the_full_image_arrives() {
    let mut state = details_fixture();
    state.previews = Some(Previews::thumbnail_fixture(
        "https://staticdelivery.nexusmods.com/1-small.jpg",
    ));
    let mut runtime = Runtime::new(state, view, (960.0, 620.0), Env::new(true, 1.0));
    runtime.poll = Some(State::poll);
    runtime.frame(0.0);
    let (x, y) = runtime.find("gallery-main").unwrap();
    runtime.pointer_down(x, y);
    runtime.pointer_up(x, y);
    runtime.frame(1.0 / 60.0);
    let fitted = runtime
        .presented("expanded-gallery-image")
        .expect("cached screenshot")
        .0;
    assert!(
        fitted[2] > 400.0,
        "thumbnail fills the available viewer instead of drawing at its original size"
    );
    let (x, y) = runtime.find("image-preview-zoom-in").unwrap();
    runtime.pointer_down(x, y);
    runtime.pointer_up(x, y);
    for _ in 0..30 {
        runtime.frame(1.0 / 60.0);
    }
    let zoomed = runtime
        .presented("expanded-gallery-image")
        .expect("zoomed screenshot")
        .0;
    assert!(
        zoomed[2] > fitted[2] * 1.3,
        "zoom controls enlarge the displayed screenshot"
    );
}

#[test]
fn catalog_loading_does_not_block_setup_or_local_import_controls() {
    let mut state = details_fixture();
    state.browser.detail = None;
    state.snapshot.instances = state.snapshot.selected.clone().into_iter().collect();
    state.catalog_busy = Some("Searching mods…");
    let mut runtime = Runtime::new(state, view, (960.0, 620.0), Env::new(true, 1.0));
    runtime.frame(0.0);
    let press = |runtime: &mut Runtime<State>, key| {
        let (x, y) = runtime
            .find(key)
            .unwrap_or_else(|| panic!("{key} not found"));
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(1.0 / 60.0);
    };
    press(&mut runtime, "nav-Game setup");
    assert!(matches!(runtime.state.screen, Screen::Setup));
    assert!(runtime.find("setup:root").is_none());
    press(&mut runtime, "setup:add");
    assert!(runtime.find("setup:root").is_some());
    press(&mut runtime, "nav-Local mods");
    assert!(matches!(runtime.state.screen, Screen::Local));
    assert!(runtime.find("local:path").is_none());
    press(&mut runtime, "local:add");
    assert!(runtime.find("local:path").is_some());
}

#[cfg(target_os = "linux")]
#[test]
fn xxmi_setup_blocks_navigation_and_escape_until_the_worker_finishes() {
    let mut state = State::new(PathBuf::from("unused"));
    state.screen = Screen::Setup;
    state.public_game = GameKind::Zzmi;
    state.busy = Some("Setting up XXMI…");
    let (jobs, _requests) = mpsc::channel();
    let (responses, results) = mpsc::channel();
    state.worker = Some(Worker { jobs, results });
    let (stages, progress) = mpsc::channel();
    state.setup.steam_dialog = Some(SteamDialog::Running {
        stage: InstallStage::Preparing,
        progress,
    });
    let mut runtime = Runtime::new(state, view, (960.0, 620.0), Env::new(true, 1.0));
    runtime.poll = Some(State::poll);
    runtime.frame(0.0);
    stages.send(InstallStage::InstallingRuntime).unwrap();
    runtime.frame(0.1);
    assert!(runtime.presented("xxmi-setup-progress").is_some());
    let (x, y) = runtime.find("nav-Local mods").unwrap();
    runtime.pointer_down(x, y);
    runtime.pointer_up(x, y);
    runtime.key(KeyEvent {
        key: Key::Escape,
        mods: Mods::default(),
    });
    runtime.frame(0.1);
    assert!(matches!(runtime.state.screen, Screen::Setup));
    assert!(runtime.presented("xxmi-setup-progress").is_some());

    responses
        .send(Ok(Update::SteamXxmiInstalled(Err(anyhow::anyhow!(
            "Test installer failure"
        )))))
        .unwrap();
    runtime.frame(0.1);
    let (x, y) = runtime
        .find("xxmi-setup-done")
        .expect("failed setup is dismissible");
    runtime.pointer_down(x, y);
    runtime.pointer_up(x, y);
    runtime.frame(0.1);
    assert!(runtime.state.setup.steam_dialog.is_none());
}

#[test]
fn local_grid_presents_bookmarks_and_requires_confirmation_before_removing_an_install() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let state_dir = dir.path().join("state");
    let game = dir.path().join("game");
    let input = dir.path().join("input");
    std::fs::create_dir(&game)?;
    std::fs::create_dir(&input)?;
    std::fs::write(input.join("mod.archive"), b"archive fixture")?;
    let mut library = Library::open(&state_dir)?;
    library.register("cp", GameKind::Cyberpunk, &game, None)?;
    library.install("cp", "night-city", &input, None, Default::default())?;
    let bookmark = RemoteMod {
        source: Source::Nexus,
        game: "cyberpunk2077".into(),
        id: 11,
    };
    library.save_favorite(&bookmark, "Saved wardrobe", None)?;
    let mut state = State::new(state_dir.clone());
    state.screen = Screen::Local;
    state.snapshot = Snapshot::load(&library, Some("cp"))?;
    drop(library);
    let (jobs, requests) = mpsc::channel();
    let (responses, results) = mpsc::channel();
    state.worker = Some(Worker { jobs, results });

    let mut runtime = Runtime::new(state, view, (960.0, 620.0), Env::new(true, 1.0));
    runtime.poll = Some(State::poll);
    runtime.frame(0.0);
    let click = |runtime: &mut Runtime<State>, key| {
        let (x, y) = runtime
            .find(key)
            .unwrap_or_else(|| panic!("{key} not found"));
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(1.0 / 60.0);
    };
    assert!(
        runtime
            .presented("saved-card-nexus-cyberpunk2077-11")
            .is_some()
    );
    let mut backend = Backend {
        state_dir,
        online: None,
    };
    let live = game.join("archive/pc/mod/mod.archive");
    for enable in [false, true] {
        click(&mut runtime, "mod:cp:night-city:enabled");
        responses.send(requests.try_recv()?(&mut backend))?;
        runtime.frame(1.0 / 60.0);
        assert_eq!(runtime.state.snapshot.mods[0].enabled, enable);
        assert_eq!(live.exists(), enable);
    }
    click(&mut runtime, "mod:cp:night-city:details");
    click(&mut runtime, "installed:cp:night-city:remove");
    assert!(runtime.presented("remove-installed-dialog").is_some());
    assert!(
        requests.try_recv().is_err(),
        "opening confirmation must not remove anything"
    );
    click(&mut runtime, "remove-cancel");
    for _ in 0..24 {
        runtime.frame(1.0 / 60.0);
    }
    assert!(runtime.presented("remove-installed-dialog").is_none());
    assert!(game.join("archive/pc/mod/mod.archive").is_file());
    click(&mut runtime, "installed:cp:night-city:remove");
    click(&mut runtime, "remove-confirm");

    responses.send(requests.try_recv()?(&mut backend))?;
    runtime.frame(1.0 / 60.0);
    assert!(runtime.state.local_detail.is_none());
    assert!(runtime.state.snapshot.mods.is_empty());
    assert!(!game.join("archive/pc/mod/mod.archive").exists());
    assert!(
        runtime
            .presented("saved-card-nexus-cyberpunk2077-11")
            .is_some()
    );
    Ok(())
}
