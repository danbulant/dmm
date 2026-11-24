use crate::games::GameKind;
#[cfg(target_os = "linux")]
use crate::games::{endfield, zzz};

#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

#[derive(Default, PartialEq, Eq)]
pub struct RunningGames {
    zzmi: bool,
    efmi: bool,
}

impl RunningGames {
    pub fn contains(&self, kind: GameKind) -> bool {
        (kind == GameKind::Zzmi && self.zzmi) || (kind == GameKind::Efmi && self.efmi)
    }
}

#[cfg(target_os = "linux")]
pub fn running_games() -> RunningGames {
    scan(Path::new("/proc"))
}

#[cfg(not(target_os = "linux"))]
pub fn running_games() -> RunningGames {
    RunningGames::default()
}

#[cfg(target_os = "linux")]
fn scan(proc: &Path) -> RunningGames {
    let mut running = RunningGames::default();
    let Ok(entries) = fs::read_dir(proc) else { return running };
    let mut path = PathBuf::from(proc);
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.as_encoded_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        path.push(name);
        path.push("comm");
        if let Ok(comm) = fs::read_to_string(&path) {
            match comm.trim_end() {
                endfield::EXE => running.efmi = true,
                zzz::PROCESS_NAME if !running.zzmi => {
                    // Linux truncates Wine's executable name to 15 bytes in comm.
                    path.set_file_name("maps");
                    running.zzmi = fs::read_to_string(&path).is_ok_and(|maps| {
                        maps.lines().any(|line| {
                            zzz::PROCESS_IMAGES.iter().any(|exe| {
                                line.strip_suffix(*exe).is_some_and(|path| path.ends_with('/'))
                            })
                        })
                    });
                }
                _ => {}
            }
        }
        path.pop();
        path.pop();
        if running.zzmi && running.efmi {
            break;
        }
    }
    running
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn detects_wine_games_without_mistaking_the_launcher_for_a_game() -> std::io::Result<()> {
        let proc = tempfile::tempdir()?;
        for (pid, name) in [(10, "XXMI Launcher.e"), (20, "ZenlessZoneZero"), (30, "Endfield.exe")] {
            let process = proc.path().join(pid.to_string());
            fs::create_dir(&process)?;
            fs::write(process.join("comm"), format!("{name}\n"))?;
        }
        let running = scan(proc.path());
        assert!(!running.contains(GameKind::Zzmi));
        assert!(running.contains(GameKind::Efmi));
        fs::write(proc.path().join("20/maps"), "r--p /games/ZZZ/ZenlessZoneZero.exe\n")?;
        assert!(scan(proc.path()).contains(GameKind::Zzmi));
        Ok(())
    }
}
