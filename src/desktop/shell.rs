use dmm::games::GameKind;
use dmm::games::discovery::SUPPORTED;
use rune::tok::{self, Theme, type_};
use rune::{Cx, El, ScrollSpec, SurfaceFit, col, el, row, text};
use rune_kit::shell::{Sidebar, SidebarRow, SplitView, Toolbar};
use rune_kit::{Kit, Menu, MenuItem};

use super::components::{self, button, heading, muted};
use super::{Screen, State, browse, download_queue, game_setup, workbench};

pub(super) fn view(s: &State, cx: &mut Cx<State>) -> El {
    let dark = cx.system_dark();
    let t = tok::theme(dark);
    let k = components::kit(dark);
    let mut layer = Vec::new();
    let detail = if s.screen == Screen::Setup {
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
    browse::details::install_overlay(s, t, &k, &mut layer);
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
    let working_on_game = s.snapshot.selected.as_ref().and_then(|game| {
        s.queue
            .active()
            .filter(|(instance, _)| *instance == game.name)
    });
    let deploying = working_on_game.is_some_and(|(_, applying)| applying);
    let launch = button(
        "launch-game",
        "Launch game",
        t,
        true,
        s.snapshot.selected.is_some()
            && s.game_kind() != GameKind::Generic
            && !deploying
            && s.busy.is_none(),
        |s, _| s.launch_game(),
    );
    let launch = match working_on_game {
        Some((_, true)) => launch.help("Wait until DMM finishes writing mod files."),
        Some((_, false)) => launch.help(
            "A download is in progress. It may finish and install files while the game is running.",
        ),
        None => launch,
    };
    Sidebar::new()
        .header(header)
        .child(col().key("navigation").gap(2.0).children(navigation))
        .footer(
            col()
                .gap(4.0)
                .child(download_queue::sidebar(s, t, k))
                .child(launch)
                .child(status(s, t)),
        )
        .build(k)
}

fn game_select(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) -> El {
    let name = s.public_game.display_name();
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
            let detected = s.detected.iter().chain(&s.steam_games).any(|game| game.kind == kind);
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
fn game_icon(s: &State, kind: GameKind, t: Theme) -> El {
    let pixels = match kind {
        GameKind::Cyberpunk => s.cyberpunk_icon.as_ref(),
        GameKind::Zzmi => s.zzmi_icon.as_ref(),
        GameKind::Efmi | GameKind::Generic => None,
    };
    let square = el()
        .size(24.0, 24.0)
        .clip_rounded(5.0)
        .fill(t.control_track)
        .center();
    if let Some(pixels) = pixels {
        square.pixels_fit(pixels, SurfaceFit::Fit)
    } else {
        square.child(muted(if kind == GameKind::Efmi { "E" } else { "G" }, t))
    }
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
