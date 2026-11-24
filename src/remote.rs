use anyhow::{Context, Result, ensure};
use clap::Args;
use dmm::{
    acquisition::{
        RemoteOutcome, accept_nxm as acquire_nxm, install_archive_with_details, install_remote,
        source_client,
    },
    library::{InstallOptions, Library, Release, SavedSearch},
    sources::{
        DownloadAction, Maturity, ModFile, RemoteMod, SearchQuery, Sort, Source, SourceClient,
    },
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
    source: Source,
    mod_id: u64,
    #[arg(num_args = 1..)]
    file_ids: Vec<u64>,
    #[command(flatten)]
    flags: InstallFlags,
    /// Print Nexus handoff URLs instead of opening the browser.
    #[arg(long)]
    no_browser: bool,
}

pub(crate) fn client() -> Result<SourceClient> {
    source_client()
}

pub(crate) fn search(library: &Library, search: SavedSearch, instance: Option<&str>) -> Result<()> {
    let mut query = search.query.clone();
    if search.source == Source::GameBanana {
        query.maturity = Maturity::All;
    }
    let results = client()?.search(search.source, &search.game, &query)?;
    let favorites = library.saved_favorites()?;
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
            json!({
                "id": item.id,
                "saved": favorites.iter().any(|saved| saved.item == item.id),
                "installed": slugs
            })
        })
        .collect::<Vec<_>>();
    print(&json!({ "results": results, "overlays": overlays }))
}

pub(crate) fn install(library: &mut Library, input: RemoteInstall) -> Result<()> {
    let kind = library.instance(&input.instance)?.kind;
    let game = kind
        .source_game(input.source)
        .with_context(|| format!("{} has no {} game ID", kind.display_name(), input.source))?;
    let id = RemoteMod {
        source: input.source,
        game: game.to_owned(),
        id: input.mod_id,
    };
    let client = client()?;
    let details = client.details(&id)?;
    let files = input
        .file_ids
        .iter()
        .map(|file_id| {
            details
                .files
                .iter()
                .find(|file| file.id == *file_id)
                .with_context(|| format!("file ID {file_id} does not belong to that mod"))
        })
        .collect::<Result<Vec<_>>>()?;
    let files = files.into_iter().cloned().collect::<Vec<_>>();
    match install_remote(
        library,
        &client,
        &input.instance,
        &details,
        &files,
        input.flags.options(),
        |_| {},
    )? {
        RemoteOutcome::Installed => Ok(()),
        RemoteOutcome::Browser(urls) => {
            print(&json!({
                "action": "open_browser",
                "urls": urls,
                "then": "Pick Mod Manager Download for every selected file; pass each nxm:// link to accept-nxm."
            }))?;
            if !input.no_browser {
                for url in urls {
                    open::that(url.as_str())?;
                }
            }
            Ok(())
        }
    }
}

pub(crate) fn accept_nxm(library: &mut Library, uri: &str) -> Result<()> {
    let client = client()?;
    match acquire_nxm(library, &client, uri, |_| {})? {
        RemoteOutcome::Installed => Ok(()),
        RemoteOutcome::Browser(urls) => print(&json!({ "action": "open_browser", "urls": urls })),
    }
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
            if record.is_bundle() {
                return Ok(json!({
                    "mod": record.slug,
                    "status": "choose_bundle_files",
                    "files": details.files,
                    "installed_files": record.releases()
                }));
            }
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
            let next = Release {
                id: old.id.clone(),
                source_url: details.url.clone(),
                file_id: file.id,
                filename: file.name.clone(),
                version: file.version.clone(),
                updated_at: file.updated_at.clone(),
            };
            let status = if !apply {
                "available"
            } else if let DownloadAction::Direct(url) = client.download(&old.id, file)? {
                install_archive_with_details(
                    library,
                    &client,
                    &url,
                    instance,
                    &record.slug,
                    next.clone(),
                    &details,
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
