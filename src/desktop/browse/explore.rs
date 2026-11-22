use dmm::sources::{SearchPage, SearchResult, Sort, Source};
use rune::tok::{Theme, type_};
use rune::{El, TextEdit, col, el, oklch, row, text};
use rune_kit::Kit;

use super::{State, filters};
use crate::desktop::components::{
    button, field, heading, muted, panel, source_icon, source_name, styled_field,
};
use crate::desktop::mod_card::{Card, byline, remote_key};

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
        view.child(results(s, page, t, k))
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
fn results(s: &State, page: &SearchPage, t: Theme, k: &Kit) -> El {
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
                card(s, item, t, k)
            } else {
                el().w(248.0).h(248.0).radius(12.0).fill(t.card)
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

fn card(s: &State, item: &SearchResult, t: Theme, k: &Kit) -> El {
    let mut meta = Vec::new();
    let sort = s
        .browser
        .shown_query
        .as_ref()
        .map_or(Sort::Newest, |query| query.sort);
    match sort {
        Sort::Popularity => {
            let count = match item.id.source {
                Source::Nexus => item.endorsements.map(|n| format!("{n} endorsements")),
                Source::GameBanana => item.downloads.map(|n| format!("{n} downloads")),
                Source::Local => None,
            };
            if let Some(value) = count {
                meta.push(value);
            }
        }
        Sort::Likes => {
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
    if let Some(downloads) = item.downloads
        && !(sort == Sort::Popularity && item.id.source == Source::GameBanana)
    {
        meta.push(format!("{downloads} downloads"));
    }
    if sort != Sort::Updated
        && let Some(updated) = item.updated_at.as_deref().and_then(short_date)
    {
        meta.push(format!("Updated {updated}"));
    }
    if item.adult == Some(true) {
        meta.push("NSFW".into());
    }
    Card {
        key: format!("result-{}", remote_key(&item.id)),
        name: &item.name,
        image: item.image.as_deref(),
        byline: byline(item.author.as_deref(), item.category.as_deref(), "Mod"),
        metadata: meta.join(" · "),
        remote: Some(&item.id),
        installed: s.installed_for(&item.id),
    }
    .view(s, t, k)
}

fn short_date(value: &str) -> Option<&str> {
    let date = value.get(..10)?;
    (date.as_bytes().get(4) == Some(&b'-') && date.as_bytes().get(7) == Some(&b'-')).then_some(date)
}
pub(super) fn sort_name(sort: Sort, source: Source) -> &'static str {
    match sort {
        Sort::Relevance => "Relevance",
        Sort::Popularity => match source {
            Source::GameBanana => "Most downloaded",
            Source::Nexus => "Most endorsed",
            Source::Local => "Popular",
        },
        Sort::Likes => match source {
            Source::GameBanana => "Most liked",
            Source::Nexus => "Most endorsed",
            Source::Local => "Likes",
        },
        Sort::Newest => "Recent",
        Sort::Updated => "Recently updated",
    }
}
