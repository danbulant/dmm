use std::path::PathBuf;
use std::process::Child;
#[cfg(target_os = "linux")]
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use dmm::games::GameKind;
use dmm::games::xxmi::game_process::running_games;
use dmm::games::xxmi::{launch_xxmi, launcher_window};
#[cfg(target_os = "linux")]
use dmm::games::xxmi::steam_xxmi::{InstallStage, SteamXxmi};
use dmm::library::LaunchSettings;
use rune::Tx;

#[cfg(target_os = "linux")]
use super::SteamDialog;
use super::{State, Update};

pub(super) enum XxmiLaunch {
    Idle,
    Starting,
    Running(Child, PathBuf),
    Failed(String),
}

impl XxmiLaunch {
    pub(super) fn active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running(..))
    }

    pub(super) fn label<'a>(&self, idle: &'a str) -> &'a str {
        match self {
            Self::Starting => "Starting XXMI…",
            Self::Running(..) => "XXMI is running",
            Self::Idle | Self::Failed(_) => idle,
        }
    }

    pub(super) fn poll(&mut self, tx: &mut Tx) -> bool {
        let Self::Running(child, _) = self else {
            return false;
        };
        *self = match child.try_wait() {
            Ok(None) => {
                tx.wake_after(Duration::from_millis(250));
                return false;
            }
            Ok(Some(status)) if status.success() => Self::Idle,
            Ok(Some(status)) => Self::Failed(format!("XXMI exited with {status}.")),
            Err(error) => Self::Failed(format!("Could not check the XXMI process: {error}")),
        };
        true
    }
}

pub(super) struct XxmiGameStart {
    pub(super) kind: GameKind,
    launcher: Option<Child>,
    exited_at: Option<Instant>,
    pub(super) seen: Option<Instant>,
}

impl XxmiGameStart {
    pub(super) fn new(kind: GameKind, launcher: Child) -> Self {
        Self {
            kind,
            launcher: Some(launcher),
            exited_at: None,
            seen: None,
        }
    }

    pub(super) fn poll(&mut self, running: bool, now: Instant) -> Option<Option<String>> {
        if running {
            self.seen.get_or_insert(now);
        }
        if let Some(child) = &mut self.launcher {
            match child.try_wait() {
                Ok(None) => return None,
                Ok(Some(status)) => {
                    self.launcher = None;
                    self.exited_at = Some(now);
                    if !status.success() && self.seen.is_none() {
                        return Some(Some(format!("XXMI exited with {status}. Check XXMI Launcher Log.txt.")));
                    }
                }
                Err(error) => return Some(Some(format!("Could not check the XXMI process: {error}"))),
            }
        }
        if let Some(seen) = self.seen {
            if now.duration_since(seen) >= Duration::from_secs(15) {
                return Some(None);
            }
            if !running {
                return Some(Some(format!("{} exited shortly after starting. Check the game log.", self.kind.display_name())));
            }
        } else if self.exited_at.is_some_and(|exit| now.duration_since(exit) >= Duration::from_secs(5)) {
            return Some(Some(format!("{} did not stay running after launch. Check the game and XXMI logs.", self.kind.display_name())));
        }
        None
    }
}

impl State {
    pub(super) fn game_is_starting(&self) -> bool {
        self.xxmi_game_start.as_ref().is_some_and(|start| {
            start.kind == self.game_kind() && start.seen.is_none()
        })
    }

    pub(super) fn poll_games(&mut self, tx: &mut Tx) -> bool {
        if !cfg!(target_os = "linux") || (!self.game_kind().is_xxmi() && self.xxmi_game_start.is_none()) {
            self.next_game_check = None;
            return false;
        }
        let now = Instant::now();
        if let Some(next) = self.next_game_check.filter(|next| *next > now) {
            tx.wake_after(next.duration_since(now));
            return false;
        }
        let running = running_games();
        let mut changed = self.running_games != running;
        self.running_games = running;
        if let Some(start) = &mut self.xxmi_game_start
            && let Some(notice) = start.poll(self.running_games.contains(start.kind), now)
        {
            self.xxmi_game_start = None;
            if let Some(notice) = notice {
                self.notice = notice;
            }
            changed = true;
        }
        let interval = Duration::from_secs(1);
        self.next_game_check = Some(now + interval);
        tx.wake_after(interval);
        changed
    }

    pub(super) fn open_xxmi(&mut self) {
        let root = self
            .snapshot
            .selected
            .as_ref()
            .map(|instance| instance.root.clone());
        let LaunchSettings { xxmi, wine_prefix } = self.snapshot.launch.clone();
        self.start_xxmi(move || {
            let (command, exe) = launch_xxmi(root.as_deref(), xxmi.as_deref(), wine_prefix.as_deref())?;
            Ok((command.spawn_process()?, exe))
        });
    }

    pub(super) fn start_xxmi(&mut self, start: impl FnOnce() -> Result<(Child, PathBuf)> + Send + 'static) {
        if self.busy.is_some() || self.xxmi_launch.active() {
            return;
        }
        self.submit("Starting XXMI…", move |_| {
            let (child, exe) = start()?;
            Ok(Update::XxmiStarted(child, exe))
        });
        self.xxmi_launch = if self.busy.is_some() {
            XxmiLaunch::Starting
        } else {
            XxmiLaunch::Failed(self.notice.clone())
        };
    }

    pub(super) fn close_xxmi(&mut self) {
        let XxmiLaunch::Running(child, exe) = &self.xxmi_launch else {
            return;
        };
        let pid = child.id();
        let exe = exe.clone();
        self.submit("Closing XXMI…", move |_| {
            launcher_window::close(pid, &exe)?;
            Ok(Update::Message("XXMI close requested".into()))
        });
    }

    #[cfg(target_os = "linux")]
    pub(super) fn install_steam_xxmi(&mut self, mut target: SteamXxmi) {
        let Some(runner) = self.protontricks.clone() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let (updates, progress) = mpsc::channel();
        self.submit(
            "Setting up XXMI and Visual C++ in Steam Proton…",
            move |_| {
                let result = target
                    .install(&runner, |stage| {
                        let _ = updates.send(stage);
                    })
                    .map(|()| {
                        target.installed = true;
                        target
                    });
                Ok(Update::SteamXxmiInstalled(result))
            },
        );
        self.setup.steam_dialog = Some(if self.busy.is_some() {
            SteamDialog::Running {
                stage: InstallStage::Preparing,
                progress,
            }
        } else {
            SteamDialog::Complete(Err(self.notice.clone()))
        });
    }

    #[cfg(target_os = "linux")]
    pub(super) fn open_steam_xxmi(&mut self, target: SteamXxmi) {
        let Some(runner) = self.protontricks.clone() else {
            return;
        };
        self.start_xxmi(move || target.start_launcher(&runner));
    }
}

