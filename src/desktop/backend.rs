use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use dmm::acquisition::source_client;
use dmm::games::GameKind;
use dmm::games::discovery::Discovery;
#[cfg(target_os = "linux")]
use dmm::games::xxmi::steam_xxmi::SteamXxmi;
use dmm::library::{Inspection, Library, SavedFavorite};
use dmm::sources::{FilterCatalog, ModDetails, SearchPage, SearchQuery, SourceClient};
use rune::Waker;

use super::browse::DetailsIntent;
use super::{Origin, Snapshot};

pub(super) enum Update {
    Library(Snapshot),
    Favorites(Vec<SavedFavorite>),
    ModRenamed {
        instance: String,
        slug: String,
        enabled: bool,
    },
    LocalPlan(Inspection),
    Discovered(Snapshot, Discovery),
    #[cfg(target_os = "linux")]
    SteamXxmiInstalled(Result<SteamXxmi>),
    XxmiStarted(Child, PathBuf),
    XxmiGameStarted(GameKind, Result<Child>),
    GameChanged(GameKind, Snapshot),
    Message(String),
    Search(u64, SearchQuery, Result<SearchPage>),
    Filters(Origin, Result<FilterCatalog>),
    Details {
        revision: u64,
        intent: DetailsIntent,
        result: Result<ModDetails>,
    },
}

pub(super) type Job = Box<dyn FnOnce(&mut Backend) -> Result<Update> + Send>;

pub(super) struct Backend {
    pub(super) state_dir: PathBuf,
    pub(super) online: Option<SourceClient>,
}

impl Backend {
    pub(super) fn online(&mut self) -> Result<&SourceClient> {
        if self.online.is_none() {
            self.online =
                Some(source_client()?.with_cache(&self.state_dir.join("source-cache.sqlite3")));
        }
        Ok(self.online.as_ref().expect("initialized client"))
    }
}

pub(super) struct Worker {
    pub(super) jobs: Sender<Job>,
    pub(super) results: Receiver<Result<Update>>,
}

impl Worker {
    pub(super) fn start(name: &str, state_dir: PathBuf, wake: Waker) -> Result<Self> {
        let (jobs, requests) = mpsc::channel::<Job>();
        let (responses, results) = mpsc::channel();
        thread::Builder::new().name(name.into()).spawn(move || {
            let mut backend = Backend {
                state_dir,
                online: None,
            };
            while let Ok(job) = requests.recv() {
                if responses.send(job(&mut backend)).is_err() {
                    break;
                }
                wake();
            }
        })?;
        Ok(Self { jobs, results })
    }

    pub(super) fn receive(
        worker: Option<&Self>,
        busy: &mut Option<&'static str>,
    ) -> Option<Result<Update>> {
        match worker?.results.try_recv() {
            Ok(result) => {
                *busy = None;
                Some(result)
            }
            Err(TryRecvError::Disconnected) if busy.take().is_some() => Some(Err(anyhow::anyhow!(
                "The background worker stopped. Restart DMM to try again."
            ))),
            Err(_) => None,
        }
    }
}

pub(super) fn open_library(path: &Path) -> Result<Library> {
    Library::open_wait(path, Duration::from_secs(5))
}
