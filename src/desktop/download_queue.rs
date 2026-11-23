mod view;
#[cfg(windows)]
mod windows_notification;

#[cfg(target_os = "linux")]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use dmm::acquisition::{
    InstallControl, InstallProgress, RemoteOutcome, cancel_pending, nexus_identity, redeem_remote,
    run_remote, source_client,
};
use dmm::library::{InstallOptions, RemoteModState};
use dmm::sources::{ModDetails, ModFile, RemoteMod, SourceClient};
use rune::{Tx, Waker};
use url::Url;
#[cfg(target_os = "linux")]
use zbus::{blocking::Proxy, zvariant::Value};

use super::download_recovery::{self, Bookmark, Recovery};
use super::{Screen, Snapshot, State, Update};
pub(super) use view::{overlay, page, sidebar};
#[cfg(windows)]
use windows_notification::notify_completed_downloads;

struct Request {
    details: ModDetails,
    files: Vec<ModFile>,
    destination: Option<PathBuf>,
    keep_disabled: bool,
}

enum Action {
    Install(Box<Request>),
    Redeem(String),
    DiscardNexus,
}

struct Transfer {
    file_name: String,
    current: usize,
    total: usize,
    received_bytes: u64,
    total_bytes: Option<u64>,
    bytes_per_second: u64,
}

enum Phase {
    Queued(Action),
    Running(InstallControl),
    Waiting(Vec<Url>),
    Complete,
    Cancelled,
    Failed(String),
}

struct Entry {
    id: RemoteMod,
    instance: String,
    name: String,
    image: Option<String>,
    file_count: usize,
    estimated_bytes: Option<u64>,
    received_before_file: u64,
    transfer: Option<Transfer>,
    recovery_path: Option<PathBuf>,
    phase: Phase,
}

pub(super) struct PendingMod<'a> {
    pub id: &'a RemoteMod,
    pub name: &'a str,
    pub image: Option<&'a str>,
    pub status: &'static str,
}

impl Entry {
    fn pending(&self) -> bool {
        matches!(
            self.phase,
            Phase::Queued(_) | Phase::Running(_) | Phase::Waiting(_)
        )
    }

    fn installed_state<'a>(&self, snapshot: &'a Snapshot) -> Option<&'a RemoteModState> {
        if !matches!(self.phase, Phase::Complete) {
            return None;
        }
        snapshot.remote_mods.iter().find(|record| {
            record.instance == self.instance && record.id == self.id
        })
    }

    fn status(&self) -> &'static str {
        match &self.phase {
            Phase::Queued(Action::DiscardNexus) => "Cancelling…",
            Phase::Queued(_) => "Queued",
            Phase::Running(control) if control.is_cancelled() => "Cancelling…",
            Phase::Running(control) if control.is_applying() => "Installing…",
            Phase::Running(_) if self.transfer.is_some() => "Downloading…",
            Phase::Running(_) => "Preparing download…",
            Phase::Waiting(_) => "Needs Nexus download",
            Phase::Complete => "Installed",
            Phase::Cancelled => "Cancelled",
            Phase::Failed(_) => "Failed",
        }
    }

    fn received(&self) -> u64 {
        self.received_before_file
            .saturating_add(self.transfer.as_ref().map_or(0, |file| file.received_bytes))
    }
}

struct Work {
    index: usize,
    id: RemoteMod,
    instance: String,
    recovery_path: Option<PathBuf>,
    action: Action,
    control: InstallControl,
}

enum Completion {
    Installed,
    Browser(Vec<Url>),
    Cancelled,
}

enum Event {
    Progress(usize, InstallProgress),
    Finished(usize, Result<Completion>),
}

struct Worker {
    jobs: Sender<Work>,
    events: Receiver<Event>,
}

#[derive(Default)]
pub(super) struct Queue {
    worker: Option<Worker>,
    recovery: Option<Arc<Recovery>>,
    items: Vec<Entry>,
    running: Option<usize>,
    save: Vec<PathBuf>,
    refresh: bool,
    #[cfg(any(target_os = "linux", windows))]
    completed_in_batch: usize,
}

impl Drop for Queue {
    fn drop(&mut self) {
        if let Some(index) = self.running
            && let Phase::Running(control) = &self.items[index].phase
        {
            control.cancel();
        }
    }
}

impl Queue {
    pub(super) fn available(&self) -> bool {
        self.worker.is_some()
    }

    pub(super) fn active(&self) -> Option<(&str, bool)> {
        let item = self.items.get(self.running?)?;
        let Phase::Running(control) = &item.phase else {
            return None;
        };
        Some((&item.instance, control.is_applying()))
    }

    pub(super) fn preview_urls(&self, full_page: bool) -> impl Iterator<Item = &str> {
        let active = self.running.and_then(|index| self.items.get(index));
        active
            .into_iter()
            .chain(
                self.items
                    .iter()
                    .filter(move |item| full_page && item.pending()),
            )
            .chain(
                self.items
                    .iter()
                    .rev()
                    .filter(move |item| full_page && !item.pending()),
            )
            .filter_map(|item| item.image.as_deref())
    }

    pub(super) fn contains(&self, instance: &str, id: &RemoteMod) -> bool {
        self.items
            .iter()
            .any(|item| item.pending() && item.instance == instance && item.id == *id)
    }

    pub(super) fn pending_mods<'a>(
        &'a self,
        instance: &str,
    ) -> impl Iterator<Item = PendingMod<'a>> {
        self.items
            .iter()
            .filter(move |item| item.instance == instance && item.pending())
            .map(|item| PendingMod {
                id: &item.id,
                name: &item.name,
                image: item.image.as_deref(),
                status: item.status(),
            })
    }

    pub(super) fn start(&mut self, state: &Path, wake: Waker) -> Result<Vec<PathBuf>> {
        let recovery = Arc::new(Recovery::open(state)?);
        recovery.clean_downloads(state)?;
        let interrupted = recovery.interrupted()?;
        let (jobs, requests) = mpsc::channel::<Work>();
        let (finished, events) = mpsc::channel();
        let state = state.to_owned();
        let recovery_lease = Arc::clone(&recovery);
        thread::Builder::new()
            .name("dmm-downloads".into())
            .spawn(move || {
                let _recovery_lease = recovery_lease;
                let mut client = None;
                while let Ok(work) = requests.recv() {
                    let index = work.index;
                    let result = run(&state, &mut client, work, |progress| {
                        if finished.send(Event::Progress(index, progress)).is_ok() {
                            wake();
                        }
                    });
                    if finished.send(Event::Finished(index, result)).is_err() {
                        return;
                    }
                    wake();
                }
            })?;
        self.worker = Some(Worker { jobs, events });
        self.recovery = Some(recovery);
        Ok(interrupted)
    }

    fn enqueue(&mut self, instance: String, request: Request) -> Result<()> {
        let recovery = self
            .recovery
            .as_ref()
            .context("Download queue is unavailable")?;
        anyhow::ensure!(
            !self.contains(&instance, &request.details.id),
            "This mod is already in the download queue"
        );
        let details = &request.details;
        let image = details
            .images
            .first()
            .map(|image| image.thumbnail.as_ref().unwrap_or(&image.url).clone());
        let recovery_path = recovery.record(&Bookmark {
            item: details.id.clone(),
            name: details.name.clone(),
            image: image.clone(),
        })?;
        self.items.push(Entry {
            id: details.id.clone(),
            instance,
            name: details.name.clone(),
            image,
            file_count: request.files.len(),
            estimated_bytes: request
                .files
                .iter()
                .try_fold(0u64, |sum, file| sum.checked_add(file.size?)),
            received_before_file: 0,
            transfer: None,
            recovery_path: Some(recovery_path),
            phase: Phase::Queued(Action::Install(Box::new(request))),
        });
        self.advance();
        Ok(())
    }

    fn redeem(
        &mut self,
        instance: String,
        id: RemoteMod,
        name: String,
        image: Option<String>,
        uri: String,
    ) -> Result<()> {
        anyhow::ensure!(self.worker.is_some(), "Download queue is unavailable");
        let find = self
            .items
            .iter_mut()
            .find(|item| item.instance == instance && item.id == id && item.pending());
        if let Some(item) = find {
            anyhow::ensure!(
                matches!(item.phase, Phase::Waiting(_)),
                "This mod already has a download queued or running"
            );
            item.phase = Phase::Queued(Action::Redeem(uri));
        } else {
            self.items.push(Entry {
                id,
                instance,
                name,
                image,
                file_count: 0,
                estimated_bytes: None,
                transfer: None,
                received_before_file: 0,
                recovery_path: None,
                phase: Phase::Queued(Action::Redeem(uri)),
            });
        }
        self.advance();
        Ok(())
    }

    fn advance(&mut self) {
        if self.running.is_some() {
            return;
        }
        while let Some((index, item)) = self
            .items
            .iter_mut()
            .enumerate()
            .find(|(_, item)| matches!(item.phase, Phase::Queued(_)))
        {
            let control = InstallControl::default();
            let Phase::Queued(action) =
                std::mem::replace(&mut item.phase, Phase::Running(control.clone()))
            else {
                unreachable!()
            };
            let work = Work {
                index,
                id: item.id.clone(),
                instance: item.instance.clone(),
                recovery_path: item.recovery_path.clone(),
                action,
                control,
            };
            if self
                .worker
                .as_ref()
                .is_some_and(|worker| worker.jobs.send(work).is_ok())
            {
                self.running = Some(index);
                return;
            }
            self.finish(
                index,
                Err(anyhow::anyhow!(
                    "Download worker stopped. Restart DMM to resume."
                )),
            );
        }
    }

    fn cancel(&mut self, index: usize) {
        let item = &mut self.items[index];
        match &item.phase {
            Phase::Queued(Action::Install(_)) => self.finish(index, Ok(Completion::Cancelled)),
            Phase::Queued(Action::Redeem(_)) | Phase::Waiting(_) => {
                item.phase = Phase::Queued(Action::DiscardNexus)
            }
            Phase::Running(control) => {
                control.cancel();
            }
            Phase::Queued(Action::DiscardNexus)
            | Phase::Complete
            | Phase::Cancelled
            | Phase::Failed(_) => {}
        }
        self.advance();
    }

    fn finish(&mut self, index: usize, result: Result<Completion>) {
        let item = &mut self.items[index];
        item.phase = match result {
            Ok(Completion::Installed) => {
                item.recovery_path = None;
                #[cfg(any(target_os = "linux", windows))]
                {
                    self.completed_in_batch += 1;
                }
                Phase::Complete
            }
            Ok(Completion::Browser(urls)) => Phase::Waiting(urls),
            Ok(Completion::Cancelled) => Phase::Cancelled,
            Err(error) => Phase::Failed(format!("{error:#}")),
        };
        if matches!(item.phase, Phase::Cancelled | Phase::Failed(_)) {
            self.save.extend(item.recovery_path.take());
        }
        self.refresh = true;
    }

    #[cfg(any(target_os = "linux", windows))]
    fn take_finished_batch(&mut self) -> Option<usize> {
        if self.completed_in_batch == 0 || self.items.iter().any(Entry::pending) {
            return None;
        }
        Some(std::mem::take(&mut self.completed_in_batch))
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(worker) = &self.worker {
            match worker.events.try_recv() {
                Ok(Event::Progress(index, progress)) => {
                    let item = &mut self.items[index];
                    if let InstallProgress::Downloading {
                        current,
                        total,
                        file_name,
                        received_bytes,
                        total_bytes,
                        bytes_per_second,
                    } = progress
                    {
                        if let Some(prior) = &item.transfer
                            && prior.current != current
                        {
                            item.received_before_file = item
                                .received_before_file
                                .saturating_add(prior.received_bytes);
                        }
                        item.transfer = Some(Transfer {
                            current,
                            total,
                            file_name,
                            received_bytes,
                            total_bytes,
                            bytes_per_second,
                        });
                    }
                    changed = true;
                }
                Ok(Event::Finished(index, result)) => {
                    self.running = None;
                    self.finish(index, result);
                    self.advance();
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                    if let Some(index) = self.running.take() {
                        self.finish(index, Err(anyhow::anyhow!("Download worker stopped")));
                    }
                    self.advance();
                    changed = true;
                }
            }
        }
        changed
    }
}

fn run(
    state: &Path,
    shared_client: &mut Option<SourceClient>,
    work: Work,
    progress: impl FnMut(InstallProgress),
) -> Result<Completion> {
    if matches!(work.action, Action::DiscardNexus) {
        cancel_pending(state, &work.instance, &work.id)?;
        return Ok(Completion::Cancelled);
    }
    if shared_client.is_none() {
        *shared_client = Some(source_client()?);
    }
    let client = shared_client.as_ref().expect("download client initialized");
    let sent_nexus = matches!(work.action, Action::Redeem(_));
    let result = match work.action {
        Action::Install(request) => run_remote(
            state,
            client,
            &work.instance,
            &request.details,
            &request.files,
            InstallOptions {
                destination: request.destination.as_deref(),
                keep_disabled: request.keep_disabled,
                ..Default::default()
            },
            &work.control,
            progress,
        ),
        Action::Redeem(uri) => {
            redeem_remote(state, client, &work.instance, &uri, &work.control, progress)
        }
        Action::DiscardNexus => unreachable!(),
    };
    match result {
        Ok(RemoteOutcome::Installed) => {
            if let Some(path) = &work.recovery_path {
                download_recovery::discard(path)?;
            }
            Ok(Completion::Installed)
        }
        Ok(RemoteOutcome::Browser(urls)) => Ok(Completion::Browser(urls)),
        Err(_) if work.control.is_cancelled() => {
            if sent_nexus {
                cancel_pending(state, &work.instance, &work.id)?;
            }
            Ok(Completion::Cancelled)
        }
        Err(error) => Err(error),
    }
}

impl State {
    fn toggle_completed_download(&mut self, index: usize, enabled: bool) {
        let Some(item) = self.queue.items.get(index) else { return };
        let Some(record) = item.installed_state(&self.snapshot) else { return };
        if self.queue.active().is_some_and(|(instance, applying)| {
            applying && instance == item.instance
        }) {
            return;
        }
        self.set_mod_enabled(
            record.instance.clone(),
            record.slug.clone(),
            enabled,
            Some(record.id.clone()),
        );
    }

    pub(super) fn queued_for_selected(&self, id: &RemoteMod) -> bool {
        self.snapshot
            .selected
            .as_ref()
            .is_some_and(|game| self.queue.contains(&game.name, id))
    }

    pub(super) fn pending_install(&self, id: &RemoteMod) -> Option<&'static str> {
        let instance = self.snapshot.selected.as_ref()?;
        self.queue
            .pending_mods(&instance.name)
            .find(|item| item.id == id)
            .map(|item| item.status)
    }

    pub(super) fn enqueue_install(
        &mut self,
        instance: String,
        details: ModDetails,
        files: Vec<ModFile>,
        destination: Option<PathBuf>,
        keep_disabled: bool,
    ) -> bool {
        match self.queue.enqueue(
            instance,
            Request {
                details,
                files,
                destination,
                keep_disabled,
            },
        ) {
            Ok(()) => {
                self.notice.clear();
                self.sync_previews();
                true
            }
            Err(error) => {
                self.notice = format!("{error:#}");
                false
            }
        }
    }

    pub(super) fn enqueue_nexus(&mut self, uri: String) -> bool {
        let result = (|| -> Result<()> {
            let (id, _) = nexus_identity(&uri)?;
            let pending = self
                .queue
                .items
                .iter()
                .find(|item| item.id == id && matches!(item.phase, Phase::Waiting(_)));
            let instance = pending
                .map(|item| item.instance.as_str())
                .or_else(|| {
                    self.snapshot
                        .selected
                        .as_ref()
                        .map(|game| game.name.as_str())
                })
                .context("Select the target game before receiving a Nexus download")?
                .to_owned();
            let favorite = self
                .snapshot
                .favorites
                .iter()
                .find(|favorite| favorite.item == id);
            let name = favorite.map_or_else(
                || format!("Nexus mod {}", id.id),
                |saved| saved.name.clone(),
            );
            let image = favorite.and_then(|saved| saved.image.clone());
            self.queue.redeem(instance, id, name, image, uri)
        })();
        match result {
            Ok(()) => {
                self.notice.clear();
                self.sync_previews();
                true
            }
            Err(error) => {
                self.notice = format!("{error:#}");
                false
            }
        }
    }

    pub(super) fn poll_queue(&mut self, tx: &mut Tx) -> bool {
        let previous = self.queue.running;
        let changed = self.queue.poll();
        #[cfg(any(target_os = "linux", windows))]
        if let Some(installed) = self.queue.take_finished_batch()
            && self.preferences.notify_downloads_complete
            && let Err(error) = thread::Builder::new().name("dmm-notification".into()).spawn(move || {
                let body = if installed == 1 {
                    "1 mod installed.".into()
                } else {
                    format!("{installed} mods installed.")
                };
                if let Err(error) = notify_completed_downloads(&body) {
                    eprintln!("Could not send download notification: {error:#}");
                }
            })
        {
            self.notice = format!("Could not start download notification: {error}");
        }
        if previous != self.queue.running {
            self.sync_previews();
        }
        if self.queue.running.is_some() {
            tx.wake_after(Duration::from_millis(250));
        }
        if self.busy.is_none() && self.queue.refresh {
            let paths = std::mem::take(&mut self.queue.save);
            self.queue.refresh = false;
            let selected = self
                .snapshot
                .selected
                .as_ref()
                .map(|game| game.name.clone());
            self.submit("Refreshing local mods…", move |dir| {
                let library = super::open_library(&dir)?;
                download_recovery::save(&library, &paths)?;
                Ok(Update::Library(Snapshot::load(
                    &library,
                    selected.as_deref(),
                )?))
            });
        }
        changed
    }

    pub(super) fn open_downloads(&mut self, tx: &mut Tx) {
        self.set_screen(Screen::Downloads, tx);
    }
}

#[cfg(target_os = "linux")]
fn notify_completed_downloads(body: &str) -> Result<()> {
    let connection = zbus::blocking::Connection::session()?;
    let notifications = Proxy::new(
        &connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )?;
    let hints = HashMap::<&str, Value<'_>>::from([
        ("desktop-entry", Value::from("com.dmm.ModManager")),
    ]);
    let actions: &[&str] = &[];
    let _: u32 = notifications.call(
        "Notify",
        &("DMM", 0u32, "", "Download queue finished", body, actions, hints, -1i32),
    )?;
    Ok(())
}

#[cfg(test)]
impl Queue {
    pub(super) fn fixture(&mut self, state: &Path) -> Result<impl FnMut() -> bool + use<>> {
        self.recovery = Some(Arc::new(Recovery::open(state)?));
        let (jobs, requests) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        self.worker = Some(Worker {
            jobs,
            events: receiver,
        });
        Ok(move || {
            let _keep_open = &events;
            requests.try_recv().is_ok()
        })
    }
}

#[cfg(test)]
mod tests;
