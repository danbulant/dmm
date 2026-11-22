use std::collections::hash_map::DefaultHasher;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use rune::tok::Theme;
use rune::{El, col, row};
use rune_kit::{Dialog, Kit};

use super::components::{button, heading, muted, panel};
use super::{State, Tx};

const DESKTOP_ID: &str = "com.dmm.ModManager-nxm.desktop";
const MAX_URI_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS_PER_POLL: usize = 8;

#[derive(Default)]
pub(super) struct HandlerDialog {
    previous: Option<String>,
    result: Option<std::result::Result<String, String>>,
}

pub(super) struct Listener {
    listener: UnixListener,
    path: PathBuf,
}

impl Listener {
    pub(super) fn bind(state_dir: &Path) -> Result<Self> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .context(
                "XDG_RUNTIME_DIR is not set; Nexus link handling needs a private runtime directory",
            )?;
        let metadata = fs::metadata(&runtime).context("reading XDG_RUNTIME_DIR")?;
        let self_metadata = fs::metadata("/proc/self").context("checking current user")?;
        ensure!(metadata.is_dir(), "XDG_RUNTIME_DIR is not a directory");
        ensure!(
            metadata.uid() == self_metadata.uid(),
            "XDG_RUNTIME_DIR is not owned by this user"
        );
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "XDG_RUNTIME_DIR must not be accessible by other users"
        );
        let state_dir = state_dir
            .canonicalize()
            .context("resolving state directory for Nexus listener")?;
        let path = socket_path(&runtime, &state_dir);
        if let Ok(previous) = fs::symlink_metadata(&path) {
            ensure!(
                previous.file_type().is_socket() && previous.uid() == metadata.uid(),
                "Unexpected file at the Nexus socket path"
            );
            match UnixStream::connect(&path) {
                Ok(_) => anyhow::bail!("The Nexus link listener is already running"),
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    fs::remove_file(&path).context("removing an inactive Nexus link socket")?;
                }
                Err(error) => return Err(error).context("checking previous Nexus link socket"),
            }
        }
        let listener = UnixListener::bind(&path)
            .with_context(|| format!("binding Nexus link listener in {}", runtime.display()))?;
        if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(&path);
            return Err(error).context("protecting Nexus link listener socket");
        }
        if let Err(error) = listener.set_nonblocking(true) {
            let _ = fs::remove_file(&path);
            return Err(error).context("configuring Nexus link listener");
        }
        Ok(Self { listener, path })
    }

    pub(super) fn poll(&self, mut accept: impl FnMut(String) -> bool) -> bool {
        let mut changed = false;
        for _ in 0..MAX_CONNECTIONS_PER_POLL {
            let (mut stream, _) = match self.listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            };
            let uri = receive_uri(&mut stream);
            changed |= uri.is_some();
            let accepted = uri.is_some_and(&mut accept);
            let _ = stream.write_all(if accepted { b"ok\n" } else { b"error\n" });
        }
        changed
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn socket_path(runtime: &Path, state_dir: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    state_dir.hash(&mut hasher);
    runtime.join(format!("dmm-nxm-{:016x}.sock", hasher.finish()))
}

fn receive_uri(stream: &mut UnixStream) -> Option<String> {
    let deadline = Instant::now() + Duration::from_millis(40);
    let mut bytes = Vec::with_capacity(256);
    let mut chunk = [0; 512];
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        stream.set_read_timeout(Some(remaining)).ok()?;
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        let newline = chunk[..count].iter().position(|&byte| byte == b'\n');
        let payload = &chunk[..newline.unwrap_or(count)];
        if bytes.len() + payload.len() > MAX_URI_BYTES {
            return None;
        }
        bytes.extend_from_slice(payload);
        if newline.is_some() {
            break;
        }
    }
    let uri = String::from_utf8(bytes).ok()?;
    uri.starts_with("nxm://").then_some(uri)
}

pub(crate) fn forward_from_cli(state_dir: &Path, uri: &str) -> Result<()> {
    ensure!(uri.starts_with("nxm://"), "invalid Nexus link");
    ensure!(uri.len() <= MAX_URI_BYTES, "Nexus link is too long");
    ensure!(!uri.chars().any(char::is_control), "invalid Nexus link");
    let state_dir = state_dir
        .canonicalize()
        .context("resolving state directory")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .context("XDG_RUNTIME_DIR is not set; start DMM before opening Nexus links")?;
    let mut stream = UnixStream::connect(socket_path(&runtime, &state_dir))
        .context("DMM is not running with Nexus downloads available; start DMM and open the Nexus link again")?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.write_all(uri.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut response = [0; 3];
    stream.read_exact(&mut response)?;
    ensure!(
        &response == b"ok\n",
        "DMM could not accept the Nexus link; check the Downloads screen"
    );
    Ok(())
}

pub(super) fn setup_panel(s: &State, t: Theme) -> El {
    panel(t)
        .child(heading("Nexus links", t))
        .child(muted("Optionally make nxm:// links open in the running DMM Downloads queue. Registration is per-user and can replace your current handler only after confirmation. If no GUI is running, DMM asks you to start it instead of mutating the library in a second process.", t).wrap())
        .child(button(
            "setup:nexus-handler",
            "Register Nexus link handler…",
            t,
            false,
            s.nexus_listener.is_some(),
            |s, _| {
                let previous = current_default_handler().ok().flatten();
                s.setup.nexus_handler_dialog = Some(HandlerDialog { previous, result: None });
            },
        ))
}

pub(super) fn overlay(s: &State, t: Theme, k: &Kit, layer: &mut Vec<El>) {
    let Some(dialog) = &s.setup.nexus_handler_dialog else {
        return;
    };
    let (title, body) = if let Some(result) = &dialog.result {
        let (message, success) = match result {
            Ok(message) => (message.as_str(), true),
            Err(error) => (error.as_str(), false),
        };
        let mut content = col().gap(12.0).child(muted(message, t).wrap());
        if success {
            content = content.child(muted("Nexus links will be delivered only while this DMM session is running. Close this dialog to finish.", t).wrap());
        }
        (
            if success {
                "Nexus handler registered"
            } else {
                "Nexus handler registration failed"
            },
            content.child(row().justify_end().child(button(
                "nexus-handler-close",
                "Close",
                t,
                false,
                true,
                close_dialog,
            ))),
        )
    } else {
        let prior = dialog
            .previous
            .as_deref()
            .unwrap_or("No current default handler was reported.");
        let content = col().gap(12.0)
            .child(muted("DMM will add a user-level desktop entry and set it as the nxm:// handler with xdg-mime. This does not modify system files. Opening a link requires DMM to be running; otherwise the handler reports that DMM must be started. Nexus links contain temporary authorization data and are sent over a private local socket, never saved to disk.", t).wrap())
            .child(muted(format!("Current handler: {prior}"), t).wrap())
            .child(row().gap(8.0).flex_wrap().justify_end()
                .child(button("nexus-handler-cancel", "Cancel", t, false, true, close_dialog))
                .child(button("nexus-handler-confirm", "Register", t, true, true, |s, _| register_for_state(s))));
        ("Register Nexus link handler?", content)
    };
    Dialog::new("nexus-handler", title)
        .open(true)
        .fit()
        .width(500.0)
        .click_outside(false)
        .on_close(|s: &mut State| s.setup.nexus_handler_dialog = None)
        .child(body.px(16.0).pb(16.0))
        .build(k, layer);
}

fn close_dialog(s: &mut State, _: &mut Tx) {
    s.setup.nexus_handler_dialog = None;
}

fn register_for_state(s: &mut State) {
    let outcome = (|| -> Result<String> {
        let executable = std::env::current_exe().context("resolving DMM executable path")?;
        let executable = handler_executable(
            executable,
            std::env::var_os("APPDIR").map(PathBuf::from),
            std::env::var_os("APPIMAGE").map(PathBuf::from),
        );
        register(&executable, &s.state_dir)
    })()
    .map_err(|error| format!("{error:#}"));
    if let Some(dialog) = &mut s.setup.nexus_handler_dialog {
        dialog.result = Some(outcome);
    }
}

fn handler_executable(
    executable: PathBuf,
    appdir: Option<PathBuf>,
    appimage: Option<PathBuf>,
) -> PathBuf {
    if let (Some(appdir), Some(appimage)) = (appdir, appimage)
        && appdir.is_absolute()
        && appimage.is_absolute()
        && executable.starts_with(appdir)
    {
        appimage
    } else {
        executable
    }
}

pub(super) fn register(executable: &Path, state_dir: &Path) -> Result<String> {
    let executable = executable
        .canonicalize()
        .context("resolving DMM executable path")?;
    let state_dir = state_dir
        .canonicalize()
        .context("resolving DMM state directory")?;
    let exec = format!(
        "{} --state {} accept-nxm --from-handler %u",
        exec_quote(&executable)?,
        exec_quote(&state_dir)?
    );
    let entry = desktop_entry(&exec);
    let applications = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .context("HOME or XDG_DATA_HOME is not set")?
        .join("applications");
    fs::create_dir_all(&applications).context("creating per-user applications directory")?;
    let desktop_path = applications.join(DESKTOP_ID);
    ensure!(
        !desktop_path.exists(),
        "DMM's Nexus desktop entry already exists; remove it before registering again"
    );

    let previous = query_default_handler()?;
    let mut temp = desktop_path.clone();
    temp.set_extension(format!("desktop.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .context("creating temporary Nexus desktop entry")?;
    let mut installed_entry = false;
    let result = (|| -> Result<()> {
        file.write_all(entry.as_bytes())
            .context("writing Nexus desktop entry")?;
        file.sync_all().context("syncing Nexus desktop entry")?;
        fs::hard_link(&temp, &desktop_path)
            .context("installing Nexus desktop entry without replacing an existing entry")?;
        installed_entry = true;
        fs::remove_file(&temp).context("removing temporary Nexus desktop entry")?;
        let status = Command::new("xdg-mime")
            .args(["default", DESKTOP_ID, "x-scheme-handler/nxm"])
            .status()
            .context("running xdg-mime to register the Nexus link handler")?;
        ensure!(
            status.success(),
            "xdg-mime could not register the Nexus link handler"
        );
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        if installed_entry {
            let _ = fs::remove_file(&desktop_path);
        }
        if let Some(previous) = previous.as_deref() {
            let _ = Command::new("xdg-mime")
                .args(["default", previous, "x-scheme-handler/nxm"])
                .status();
        }
        return Err(error);
    }
    Ok(previous.map_or_else(
        || "No previous Nexus handler was registered.".into(),
        |handler| format!("Previous handler: {handler}. Restore it with: xdg-mime default {handler} x-scheme-handler/nxm"),
    ))
}

pub(super) fn current_default_handler() -> Result<Option<String>> {
    query_default_handler()
}

fn query_default_handler() -> Result<Option<String>> {
    let output = Command::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/nxm"])
        .output()
        .context("running xdg-mime to inspect the current Nexus handler")?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok((!value.is_empty()).then_some(value))
}

fn exec_quote(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .context("Nexus handler paths must be valid UTF-8")?;
    ensure!(
        !text.chars().any(char::is_control),
        "Nexus handler paths cannot contain control characters"
    );
    let mut output = String::from("\"");
    for ch in text.chars() {
        if ch == '\\' {
            output.push_str(r"\\\\");
            continue;
        } else if matches!(ch, '"' | '`' | '$') {
            output.push_str(r"\\");
        }
        if ch == '%' {
            output.push('%');
        }
        output.push(ch);
    }
    output.push('\"');
    Ok(output)
}

fn desktop_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=DMM Nexus link handler\nNoDisplay=true\nMimeType=x-scheme-handler/nxm;\nTerminal=false\nExec={exec}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_entry_uses_absolute_paths_and_uri_field_code() {
        let executable = exec_quote(Path::new("/opt/DMM app/dmm")).unwrap();
        let state = exec_quote(Path::new("/home/user/.local/state/dmm")).unwrap();
        let entry = desktop_entry(&format!(
            "{executable} --state {state} accept-nxm --from-handler %u"
        ));
        assert!(entry.contains("MimeType=x-scheme-handler/nxm;"));
        assert!(entry.contains("Exec=\"/opt/DMM app/dmm\" --state \"/home/user/.local/state/dmm\" accept-nxm --from-handler %u"));
    }

    #[test]
    fn handler_uses_the_appimage_only_for_its_bundled_executable() {
        let appdir = "/tmp/.mount_dmm";
        let appimage = "/opt/DMM app/dmm.AppImage";
        for (executable, expected) in [
            ("/tmp/.mount_dmm/usr/bin/dmm", appimage),
            ("/usr/bin/dmm", "/usr/bin/dmm"),
        ] {
            let resolved = handler_executable(
                executable.into(),
                Some(appdir.into()),
                Some(appimage.into()),
            );
            assert_eq!(resolved, Path::new(expected));
        }
    }

    #[test]
    fn exec_quoting_escapes_desktop_metacharacters() {
        assert_eq!(
            exec_quote(Path::new("/tmp/a\\b\"c$d`e")).unwrap(),
            r#""/tmp/a\\\\b\\"c\\$d\\`e""#
        );
        assert!(exec_quote(Path::new("/tmp/a\nb")).is_err());
    }

    #[test]
    fn routing_accepts_only_bounded_nxm_uri_lines() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender
            .write_all(b"nxm://game/mod/file?key=secret\n")
            .unwrap();
        assert_eq!(
            receive_uri(&mut receiver).as_deref(),
            Some("nxm://game/mod/file?key=secret")
        );

        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.write_all(b"https://example.com\n").unwrap();
        assert!(receive_uri(&mut receiver).is_none());
    }
}
