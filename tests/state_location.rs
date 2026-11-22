use std::path::Path;
use std::process::Command;

fn cli(cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dmm"));
    command.current_dir(cwd);
    command
}

fn list_games(command: &mut Command) {
    let output = command.arg("games").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_default_library_uses_the_platform_state_root() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("platform-state");
    let state_env = if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "XDG_STATE_HOME"
    };
    list_games(cli(dir.path()).env(state_env, &state).env_remove("HOME"));
    assert!(state.join("dmm/library.sqlite3").is_file());
    assert!(!dir.path().join(".dmm").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn linux_uses_home_when_xdg_state_home_is_unset_or_relative() {
    for xdg in [None, Some("relative-state")] {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let mut command = cli(dir.path());
        command.env("HOME", &home).env_remove("XDG_STATE_HOME");
        if let Some(xdg) = xdg {
            command.env("XDG_STATE_HOME", xdg);
        }
        list_games(&mut command);
        assert!(home.join(".local/state/dmm/library.sqlite3").is_file());
        assert!(!dir.path().join(".dmm").exists());
        assert!(!dir.path().join("relative-state").exists());
    }
}

#[test]
fn an_explicit_relative_state_path_still_works_without_platform_state_roots() {
    let dir = tempfile::tempdir().unwrap();
    list_games(
        cli(dir.path())
            .env_remove("HOME")
            .env_remove("XDG_STATE_HOME")
            .env_remove("LOCALAPPDATA")
            .args(["--state", ".dmm"]),
    );
    assert!(dir.path().join(".dmm/library.sqlite3").is_file());
}
