use dmm::sources::{RemoteMod, SearchPage, SearchResult, Sort, Source};
use rune::tok::{Theme, type_};
use rune::{El, SurfaceFit, TextEdit, col, el, oklch, row, text};
use rune_kit::Kit;

use super::{State, filters};
use crate::desktop::components::{
    button, card_title, field, heading, mod_card, muted, panel, preview, source_icon, source_name,
    styled_field,
};

pub(in crate::desktop) fn explore(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) -> El {
    let game = s.game_kind().display_name().to_uppercase();
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
    let query = search_field("query", "Search mods…", &s.browser.query, k, t, |s| {
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

pub(super) fn search_field(
    key: &str,
    label: &str,
    edit: &TextEdit,
    k: &Kit,
    t: Theme,
    access: fn(&mut State) -> &mut TextEdit,
) -> El {
    styled_field(
        key,
        edit,
        field(edit, label, access)
            .on_change(State::search_after_edit)
            .on_submit(|s: &mut State, _| s.search()),
        k,
        t,
    )
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
        .child(source_icon(s, source, 16.0))
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
    let enabled = s.details_busy.is_none();
    let saved = s.snapshot.is_saved(&id);
    let queued = s.queued_for_selected(&id);
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
    if queued {
        state.push("Queued");
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
    let mut card = mod_card(t)
        .key(format!("result-{key}"))
        .role("button")
        .label(&item.name)
        .focusable(enabled)
        .disabled(!enabled)
        .hover_group()
        .child(preview(
            s,
            item.image.as_deref(),
            128.0,
            SurfaceFit::Fill,
            t,
        ))
        .child(card_title(&item.name, t))
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
    let saved_name = item.name.clone();
    let saved_image = item.image.clone();
    let actions = row()
        .pin(None, Some(16.0), Some(16.0), None)
        .group_opacity(0.0, 1.0)
        .gap(6.0)
        .child(button(
            format!("result-save-{key}"),
            if saved { "Unsave" } else { "Save" },
            t,
            saved,
            s.busy.is_none(),
            move |s, _| {
                s.toggle_bookmark(id.clone(), saved_name.clone(), saved_image.clone(), saved)
            },
        ))
        .child(button(
            format!("result-install-{key}"),
            if queued { "Queued" } else { "Install" },
            t,
            false,
            enabled && !queued && s.queue.available() && s.matches_instance(&item.id),
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

fn id_key(id: &RemoteMod) -> String {
    format!("{}-{}-{}", id.source, id.game, id.id)
}

pub(super) fn sort_name(sort: Sort) -> &'static str {
    match sort {
        Sort::Relevance => "Relevance",
        Sort::Popularity => "Popular",
        Sort::Newest => "Newest",
        Sort::Updated => "Recently updated",
    }
}
