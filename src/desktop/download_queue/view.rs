use rune::tok::{Theme, type_};
use rune::{El, ScrollSpec, col, row, text};
use rune_kit::{Dialog, Kit, Progress};

use super::{Action, Entry, Phase, Transfer};
use crate::desktop::components::{button, field, heading, muted, open_page, panel, styled_field};
use crate::desktop::{Screen, State};

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
    let nexus_files = s.snapshot.nexus_pages.len();
    let nexus_label = if nexus_files == 0 {
        "Enter Nexus link".to_owned()
    } else {
        format!("Nexus links needed ({nexus_files})")
    };
    let mut view = col().w_full().gap(14.0)
        .child(row().w_full().gap(12.0).items_center()
            .child(heading("Downloads", t).grow(1.0))
            .child(button("queue-nexus-link", nexus_label, t, false, s.queue.available() && s.snapshot.selected.is_some(), |s, _| s.handoff_open = true)))
        .child(muted(format!("{pending} pending  ·  {installed} installed  ·  {stopped} stopped  ·  {} transferred", bytes(transferred)), t));
    if queue.items.is_empty() {
        return view.child(panel(t).child(muted(
            if nexus_files > 0 {
                "Finish the pending Nexus downloads by opening the file pages and entering their links."
            } else {
                "Mods you install from Explore or Local mods will appear here."
            },
            t,
        )));
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
        view = view.child(heading("This session", t)).children(
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
    let mut line = row().w_full().items_center().gap(12.0).child(
        col()
            .grow(1.0)
            .shrink()
            .min_w(0.0)
            .gap(4.0)
            .child(
                text(&item.name)
                    .font(type_::TITLE)
                    .color(t.text)
                    .truncate()
                    .help(&item.name),
            )
            .child(muted(format!("{}  ·  {}", item.instance, description(item)), t).wrap()),
    );
    if let Phase::Waiting(_) = item.phase {
        line = line.child(button(
            format!("queue-nexus-{index}"),
            "Nexus link",
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
        card = card.child(muted(&transfer.file_name, t).wrap());
        if matches!(item.phase, Phase::Running(_)) {
            card = card.child(muted(metrics(transfer), t).wrap()).child(
                Progress::new(fraction(transfer))
                    .label("File download progress")
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
    let waiting = s
        .queue
        .items
        .iter()
        .filter(|item| matches!(item.phase, Phase::Waiting(_)))
        .count();
    let label = match (active, pending, waiting) {
        (Some(_), _, _) => format!("Downloads · {pending} pending"),
        (None, _, 1..) => format!("Downloads · {waiting} waiting"),
        (None, 1.., _) => format!("Downloads · {pending} pending"),
        _ if !s.snapshot.nexus_pages.is_empty() => "Downloads · Nexus links needed".into(),
        _ => "Downloads".into(),
    };
    let mut card = col()
        .key("download-sidebar")
        .w_full()
        .gap(5.0)
        .child(button(
            "nav-Downloads",
            label,
            t,
            s.screen == Screen::Downloads,
            true,
            State::open_downloads,
        ));
    if let Some(item) = active {
        card = card.child(
            text(&item.name)
                .font(type_::LABEL)
                .color(t.text)
                .truncate()
                .help(&item.name),
        );
        match (&item.phase, &item.transfer) {
            (Phase::Running(control), _) if control.is_applying() || control.is_cancelled() => {
                card = card.child(muted(description(item), t).truncate());
            }
            (_, Some(transfer)) => {
                card = card
                    .child(
                        muted(&transfer.file_name, t)
                            .truncate()
                            .help(&transfer.file_name),
                    )
                    .child(
                        muted(
                            format!("File {} of {}", transfer.current, transfer.total),
                            t,
                        )
                        .truncate(),
                    )
                    .child(muted(metrics(transfer), t).truncate())
                    .child(Progress::new(fraction(transfer)).build(k));
            }
            _ => {
                card = card
                    .child(muted(description(item), t).truncate())
                    .child(Progress::new(None).build(k));
            }
        }
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

fn description(item: &Entry) -> String {
    let status = match &item.phase {
        Phase::Queued(Action::DiscardNexus) => "Cancelling…".into(),
        Phase::Queued(_) => "Queued".into(),
        Phase::Running(control) if control.is_applying() => "Installing…".into(),
        Phase::Running(control) if control.is_cancelled() => "Cancelling…".into(),
        Phase::Running(_) => item.transfer.as_ref().map_or_else(
            || "Preparing download…".into(),
            |transfer| {
                format!(
                    "Downloading file {} of {}",
                    transfer.current, transfer.total
                )
            },
        ),
        Phase::Waiting(_) => "Waiting for Nexus".into(),
        Phase::Complete => "Installed".into(),
        Phase::Cancelled => "Cancelled".into(),
        Phase::Failed(_) => "Failed".into(),
    };
    if item.file_count == 0 {
        return status;
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
    format!("{status}  ·  {count} {unit}  ·  {size} total")
}

fn fraction(transfer: &Transfer) -> Option<f32> {
    transfer
        .total_bytes
        .filter(|total| *total > 0)
        .map(|total| (transfer.received_bytes as f32 / total as f32).min(1.0))
}

fn metrics(transfer: &Transfer) -> String {
    let received = bytes(transfer.received_bytes);
    let speed = bytes(transfer.bytes_per_second);
    match transfer.total_bytes {
        Some(total) => format!("{received} / {}  ·  {speed}/s", bytes(total)),
        None => format!("{received}  ·  {speed}/s"),
    }
}

fn bytes(value: u64) -> String {
    if value < 1024 {
        return format!("{value} B");
    }
    let mut amount = value as f64;
    for unit in ["KiB", "MiB", "GiB", "TiB", "PiB"] {
        amount /= 1024.0;
        if amount < 1024.0 || unit == "PiB" {
            return format!("{amount:.1} {unit}");
        }
    }
    unreachable!()
}

fn handoff(s: &State, t: Theme, k: &Kit) -> El {
    let enabled = s.snapshot.selected.is_some();
    let mut links = col().w_full().gap(6.0);
    for (index, url) in s.snapshot.nexus_pages.iter().enumerate() {
        let url = url.clone();
        links = links.child(button(
            format!("open-handoff-{index}"),
            format!("Open Nexus file {} ↗", index + 1),
            t,
            false,
            enabled,
            move |s, tx| open_page(s, tx, url.as_str()),
        ));
    }
    let view = panel(t)
        .child(muted("On Nexus, click Mod Manager Download for each file, then paste each nxm:// link below.", t).wrap())
        .children((!s.snapshot.nexus_pages.is_empty()).then(|| {
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
