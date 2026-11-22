use anyhow::{Context, Result, ensure};
use clap::Args;
use dmm::{
    acquisition::{
        RemoteOutcome, RemoteSelection, accept_nxm as acquire_nxm, install_archive, install_remote,
        source_client,
    },
    library::{InstallOptions, Library, Release, SavedSearch},
    sources::{DownloadAction, ModFile, RemoteMod, SearchQuery, Sort, Source, SourceClient},
};
use serde_json::json;

use crate::{InstallFlags, print};

#[derive(Args)]
pub(crate) struct Identity {
    source: Source,
    game: String,
    mod_id: u64,
}

impl From<Identity> for RemoteMod {
    fn from(value: Identity) -> Self {
        Self {
            source: value.source,
            game: value.game,
            id: value.mod_id,
        }
    }
}

#[derive(Args)]
pub(crate) struct Browse {
    source: Source,
    game: String,
    #[arg(default_value = "")]
    text: String,
    #[arg(long, default_value_t = 1)]
    page: u32,
    #[arg(long)]
    category: Option<String>,
    #[arg(long)]
    sort: Option<Sort>,
    /// Overlay installations from a registered instance.
    #[arg(long)]
    pub instance: Option<String>,
}

impl From<Browse> for SavedSearch {
    fn from(value: Browse) -> Self {
        Self {
            source: value.source,
            game: value.game,
            query: SearchQuery {
                text: value.text,
                page: value.page,
                category: value.category,
                sort: value.sort.unwrap_or_default(),
                ..SearchQuery::default()
            },
        }
    }
}

#[derive(Args)]
pub(crate) struct RemoteInstall {
    instance: String,
    slug: String,
    source: Source,
    mod_id: u64,
    file_id: u64,
    #[command(flatten)]
    flags: InstallFlags,
    /// Print the Nexus handoff URL instead of opening the browser.
    #[arg(long)]
    no_browser: bool,
}

pub(crate) fn client() -> Result<SourceClient> {
    source_client()
}

pub(crate) fn search(library: &Library, search: SavedSearch, instance: Option<&str>) -> Result<()> {
    let results = client()?.search(search.source, &search.game, &search.query)?;
    let favorites = library.favorites()?;
    let installed = instance
        .map(|name| library.mods(name))
        .transpose()?
        .unwrap_or_default();
    let overlays = results
        .items
        .iter()
        .map(|item| {
            let slugs: Vec<_> = installed
                .iter()
                .filter(|m| m.release().is_some_and(|r| r.id == item.id))
                .map(|m| (&m.slug, m.enabled))
                .collect();
            json!({ "id": item.id, "saved": favorites.contains(&item.id), "installed": slugs })
        })
        .collect::<Vec<_>>();
    print(&json!({ "results": results, "overlays": overlays }))
}

pub(crate) fn install(library: &mut Library, input: RemoteInstall) -> Result<()> {
    let game = library.instance(&input.instance)?.profile()?;
    let game = game
        .source_game(input.source)
        .with_context(|| format!("this profile has no {} game ID", input.source))?;
    let id = RemoteMod {
        source: input.source,
        game: game.to_owned(),
        id: input.mod_id,
    };
    let client = client()?;
    let details = client.details(&id)?;
    let file = details
        .files
        .iter()
        .find(|file| file.id == input.file_id)
        .context("file ID does not belong to that mod; use details to list file choices")?;
    match install_remote(
        library,
        &client,
        &input.instance,
        &input.slug,
        RemoteSelection {
            id: &id,
            file,
            source_url: details.url.as_deref(),
        },
        input.flags.options(),
    )? {
        RemoteOutcome::Installed => Ok(()),
        RemoteOutcome::Browser(url) => {
            print(
                &json!({ "action": "open_browser", "url": url, "then": "Pick Mod Manager Download; pass the nxm:// link to accept-nxm." }),
            )?;
            if !input.no_browser {
                open::that(url.as_str())?;
            }
            Ok(())
        }
    }
}

pub(crate) fn accept_nxm(library: &mut Library, uri: &str) -> Result<()> {
    let client = client()?;
    acquire_nxm(library, &client, uri)
}

pub(crate) fn updates(library: &mut Library, instance: &str, apply: bool) -> Result<()> {
    let client = client()?;
    let mut results = Vec::new();
    let mut failures = 0;
    for record in library.mods(instance)? {
        let Some(old) = record.release() else {
            continue;
        };
        let result = (|| -> Result<_> {
            let details = client.details(&old.id)?;
            let Some(file) = candidate(old, &details.files) else {
                let known = details.files.iter().any(|file| file.id == old.file_id);
                let newer_choices = details
                    .files
                    .iter()
                    .any(|file| file.id != old.file_id && later(&file.updated_at, &old.updated_at));
                return Ok(if !known || newer_choices {
                    json!({"mod": record.slug, "status": "choose_file", "files": details.files})
                } else {
                    json!({"mod": record.slug, "status": "current"})
                });
            };
            let next = RemoteSelection {
                id: &old.id,
                file,
                source_url: details.url.as_deref(),
            }
            .release();
            let status = if !apply {
                "available"
            } else if let DownloadAction::Direct(url) = client.download(&old.id, file)? {
                install_archive(
                    library,
                    &client,
                    &url,
                    instance,
                    &record.slug,
                    next.clone(),
                    InstallOptions::default(),
                )?;
                "updated"
            } else {
                "browser_required"
            };
            Ok(json!({ "mod": record.slug, "status": status, "file": next }))
        })();
        match result {
            Ok(result) => results.push(result),
            Err(err) => {
                failures += 1;
                results.push(
                    json!({ "mod": record.slug, "status": "error", "error": format!("{err:#}") }),
                );
            }
        }
    }
    print(&results)?;
    ensure!(failures == 0, "{failures} update checks failed");
    Ok(())
}

fn later(next: &Option<String>, previous: &Option<String>) -> bool {
    match (next.as_deref(), previous.as_deref()) {
        (Some(a), Some(b)) => match (a.parse::<i64>(), b.parse::<i64>()) {
            (Ok(a), Ok(b)) => a > b,
            _ => a > b,
        },
        _ => false,
    }
}

fn candidate<'a>(old: &Release, files: &'a [ModFile]) -> Option<&'a ModFile> {
    if let Some(file) = files.iter().find(|file| file.id == old.file_id)
        && (later(&file.updated_at, &old.updated_at)
            || file.version.as_ref().is_some_and(|version| {
                old.version
                    .as_ref()
                    .is_some_and(|previous| previous != version)
            }))
    {
        return Some(file);
    }
    let mut matches = files.iter().filter(|file| {
        file.id != old.file_id
            && file.name == old.filename
            && later(&file.updated_at, &old.updated_at)
    });
    let only = matches.next()?;
    matches.next().is_none().then_some(only)
}
