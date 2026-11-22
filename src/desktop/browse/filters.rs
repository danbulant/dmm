use std::collections::{HashMap, HashSet};

use dmm::sources::{Category, FilterCatalog, Maturity, Sort, Source, TimeWindow};
use rune::tok::Theme;
use rune::{El, ScrollSpec, col, row};
use rune_kit::{Checkbox, Kit, Placement, SegmentedControl, Select, SelectAppearance};

use super::explore::{search_field, sort_name};
use super::{Catalog, State, nonempty};
use crate::desktop::components::{button, field, muted, panel, styled_field};

const SORTS: [Sort; 4] = [
    Sort::Relevance,
    Sort::Popularity,
    Sort::Newest,
    Sort::Updated,
];
const CONTENT_MODES: [Maturity; 3] = [Maturity::Safe, Maturity::All, Maturity::Adult];

pub(super) fn toolbar(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) -> El {
    let names = SORTS.map(sort_name);
    let sort = Select::new("sort", &names)
        .label("Sort results")
        .appearance(SelectAppearance::Field)
        .placement(Placement::BottomStart)
        .width(165.0)
        .current(Some(sort_name(s.browser.sort)))
        .open(s.browser.sort_menu)
        .on_toggle(|s: &mut State| s.browser.sort_menu = !s.browser.sort_menu)
        .on_close(|s: &mut State| s.browser.sort_menu = false)
        .on_choose(|s: &mut State, name, _| {
            s.browser.sort_menu = false;
            if let Some(sort) = SORTS.into_iter().find(|&sort| sort_name(sort) == name)
                && s.browser.sort != sort
            {
                s.browser.sort = sort;
                s.search();
            }
        })
        .build(k, layer)
        .border(1.0, k.c.border_base);
    let mut toolbar = row()
        .w_full()
        .gap(8.0)
        .items_center()
        .flex_wrap()
        .child(sort)
        .child(
            button(
                "filters",
                "Filters",
                t,
                s.browser.show_filters,
                true,
                |s, _| s.filters(),
            )
            .expanded(s.browser.show_filters),
        )
        .children(active(s, t));
    if s.browser.source == Source::Nexus && s.snapshot.nexus_pages.is_empty() {
        toolbar = toolbar.child(button(
            "nxm-toggle",
            "Nexus link",
            t,
            s.handoff_open,
            s.busy.is_none(),
            |s, _| s.handoff_open = true,
        ));
    }
    toolbar
}

fn active(s: &State, t: Theme) -> Vec<El> {
    let b = &s.browser;
    let mut labels = Vec::new();
    if b.source == Source::Nexus {
        labels.push(match b.maturity {
            Maturity::Safe => "SFW only".to_owned(),
            Maturity::All => "All content".to_owned(),
            Maturity::Adult => "NSFW only".to_owned(),
        });
    }
    if let Some(window) = b.provider_filters.published_within {
        labels.push(format!("Published: {}", window.label()));
    }
    if let Some(catalog) = b.catalog() {
        if let Some(category) = b
            .category
            .as_deref()
            .and_then(|id| catalog.categories.iter().find(|c| c.id == id))
        {
            let mut path = vec![category.name.clone()];
            let mut parent = category.parent.as_deref();
            while let Some(id) = parent {
                let Some(ancestor) = catalog.categories.iter().find(|c| c.id == id) else {
                    break;
                };
                path.push(ancestor.name.clone());
                parent = ancestor.parent.as_deref();
            }
            path.reverse();
            labels.push(path.join(" › "));
        }
        if let Some(option) = catalog
            .release_types
            .iter()
            .find(|option| Some(&option.id) == b.provider_filters.release_type.as_ref())
        {
            labels.push(format!("Release: {}", option.name));
        }
        for option in &catalog.content_ratings {
            if b.provider_filters.content_ratings.contains(&option.id) {
                labels.push(option.name.clone());
            }
        }
    }
    if b.source == Source::GameBanana
        && b.provider_filters
            .content_ratings
            .iter()
            .any(|rating| rating == "-")
        && !labels.iter().any(|label| label == "Unrated")
    {
        labels.push("Unrated".to_owned());
    }
    for (label, edit) in [
        ("Author", &b.author),
        ("Language", &b.language),
        ("Tag", &b.tag),
    ] {
        if let Some(value) = nonempty(&edit.text) {
            labels.push(format!("{label}: {value}"));
        }
    }
    let mut chips = labels
        .into_iter()
        .map(|label| {
            muted(&label, t)
                .px(8.0)
                .py(4.0)
                .radius(6.0)
                .fill(t.control_off)
                .max_w(245.0)
                .truncate()
                .help(label)
        })
        .collect::<Vec<_>>();
    chips.push(button(
        "clear-filters",
        "Clear filters",
        t,
        false,
        true,
        |s, _| s.clear_filters(),
    ));
    chips
}

pub(super) fn options(s: &State, t: Theme, k: &Kit) -> El {
    let b = &s.browser;
    let mut body = panel(t).gap(16.0);
    if b.source == Source::Nexus {
        let maturity = SegmentedControl::new(
            &["SFW only", "All", "NSFW only"],
            CONTENT_MODES.iter().position(|&mode| mode == b.maturity),
        )
        .label("Content visibility")
        .width(Some(246.0))
        .on_pick(|s: &mut State, index, _| {
            let mode = CONTENT_MODES[index];
            if s.browser.maturity != mode {
                s.browser.maturity = mode;
                s.search();
            }
        })
        .build(k)
        .key("content-visibility")
        .motion_snaps();
        body = body.child(maturity);
    }
    if b.source == Source::Nexus {
        let selected = b.provider_filters.published_within;
        let dates = row()
            .gap(6.0)
            .items_center()
            .flex_wrap()
            .child(button(
                "published-any",
                "Any time",
                t,
                selected.is_none(),
                true,
                |s, _| s.set_publish_window(None),
            ))
            .children(TimeWindow::ALL.into_iter().map(|window| {
                button(
                    format!("published-{}", window.label()),
                    window.label(),
                    t,
                    selected == Some(window),
                    true,
                    move |s, _| s.set_publish_window(Some(window)),
                )
            }));
        body = body.child(
            col()
                .gap(6.0)
                .child(muted("Published", t))
                .child(dates)
                .child(muted("Popular ranks matching mods by lifetime endorsements.", t).wrap()),
        );
    }
    match &b.catalog {
        Catalog::Ready(catalog) => {
            if !catalog.categories.is_empty() {
                body = body.child(categories(s, catalog, t, k));
            }
            if !catalog.release_types.is_empty() {
                let mut choices = row().flex_wrap().gaps(5.0, 5.0).child(button(
                    "release-any",
                    "Any",
                    t,
                    b.provider_filters.release_type.is_none(),
                    true,
                    |s, _| {
                        s.browser.provider_filters.release_type = None;
                        s.search();
                    },
                ));
                for option in &catalog.release_types {
                    let id = option.id.clone();
                    choices = choices.child(button(
                        format!("release-{id}"),
                        &option.name,
                        t,
                        b.provider_filters.release_type.as_ref() == Some(&id),
                        true,
                        move |s, _| {
                            s.browser.provider_filters.release_type = Some(id.clone());
                            s.search();
                        },
                    ));
                }
                body = body.child(
                    col()
                        .gap(6.0)
                        .child(muted("Release type", t))
                        .child(choices),
                );
            }
            if !catalog.content_ratings.is_empty() {
                body = body.child(ratings(s, catalog, t, k));
            }
        }
        Catalog::Idle => body = body.child(muted("Waiting to load provider filters…", t)),
        Catalog::Loading => body = body.child(muted("Loading provider filters…", t)),
        Catalog::Failed => {
            body = body
                .child(muted("Provider filters unavailable. Reopen Filters to retry.", t).wrap())
        }
    }
    if b.source == Source::Nexus {
        body = body.child(
            col()
                .gap(7.0)
                .child(muted("Exact metadata matches", t))
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .child(search_field(
                            "filter-author",
                            "Author",
                            &b.author,
                            k,
                            t,
                            |s| &mut s.browser.author,
                        ))
                        .child(search_field(
                            "filter-language",
                            "Language",
                            &b.language,
                            k,
                            t,
                            |s| &mut s.browser.language,
                        ))
                        .child(search_field("filter-tag", "Tag", &b.tag, k, t, |s| {
                            &mut s.browser.tag
                        })),
                ),
        );
    }
    body
}

fn ratings(s: &State, catalog: &FilterCatalog, t: Theme, k: &Kit) -> El {
    let selected = &s.browser.provider_filters.content_ratings;
    col()
        .gap(6.0)
        .child(muted("Site content tags · match all selected", t))
        .child(row().child(button(
            "rating-all",
            "All",
            t,
            selected.is_empty(),
            true,
            |s, _| {
                s.browser.provider_filters.content_ratings.clear();
                s.search();
            },
        )))
        .child(row().w_full().flex_wrap().gaps(14.0, 6.0).children(
            catalog.content_ratings.iter().map(|option| {
                let id = option.id.clone();
                Checkbox::new(selected.contains(&id))
                    .label(&option.name)
                    .on_toggle({
                        let id = id.clone();
                        move |s: &mut State| {
                            let tags = &mut s.browser.provider_filters.content_ratings;
                            if let Some(index) = tags.iter().position(|tag| tag == &id) {
                                tags.remove(index);
                            } else {
                                if id == "-" {
                                    tags.clear();
                                } else {
                                    tags.retain(|tag| tag != "-");
                                }
                                tags.push(id.clone());
                                tags.sort_unstable();
                            }
                            s.search();
                        }
                    })
                    .build(k)
                    .key(format!("rating-{id}"))
            }),
        ))
}

fn categories(s: &State, catalog: &FilterCatalog, t: Theme, k: &Kit) -> El {
    let b = &s.browser;
    let categories = &catalog.categories;
    let parents: HashSet<_> = categories
        .iter()
        .filter_map(|category| category.parent.as_deref())
        .collect();
    let by_id: HashMap<_, _> = categories
        .iter()
        .map(|category| (category.id.as_str(), category))
        .collect();
    let group = b.category_group.as_deref();
    let query = b.find_category.text.trim().to_lowercase();
    let region = col()
        .gap(8.0)
        .child(muted("Categories", t))
        .child(styled_field(
            "category-find",
            &b.find_category,
            field(
                &b.find_category,
                "Find a category, character or Bangboo…",
                |s| &mut s.browser.find_category,
            )
            .on_change(|s: &mut State, tx| {
                s.sync_previews();
                tx.scroll_to_instant("category-scroll", 0.0, 0.0);
            }),
            k,
            t,
        ));
    let selected_path = b.category.as_deref().and_then(|id| by_id.get(id)).copied();
    let (trail, selected_trail) = if let Some(category) = selected_path {
        let mut trail = vec![category];
        let mut parent = category.parent.as_deref();
        while let Some(id) = parent {
            let Some(ancestor) = by_id.get(id).copied() else {
                break;
            };
            trail.push(ancestor);
            parent = ancestor.parent.as_deref();
        }
        trail.reverse();
        (trail, true)
    } else {
        let mut trail = Vec::new();
        let mut parent = group;
        while let Some(id) = parent {
            let Some(category) = by_id.get(id).copied() else {
                break;
            };
            trail.push(category);
            parent = category.parent.as_deref();
        }
        trail.reverse();
        (trail, false)
    };
    let mut breadcrumbs = row()
        .w_full()
        .items_center()
        .flex_wrap()
        .gaps(5.0, 4.0)
        .child(button(
            "category-any",
            "All categories",
            t,
            trail.is_empty() && b.category.is_none(),
            true,
            |s, _| {
                s.browser.category_group = None;
                s.category(None);
            },
        ));
    for (index, category) in trail.iter().enumerate() {
        breadcrumbs = breadcrumbs.child(muted("›", t)).child(button(
            format!("category-crumb-{}", category.id),
            &category.name,
            t,
            index + 1 == trail.len(),
            true,
            {
                let id = category.id.clone();
                let last_is_selection = selected_trail && index + 1 == trail.len();
                move |s, _| {
                    if last_is_selection {
                        s.category(Some(id.clone()));
                    } else {
                        s.category(None);
                        s.browser.category_group = Some(id.clone());
                        s.sync_previews();
                    }
                }
            },
        ));
    }
    let mut controls = row()
        .w_full()
        .items_center()
        .flex_wrap()
        .gaps(6.0, 6.0)
        .child(breadcrumbs);
    if !parents.is_empty() && query.is_empty() {
        for parent in categories
            .iter()
            .filter(|c| parents.contains(c.id.as_str()) && c.parent.as_deref() == group)
        {
            let id = parent.id.clone();
            controls = controls.child(
                category_tile(s, parent, t, false, "group", move |s, tx| {
                    s.category(None);
                    s.browser.category_group = Some(id.clone());
                    s.sync_previews();
                    tx.scroll_to_instant("category-scroll", 0.0, 0.0);
                })
                .help(format!("Browse {}", parent.name)),
            );
        }
    }
    let mut choices = col().w_full().gap(10.0);
    let mut visible_sections = 0;
    if query.is_empty() {
        let mut grid = category_grid();
        let mut items = 0;
        if let Some(parent) = group.and_then(|id| by_id.get(id)) {
            let id = parent.id.clone();
            grid = grid.child(button(
                "category-entire-group",
                format!("All in {}", parent.name),
                t,
                b.category.as_ref() == Some(&id),
                true,
                move |s, _| s.category(Some(id.clone())),
            ));
            items += 1;
        }
        for category in categories
            .iter()
            .filter(|c| c.parent.as_deref() == group && !parents.contains(c.id.as_str()))
        {
            grid = grid.child(category_option(s, category, t));
            items += 1;
        }
        if items > 0 {
            choices = choices.child(grid);
            visible_sections += 1;
        }
    } else {
        let mut by_parent: HashMap<Option<&str>, Vec<&Category>> = HashMap::new();
        for category in categories
            .iter()
            .filter(|c| c.name.to_lowercase().contains(&query))
        {
            by_parent
                .entry(category.parent.as_deref())
                .or_default()
                .push(category);
        }
        for parent in std::iter::once(None).chain(categories.iter().map(|c| Some(c.id.as_str()))) {
            if let Some(items) = by_parent.remove(&parent) {
                let label = parent
                    .and_then(|id| by_id.get(id))
                    .map_or("General", |c| c.name.as_str());
                choices = choices.child(
                    col().gap(5.0).child(muted(label, t)).child(
                        category_grid().children(
                            items
                                .into_iter()
                                .map(|category| category_option(s, category, t)),
                        ),
                    ),
                );
                visible_sections += 1;
            }
        }
    }
    let region = region.child(controls);
    if visible_sections == 0 {
        return if query.is_empty() {
            region
        } else {
            region.child(muted("No categories match.", t))
        };
    }
    region.child(
        col()
            .key("category-scroll")
            .w_full()
            .max_h(192.0)
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .child(choices),
    )
}

fn category_grid() -> El {
    row().w_full().flex_wrap().gaps(6.0, 6.0)
}

fn category_option(s: &State, category: &Category, t: Theme) -> El {
    let id = category.id.clone();
    category_tile(
        s,
        category,
        t,
        s.browser.category.as_ref() == Some(&id),
        "option",
        move |s, _| {
            s.category(Some(id.clone()));
        },
    )
}

fn category_tile(
    s: &State,
    category: &Category,
    t: Theme,
    selected: bool,
    kind: &str,
    action: impl Fn(&mut State, &mut rune::Tx) + 'static,
) -> El {
    let mut tile = row()
        .key(format!("category-{kind}-{}", category.id))
        .role("button")
        .label(&category.name)
        .focusable(true)
        .focus_selected(selected)
        .min_h(32.0)
        .padding(8.0)
        .gap(7.0)
        .items_center()
        .radius(7.0)
        .fill(if selected {
            t.control_track
        } else {
            t.control_off
        })
        .border(1.0, t.separator)
        .hover(t.hover)
        .on_press_tx(action);
    if let (Some(images), Some(icon)) = (&s.previews, category.icon.as_deref()) {
        tile = tile.child(images.icon(Some(icon), t));
    }
    tile.child(muted(&category.name, t).shrink().truncate())
        .help(&category.name)
}
