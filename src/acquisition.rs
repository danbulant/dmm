use std::{
    collections::{BTreeMap, VecDeque},
    env,
    error::Error,
    fmt,
    fs::{self, OpenOptions},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use tempfile::Builder;
use url::Url;

use crate::{
    library::{InstallOptions, Library, PendingDownload, Release},
    sources::{DownloadAction, ModDetails, ModFile, RemoteMod, SourceClient},
};
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallProgress {
    Downloading {
        current: usize,
        total: usize,
        file_name: String,
        received_bytes: u64,
        total_bytes: Option<u64>,
        bytes_per_second: u64,
    },
    Deploying,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteOutcome {
    Installed,
    Browser(Vec<Url>),
}

#[derive(Clone, Default)]
pub struct InstallControl(Arc<AtomicU8>);

impl InstallControl {
    pub fn cancel(&self) -> bool {
        self.0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == 1
    }

    pub fn is_applying(&self) -> bool {
        self.0.load(Ordering::Acquire) == 2
    }

    fn begin_apply(&self) -> Result<()> {
        self.0
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| anyhow::Error::new(InstallError::Cancelled))
    }

    fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(anyhow::Error::new(InstallError::Cancelled))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
pub enum InstallError {
    Cancelled,
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("installation cancelled"),
        }
    }
}

impl Error for InstallError {}

pub fn nexus_identity(uri: &str) -> Result<(RemoteMod, u64)> {
    crate::sources::nexus_identity(uri)
}

pub fn cancel_pending(state_dir: &Path, instance: &str, id: &RemoteMod) -> Result<()> {
    Library::open_wait(state_dir, Duration::from_secs(5))?.cancel_pending(instance, id)
}

pub fn run_remote(
    state_dir: &Path,
    client: &SourceClient,
    instance: &str,
    details: &ModDetails,
    files: &[ModFile],
    options: InstallOptions<'_>,
    control: &InstallControl,
    mut progress: impl FnMut(InstallProgress),
) -> Result<RemoteOutcome> {
    ensure!(!files.is_empty(), "select at least one provider file");
    let mut selected_ids = std::collections::BTreeSet::new();
    ensure!(
        files.iter().all(|file| selected_ids.insert(file.id)),
        "a provider file was selected more than once"
    );
    control.check()?;
    let (slug, releases) = {
        let library = Library::open_wait(state_dir, Duration::from_secs(5))?;
        let profile = library.instance(instance)?.profile()?;
        let game = profile
            .source_game(details.id.source)
            .with_context(|| format!("this profile has no {} game ID", details.id.source))?;
        ensure!(
            game == details.id.game.as_str(),
            "selected mod is for {}, but instance {instance} uses {}",
            details.id.game,
            game
        );
        let slug = library
            .remote_slug(instance, &details.id)?
            .unwrap_or_else(|| {
                format!(
                    "{}-{}-{}",
                    details.id.source, details.id.game, details.id.id
                )
            });
        let releases = files
            .iter()
            .map(|file| release(details, file))
            .collect::<Vec<_>>();
        (slug, releases)
    };
    let scratch = download_scratch(state_dir)?;
    let mut archives = Vec::with_capacity(files.len());
    let mut handoffs = Vec::new();
    for (index, (file, release)) in files.iter().zip(&releases).enumerate() {
        control.check()?;
        match client.download(&details.id, file)? {
            DownloadAction::Direct(url) => {
                let input = scratch.path().join(format!("archive-{index}"));
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&input)?;
                fetch_progress(
                    client,
                    &url,
                    &mut output,
                    control,
                    &mut progress,
                    index + 1,
                    files.len(),
                    &file.name,
                )?;
                drop(output);
                archives.push((input, release.clone()));
            }
            DownloadAction::OpenBrowser(url) => handoffs.push(url),
        }
    }
    ensure!(
        handoffs.is_empty() || archives.is_empty(),
        "provider mixed direct downloads and browser handoffs; no files were installed"
    );
    if !handoffs.is_empty() {
        control.begin_apply()?;
        let request = PendingDownload {
            instance: instance.to_owned(),
            slug,
            release: releases[0].clone(),
            bundle: Vec::new(),
            details: None,
            received: BTreeMap::new(),
            destination: options.destination.map(ToOwned::to_owned),
            keep_disabled: options.keep_disabled,
            replace_unmanaged: options.replace_unmanaged,
            group: options.group.map(str::to_owned),
            allow_multiple: options.allow_multiple,
        };
        Library::open_wait(state_dir, Duration::from_secs(5))?
            .await_bundle(&request, &releases, details)?;
        return Ok(RemoteOutcome::Browser(handoffs));
    }
    control.check()?;
    progress(InstallProgress::Deploying);
    control.begin_apply()?;
    Library::open_wait(state_dir, Duration::from_secs(5))?
        .install_remote_bundle(instance, &slug, &archives, details, options)?;
    Ok(RemoteOutcome::Installed)
}

pub fn redeem_remote(
    state_dir: &Path,
    client: &SourceClient,
    instance: &str,
    uri: &str,
    control: &InstallControl,
    mut progress: impl FnMut(InstallProgress),
) -> Result<RemoteOutcome> {
    control.check()?;
    let (expected_id, expected_file_id) = nexus_identity(uri)?;
    let pending = {
        let library = Library::open_wait(state_dir, Duration::from_secs(5))?;
        let pending = library.pending_download(&expected_id, expected_file_id)?;
        ensure!(
            pending.instance == instance,
            "Nexus download belongs to {}, not {instance}",
            pending.instance
        );
        pending
    };
    let (id, file_id, url) = client.redeem(uri)?;
    ensure!(
        id == expected_id && file_id == expected_file_id,
        "Nexus returned a different file"
    );
    let scratch = download_scratch(state_dir)?;
    let input = scratch.path().join("archive");
    if !pending.received.contains_key(&file_id) {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&input)?;
        fetch_progress(
            client,
            &url,
            &mut output,
            control,
            &mut progress,
            pending.received.len() + 1,
            pending.bundle.len().max(1),
            &pending.release.filename,
        )?;
        drop(output);
    }
    control.check()?;
    let mut library = Library::open_wait(state_dir, Duration::from_secs(5))?;
    let latest = library.pending_download(&id, file_id)?;
    ensure!(
        latest.instance == pending.instance
            && latest.slug == pending.slug
            && latest.release == pending.release
            && latest.bundle == pending.bundle
            && latest.destination == pending.destination
            && latest.keep_disabled == pending.keep_disabled
            && latest.replace_unmanaged == pending.replace_unmanaged
            && latest.group == pending.group
            && latest.allow_multiple == pending.allow_multiple,
        "Nexus installation target changed during download; retry the download"
    );
    let archive = latest
        .received
        .get(&file_id)
        .cloned()
        .unwrap_or_else(|| input.clone());
    if latest.bundle.is_empty() {
        progress(InstallProgress::Deploying);
        control.begin_apply()?;
        library.install(
            &latest.instance,
            &latest.slug,
            &archive,
            Some(latest.release.clone()),
            InstallOptions {
                destination: latest.destination.as_deref(),
                keep_disabled: latest.keep_disabled,
                replace_unmanaged: latest.replace_unmanaged,
                group: latest.group.as_deref(),
                allow_multiple: latest.allow_multiple,
            },
        )?;
        library.finish_download(&id, file_id)?;
        return Ok(RemoteOutcome::Installed);
    }
    control.begin_apply()?;
    let pending = if latest.received.contains_key(&file_id) {
        latest
    } else {
        library.record_pending_archive(&id, file_id, &archive)?
    };
    let remaining: Vec<_> = pending
        .bundle
        .iter()
        .filter(|item| !pending.received.contains_key(&item.file_id))
        .collect();
    if !remaining.is_empty() {
        return Ok(RemoteOutcome::Browser(
            remaining
                .into_iter()
                .map(nexus_file_page)
                .collect::<Result<Vec<_>>>()?,
        ));
    }
    let details = pending
        .details
        .as_ref()
        .context("pending bundle is missing its saved mod details")?;
    let archives = pending
        .bundle
        .iter()
        .map(|release| {
            let path = pending
                .received
                .get(&release.file_id)
                .context("pending bundle is missing a downloaded archive")?;
            Ok((path.clone(), release.clone()))
        })
        .collect::<Result<Vec<_>>>()?;
    progress(InstallProgress::Deploying);
    library.install_remote_bundle(
        &pending.instance,
        &pending.slug,
        &archives,
        details,
        InstallOptions {
            destination: pending.destination.as_deref(),
            keep_disabled: pending.keep_disabled,
            replace_unmanaged: pending.replace_unmanaged,
            group: pending.group.as_deref(),
            allow_multiple: pending.allow_multiple,
        },
    )?;
    library.finish_bundle(&id, &pending.bundle)?;
    Ok(RemoteOutcome::Installed)
}

struct DownloadRate(VecDeque<(Instant, u64)>);

impl DownloadRate {
    const WINDOW: Duration = Duration::from_secs(3);

    fn new(start: Instant) -> Self {
        Self(VecDeque::from([(start, 0)]))
    }

    fn sample(&mut self, now: Instant, bytes: u64) -> u64 {
        while self
            .0
            .get(1)
            .is_some_and(|(time, _)| now.duration_since(*time) >= Self::WINDOW)
        {
            self.0.pop_front();
        }
        let (start, prior) = self.0[0];
        self.0.push_back((now, bytes));
        let seconds = now.duration_since(start).as_secs_f64().max(0.2);
        (bytes.saturating_sub(prior) as f64 / seconds) as u64
    }
}

fn fetch_progress(
    client: &SourceClient,
    url: &Url,
    output: &mut std::fs::File,
    control: &InstallControl,
    progress: &mut impl FnMut(InstallProgress),
    current: usize,
    total: usize,
    file_name: &str,
) -> Result<()> {
    let mut rate = DownloadRate::new(Instant::now());
    let mut last_event = None;
    let mut last_bytes = 0;
    let mut last_total = None;
    client.fetch_observed(url, output, |received, total_bytes| {
        control.check()?;
        let now = Instant::now();
        if last_event.is_none_or(|last| now.duration_since(last) >= Duration::from_millis(200))
            || total_bytes != last_total
            || (received != last_bytes && total_bytes == Some(received))
        {
            let speed = rate.sample(now, received);
            progress(InstallProgress::Downloading {
                current,
                total,
                file_name: file_name.to_owned(),
                received_bytes: received,
                total_bytes,
                bytes_per_second: speed,
            });
            last_event = Some(now);
            last_bytes = received;
            last_total = total_bytes;
        }
        Ok(())
    })?;
    let received = output.metadata()?.len();
    if received != last_bytes || last_total != Some(received) {
        let speed = rate.sample(Instant::now(), received);
        progress(InstallProgress::Downloading {
            current,
            total,
            file_name: file_name.to_owned(),
            received_bytes: received,
            total_bytes: Some(received),
            bytes_per_second: speed,
        });
    }
    Ok(())
}

fn download_scratch(state_dir: &Path) -> Result<tempfile::TempDir> {
    let root = state_dir.join("download-scratch");
    fs::create_dir_all(&root)?;
    Ok(Builder::new().prefix("remote-").tempdir_in(root)?)
}
pub fn pending_pages(library: &Library, instance: &str) -> Result<Vec<Url>> {
    library
        .pending_downloads(instance)?
        .into_iter()
        .filter(|pending| pending.release.id.source == crate::sources::Source::Nexus)
        .filter(|pending| {
            pending.bundle.is_empty()
                || pending
                    .bundle
                    .iter()
                    .all(|release| pending.received.contains_key(&release.file_id))
                || !pending.received.contains_key(&pending.release.file_id)
        })
        .map(|pending| nexus_file_page(&pending.release))
        .collect()
}

fn nexus_file_page(release: &Release) -> Result<Url> {
    let mut page = Url::parse(&format!(
        "https://www.nexusmods.com/{}/mods/{}",
        release.id.game, release.id.id
    ))?;
    page.query_pairs_mut()
        .append_pair("tab", "files")
        .append_pair("file_id", &release.file_id.to_string());
    Ok(page)
}

pub fn source_client() -> Result<SourceClient> {
    SourceClient::new(env::var("NEXUS_API_KEY").ok().filter(|key| !key.is_empty()))
}

pub fn install_remote(
    library: &mut Library,
    client: &SourceClient,
    instance: &str,
    details: &ModDetails,
    files: &[ModFile],
    options: InstallOptions<'_>,
    mut progress: impl FnMut(InstallProgress),
) -> Result<RemoteOutcome> {
    ensure!(!files.is_empty(), "select at least one provider file");
    let mut selected_ids = std::collections::BTreeSet::new();
    ensure!(
        files.iter().all(|file| selected_ids.insert(file.id)),
        "a provider file was selected more than once"
    );
    let profile = library.instance(instance)?.profile()?;
    let game = profile
        .source_game(details.id.source)
        .with_context(|| format!("this profile has no {} game ID", details.id.source))?;
    ensure!(
        game == details.id.game.as_str(),
        "selected mod is for {}, but instance {instance} uses {}",
        details.id.game,
        game
    );
    let canonical_slug = format!(
        "{}-{}-{}",
        details.id.source, details.id.game, details.id.id
    );
    let slug = library
        .remote_slug(instance, &details.id)?
        .unwrap_or(canonical_slug);
    let releases = files
        .iter()
        .map(|file| release(details, file))
        .collect::<Vec<_>>();
    let scratch = library.scratch()?;
    let mut archives = Vec::with_capacity(files.len());
    let mut handoffs = Vec::new();
    for (index, (file, release)) in files.iter().zip(&releases).enumerate() {
        progress(InstallProgress::Downloading {
            current: index + 1,
            total: files.len(),
            file_name: file.name.clone(),
            received_bytes: 0,
            total_bytes: file.size,
            bytes_per_second: 0,
        });
        match client.download(&details.id, file)? {
            DownloadAction::Direct(url) => {
                let input = scratch.path().join(format!("archive-{index}"));
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&input)?;
                client.fetch(&url, &mut output)?;
                drop(output);
                archives.push((input, release.clone()));
            }
            DownloadAction::OpenBrowser(url) => handoffs.push(url),
        }
    }
    ensure!(
        handoffs.is_empty() || archives.is_empty(),
        "provider mixed direct downloads and browser handoffs; no files were installed"
    );
    if !handoffs.is_empty() {
        let request = PendingDownload {
            instance: instance.to_owned(),
            slug,
            release: releases[0].clone(),
            bundle: Vec::new(),
            details: None,
            received: BTreeMap::new(),
            destination: options.destination.map(ToOwned::to_owned),
            keep_disabled: options.keep_disabled,
            replace_unmanaged: options.replace_unmanaged,
            group: options.group.map(str::to_owned),
            allow_multiple: options.allow_multiple,
        };
        library.await_bundle(&request, &releases, details)?;
        return Ok(RemoteOutcome::Browser(handoffs));
    }
    progress(InstallProgress::Deploying);
    library.install_remote_bundle(instance, &slug, &archives, details, options)?;
    Ok(RemoteOutcome::Installed)
}

pub fn accept_nxm(
    library: &mut Library,
    client: &SourceClient,
    uri: &str,
    mut progress: impl FnMut(InstallProgress),
) -> Result<RemoteOutcome> {
    let (id, file_id, url) = client.redeem(uri)?;
    let pending = library.pending_download(&id, file_id)?;
    let dir = library.scratch()?;
    let input = dir.path().join("archive");
    if !pending.received.contains_key(&file_id) {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&input)?;
        progress(InstallProgress::Downloading {
            current: pending.received.len() + 1,
            total: pending.bundle.len().max(1),
            file_name: pending.release.filename.clone(),
            received_bytes: 0,
            total_bytes: pending.details.as_ref().and_then(|details| {
                details
                    .files
                    .iter()
                    .find(|file| file.id == file_id)
                    .and_then(|file| file.size)
            }),
            bytes_per_second: 0,
        });
        client.fetch(&url, &mut output)?;
        drop(output);
    }
    if pending.bundle.is_empty() {
        library.install(
            &pending.instance,
            &pending.slug,
            &input,
            Some(pending.release.clone()),
            InstallOptions {
                destination: pending.destination.as_deref(),
                keep_disabled: pending.keep_disabled,
                replace_unmanaged: pending.replace_unmanaged,
                group: pending.group.as_deref(),
                allow_multiple: pending.allow_multiple,
            },
        )?;
        library.finish_download(&id, file_id)?;
        return Ok(RemoteOutcome::Installed);
    }
    let pending = library.record_pending_archive(&id, file_id, &input)?;
    let remaining: Vec<_> = pending
        .bundle
        .iter()
        .filter(|release| !pending.received.contains_key(&release.file_id))
        .collect();
    if !remaining.is_empty() {
        return Ok(RemoteOutcome::Browser(
            remaining
                .into_iter()
                .map(nexus_file_page)
                .collect::<Result<Vec<_>>>()?,
        ));
    }
    let details = pending
        .details
        .as_ref()
        .context("pending bundle is missing its saved mod details")?;
    let archives = pending
        .bundle
        .iter()
        .map(|release| {
            let path = pending
                .received
                .get(&release.file_id)
                .context("pending bundle is missing a downloaded archive")?;
            Ok((path.clone(), release.clone()))
        })
        .collect::<Result<Vec<_>>>()?;
    progress(InstallProgress::Deploying);
    library.install_remote_bundle(
        &pending.instance,
        &pending.slug,
        &archives,
        details,
        InstallOptions {
            destination: pending.destination.as_deref(),
            keep_disabled: pending.keep_disabled,
            replace_unmanaged: pending.replace_unmanaged,
            group: pending.group.as_deref(),
            allow_multiple: pending.allow_multiple,
        },
    )?;
    library.finish_bundle(&id, &pending.bundle)?;
    Ok(RemoteOutcome::Installed)
}

fn release(details: &ModDetails, file: &ModFile) -> Release {
    Release {
        id: details.id.clone(),
        source_url: details.url.clone(),
        file_id: file.id,
        filename: file.name.clone(),
        version: file.version.clone(),
        updated_at: file.updated_at.clone(),
    }
}

pub fn install_archive(
    library: &mut Library,
    client: &SourceClient,
    url: &Url,
    instance: &str,
    slug: &str,
    release: Release,
    options: InstallOptions<'_>,
) -> Result<()> {
    let dir = library.scratch()?;
    let input = dir.path().join("archive");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&input)?;
    client.fetch(url, &mut output)?;
    drop(output);
    library.install(instance, slug, &input, Some(release), options)
}
pub fn install_archive_with_details(
    library: &mut Library,
    client: &SourceClient,
    url: &Url,
    instance: &str,
    slug: &str,
    release: Release,
    details: &ModDetails,
    options: InstallOptions<'_>,
) -> Result<()> {
    let dir = library.scratch()?;
    let input = dir.path().join("archive");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&input)?;
    client.fetch(url, &mut output)?;
    drop(output);
    library.install_with_metadata(
        instance,
        slug,
        &input,
        Some(release.clone()),
        Some(details.clone()),
        &[release],
        &BTreeMap::new(),
        options,
    )
}
#[cfg(test)]
mod tests {
    use super::{DownloadRate, Duration, InstallControl, InstallError, Instant};

    #[test]
    fn download_rate_smooths_bursts_and_falls_to_zero_after_a_stall() {
        let start = Instant::now();
        let mut rate = DownloadRate::new(start);
        let sample = |rate: &mut DownloadRate, millis, bytes| {
            rate.sample(start + Duration::from_millis(millis), bytes)
        };
        assert_eq!(sample(&mut rate, 1000, 2000), 2000);
        assert_eq!(sample(&mut rate, 2000, 4000), 2000);
        assert_eq!(sample(&mut rate, 2250, 4000), 1777);
        assert_eq!(sample(&mut rate, 3000, 6000), 2000);
        assert_eq!(sample(&mut rate, 4000, 6000), 1333);
        assert_eq!(sample(&mut rate, 6000, 6000), 0);
    }

    #[test]
    fn cancellation_and_apply_boundary_are_atomic() {
        let control = InstallControl::default();
        assert!(control.cancel());
        assert!(control.is_cancelled());
        assert!(
            control
                .begin_apply()
                .unwrap_err()
                .downcast_ref::<InstallError>()
                .is_some()
        );

        let control = InstallControl::default();
        control.begin_apply().unwrap();
        assert!(control.is_applying());
        assert!(!control.cancel());
    }
}
