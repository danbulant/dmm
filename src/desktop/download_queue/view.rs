use std::collections::HashSet;

use rune::tok::{Theme, type_};
use rune::{El, ScrollSpec, SurfaceFit, TextStyle, col, el, row, text};
use rune_kit::shell::SidebarRow;
use rune_kit::{Dialog, Kit, Progress};
use url::Url;

use super::{Action, Entry, Phase, Transfer};
use crate::desktop::components::{
    button, bytes, field, heading, muted, open_page, panel, source_icon, styled_field,
};
use crate::desktop::{Screen, Size, State};

pub(in crate::desktop) fn page(s: &State, t: Theme, k: &Kit) -> El {
    let queue = &s.queue;
    let pending = queue.items.iter().filter(|item| item.pending()).count();
    let installed = queue
        .items
        .iter()
        .filter(|item| matches!(item.phase, Phase::Complete))
        .count();
    let stopped = queue.items.len() - pending - installed;
    let transferred = queue
        .items
        .iter()
        .fold(0u64, |total, item| total.saturating_add(item.received()));
    let has_handoff = handoff_links(s).next().is_some();
    let can_handoff = queue.available();
    let mut view = col().w_full().gap(16.0)
        .child(row().w_full().gap(12.0).items_center()
            .child(heading("Downloads", t).grow(1.0))
            .children(has_handoff.then(|| button(
                "queue-nexus-link",
                "Finish Nexus downloads",
                t,
                false,
                can_handoff,
                |s, _| s.handoff_open = true,
            ))))
        .child(muted(format!("{pending} pending  ·  {installed} installed  ·  {stopped} stopped  ·  {} transferred", bytes(transferred)), t));
    if has_handoff {
        view = view.child(muted(
            "For a waiting install, open each Nexus file page, choose Mod Manager Download, then paste its nxm:// link.",
            t,
        ).wrap());
    }
    if queue.items.is_empty() {
        return view.child(panel(t)
            .child(text(if has_handoff { "Nexus links are ready" } else { "No downloads yet" })
                .font(TextStyle::new(16.0, 650.0)).color(t.text))
            .child(muted(
                if has_handoff {
                    "Finish the pending Nexus download by opening its file page and entering the nxm:// link."
                } else {
                    "Mods you install from Explore or Local mods will appear here."
                },
                t,
            ).wrap()));
    }
    if pending > 0 {
        view = view.child(heading("In progress", t)).children(
            queue
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.pending())
                .map(|(index, item)| entry(s, index, item, t, k)),
        );
    }
    if installed + stopped > 0 {
        view = view.child(heading("Completed and stopped", t)).children(
            queue
                .items
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, item)| !item.pending())
                .map(|(index, item)| entry(s, index, item, t, k)),
        );
    }
    view
}

fn entry(s: &State, index: usize, item: &Entry, t: Theme, k: &Kit) -> El {
    let game = s
        .snapshot
        .instances
        .iter()
        .find(|game| game.name == item.instance)
        .map_or(item.instance.as_str(), |game| game.kind.display_name());
    let mut line = row()
        .w_full()
        .items_center()
        .gap(12.0)
        .child(thumbnail(s, item, t, 64.0, 45.0).key(format!("queue-thumbnail-{index}")))
        .child(
            col()
                .grow(1.0)
                .shrink()
                .min_w(0.0)
                .gap(5.0)
                .child(
                    text(&item.name)
                        .font(TextStyle::new(16.0, 650.0))
                        .color(t.text)
                        .truncate()
                        .help(&item.name),
                )
                .child(muted(format!("{game}  ·  {}", file_summary(item)), t).wrap()),
        );
    line = line.child(
        muted(item.status(), t)
            .px(8.0)
            .py(4.0)
            .radius(6.0)
            .fill(t.control_off),
    );
    if let Phase::Waiting(_) = item.phase {
        line = line.child(button(
            format!("queue-nexus-{index}"),
            "Paste Nexus link",
            t,
            true,
            true,
            |s, _| s.handoff_open = true,
        ));
    }
    if item.pending() {
        line = line.child(button(
            format!("queue-cancel-{index}"),
            "Cancel",
            t,
            false,
            can_cancel(item),
            move |s, _| s.queue.cancel(index),
        ));
    }
    let mut card = panel(t).key(format!("queue-entry-{index}")).child(line);
    if let Some(transfer) = &item.transfer {
        card = card.child(
            text(filename(transfer))
                .font(type_::LABEL)
                .color(t.text)
                .truncate()
                .help(&transfer.file_name),
        );
        if matches!(item.phase, Phase::Running(_)) {
            card = card.child(transfer_metrics(transfer, t)).child(
                Progress::new(fraction(transfer))
                    .label(format!("Download progress for {}", transfer.file_name))
                    .build(k),
            );
        }
    }
    if let Phase::Waiting(urls) = &item.phase {
        card = card.children(urls.iter().enumerate().map(|(file, url)| {
            let url = url.clone();
            button(
                format!("queue-open-nexus-{index}-{file}"),
                format!("Open Nexus file {} ↗", file + 1),
                t,
                false,
                true,
                move |s, tx| open_page(s, tx, url.as_str()),
            )
        }));
    }
    if let Phase::Failed(error) = &item.phase {
        card = card.child(muted(error, t).wrap());
    }
    if matches!(item.phase, Phase::Failed(_) | Phase::Cancelled) && s.matches_instance(&item.id) {
        let id = item.id.clone();
        card = card.child(button(
            format!("queue-reinstall-{index}"),
            "Choose files again",
            t,
            false,
            true,
            move |s, _| s.details_for_install(id.clone()),
        ));
    }
    card
}

pub(in crate::desktop) fn sidebar(s: &State, t: Theme, k: &Kit) -> El {
    let pending = s.queue.items.iter().filter(|item| item.pending()).count();
    let active = s.queue.running.and_then(|index| s.queue.items.get(index));
    let mut nav = SidebarRow::new("Downloads")
        .selected(s.screen == Screen::Downloads)
        .on_select(State::open_downloads);
    if pending > 0 {
        nav = nav.badge(pending.to_string());
    }
    let mut card = col()
        .key("download-sidebar")
        .w_full()
        .gap(6.0)
        .child(nav.build(k).key("nav-Downloads"));
    if let Some(item) = active {
        let mut transfer_card = col()
            .key("active-download")
            .w_full()
            .padding(9.0)
            .gap(5.0)
            .radius(8.0)
            .fill(k.c.layer_01)
            .border(1.0, t.card_border)
            .child(
                row()
                    .gap(7.0)
                    .items_center()
                    .child(thumbnail(s, item, t, 30.0, 24.0).key("active-download-thumbnail"))
                    .child(
                        text(&item.name)
                            .font(type_::LABEL)
                            .color(t.text)
                            .grow(1.0)
                            .shrink()
                            .min_w(0.0)
                            .truncate()
                            .help(&item.name),
                    ),
            );
        let transfer = match (&item.phase, &item.transfer) {
            (Phase::Running(control), Some(transfer))
                if !control.is_applying() && !control.is_cancelled() =>
            {
                Some(transfer)
            }
            _ => None,
        };
        if let Some(file) = transfer {
            transfer_card = transfer_card
                .child(muted(filename(file), t).truncate().help(&file.file_name))
                .child(
                    Progress::new(fraction(file))
                        .label("File download progress")
                        .build(k),
                )
                .child(transfer_metrics(file, t));
        } else {
            transfer_card = transfer_card
                .child(muted(item.status(), t).truncate())
                .child(Progress::new(None).label(item.status()).build(k));
        }
        card = card.child(transfer_card);
    }
    card
}

pub(in crate::desktop) fn overlay(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) {
    if s.handoff_open {
        Dialog::new("nexus-downloads", "Nexus downloads")
            .open(true)
            .fit()
            .width(640.0)
            .on_close(|s: &mut State| s.handoff_open = false)
            .child(handoff(s, t, k).mx(16.0).mb(16.0))
            .build(k, layer);
    }
}

fn can_cancel(item: &Entry) -> bool {
    match &item.phase {
        Phase::Running(control) => !control.is_cancelled() && !control.is_applying(),
        Phase::Waiting(_) | Phase::Queued(Action::Install(_) | Action::Redeem(_)) => true,
        Phase::Queued(Action::DiscardNexus)
        | Phase::Complete
        | Phase::Cancelled
        | Phase::Failed(_) => false,
    }
}

fn file_summary(item: &Entry) -> String {
    if item.file_count == 0 {
        return "Nexus download".into();
    }
    let count = item.file_count;
    let unit = if count == 1 { "file" } else { "files" };
    let size = item
        .estimated_bytes
        .or_else(|| {
            item.transfer
                .as_ref()
                .filter(|file| file.total == 1)
                .and_then(|file| file.total_bytes)
        })
        .map_or_else(|| "size unknown".into(), bytes);
    format!("{count} {unit}  ·  {size} total")
}

fn fraction(transfer: &Transfer) -> Option<f32> {
    transfer
        .total_bytes
        .filter(|total| *total > 0)
        .map(|total| (transfer.received_bytes as f32 / total as f32).min(1.0))
}

fn filename(transfer: &Transfer) -> String {
    if transfer.total > 1 {
        format!(
            "{}/{}  {}",
            transfer.current, transfer.total, transfer.file_name
        )
    } else {
        transfer.file_name.clone()
    }
}

fn transfer_metrics(transfer: &Transfer, t: Theme) -> El {
    let received = match transfer.total_bytes {
        Some(total) => format!("{} / {}", bytes(transfer.received_bytes), bytes(total)),
        None => bytes(transfer.received_bytes),
    };
    row()
        .w_full()
        .items_center()
        .justify_between()
        .gap(6.0)
        .child(muted(received, t).shrink().min_w(0.0).truncate())
        .child(muted(format!("{}/s", bytes(transfer.bytes_per_second)), t).truncate())
}

fn thumbnail(s: &State, item: &Entry, t: Theme, width: f32, height: f32) -> El {
    let frame = el().size(width, height).clip_rounded(6.0).a11y_hidden();
    if let Some(previews) = &s.previews
        && item.image.is_some()
    {
        previews.surface(
            item.image.as_deref(),
            Size::Thumbnail,
            frame,
            SurfaceFit::Fill,
            t,
        )
    } else {
        frame
            .fill(t.control_off)
            .center()
            .child(source_icon(s, item.id.source, 16.0))
    }
}

fn handoff_links(s: &State) -> impl Iterator<Item = &Url> {
    let mut seen = HashSet::new();
    s.queue
        .items
        .iter()
        .flat_map(|entry| match &entry.phase {
            Phase::Waiting(links) => links.as_slice(),
            _ => &[],
        })
        .chain(&s.snapshot.nexus_pages)
        .filter(move |&url| seen.insert(url))
}

fn handoff(s: &State, t: Theme, k: &Kit) -> El {
    let waiting = s
        .queue
        .items
        .iter()
        .any(|item| matches!(item.phase, Phase::Waiting(_)));
    let enabled = s.queue.available() && (s.snapshot.selected.is_some() || waiting);
    let mut links = col().w_full().gap(6.0);
    let mut pages = handoff_links(s).peekable();
    let has_pages = pages.peek().is_some();
    for (index, url) in pages.enumerate() {
        let url = url.clone();
        links = links.child(button(
            format!("open-handoff-{index}"),
            format!("Open Nexus file {} ↗", index + 1),
            t,
            false,
            true,
            move |s, tx| open_page(s, tx, url.as_str()),
        ));
    }
    let view = panel(t)
        .child(muted("Use your free Nexus account in the browser: open each file page, choose Mod Manager Download, then paste its nxm:// link here.", t).wrap())
        .children(has_pages.then(|| {
            col().w_full().max_h(220.0).scroll(ScrollSpec { stick_bottom: false }).child(links)
        }));
    let view = view.children((!s.notice.is_empty()).then(|| muted(&s.notice, t).wrap()));
    view.child(
        row()
            .w_full()
            .gap(8.0)
            .child(
                styled_field(
                    "nxm-uri",
                    &s.browser.nxm,
                    field(&s.browser.nxm, "Paste nxm:// link", |s| &mut s.browser.nxm)
                        .on_submit(|s: &mut State, _| s.redeem_link()),
                    k,
                    t,
                )
                .grow(1.0)
                .shrink(),
            )
            .child(button(
                "receive-nxm",
                "Receive",
                t,
                false,
                enabled && !s.browser.nxm.text.trim().is_empty(),
                |s, _| s.redeem_link(),
            )),
    )
}
