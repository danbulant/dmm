use std::{env, fs::OpenOptions};

use anyhow::{Context, Result, ensure};
use url::Url;

use crate::{
    library::{InstallOptions, Library, PendingDownload, Release},
    sources::{DownloadAction, ModFile, RemoteMod, SourceClient},
};

pub struct RemoteSelection<'a> {
    pub id: &'a RemoteMod,
    pub file: &'a ModFile,
    pub source_url: Option<&'a str>,
}

impl RemoteSelection<'_> {
    pub fn release(&self) -> Release {
        Release {
            id: self.id.clone(),
            source_url: self.source_url.map(str::to_owned),
            file_id: self.file.id,
            filename: self.file.name.clone(),
            version: self.file.version.clone(),
            updated_at: self.file.updated_at.clone(),
        }
    }
}

pub enum RemoteOutcome {
    Installed,
    Browser(Url),
}

pub fn source_client() -> Result<SourceClient> {
    SourceClient::new(env::var("NEXUS_API_KEY").ok().filter(|key| !key.is_empty()))
}

pub fn install_remote(
    library: &mut Library,
    client: &SourceClient,
    instance: &str,
    slug: &str,
    selection: RemoteSelection<'_>,
    options: InstallOptions<'_>,
) -> Result<RemoteOutcome> {
    let profile = library.instance(instance)?.profile()?;
    let game = profile
        .source_game(selection.id.source)
        .with_context(|| format!("this profile has no {} game ID", selection.id.source))?;
    ensure!(
        game == selection.id.game.as_str(),
        "selected mod is for {}, but instance {instance} uses {}",
        selection.id.game,
        game
    );

    let release = selection.release();
    match client.download(selection.id, selection.file)? {
        DownloadAction::Direct(url) => {
            install_archive(library, client, &url, instance, slug, release, options)?;
            Ok(RemoteOutcome::Installed)
        }
        DownloadAction::OpenBrowser(url) => {
            library.await_download(&PendingDownload {
                instance: instance.to_owned(),
                slug: slug.to_owned(),
                release,
                destination: options.destination.map(ToOwned::to_owned),
                keep_disabled: options.keep_disabled,
                replace_unmanaged: options.replace_unmanaged,
                group: options.group.map(str::to_owned),
                allow_multiple: options.allow_multiple,
            })?;
            Ok(RemoteOutcome::Browser(url))
        }
    }
}

pub fn accept_nxm(library: &mut Library, client: &SourceClient, uri: &str) -> Result<()> {
    let (id, file_id, url) = client.redeem(uri)?;
    let pending = library.pending_download(&id, file_id)?;
    install_archive(
        library,
        client,
        &url,
        &pending.instance,
        &pending.slug,
        pending.release,
        InstallOptions {
            destination: pending.destination.as_deref(),
            keep_disabled: pending.keep_disabled,
            replace_unmanaged: pending.replace_unmanaged,
            group: pending.group.as_deref(),
            allow_multiple: pending.allow_multiple,
        },
    )?;
    library.finish_download(&id, file_id)
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
