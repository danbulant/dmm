use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use anyhow::Context;
use anyhow::Result;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "windows")]
use std::process::Command;
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{AtomEnum, ClientMessageEvent, ConnectionExt, EventMask};
#[cfg(target_os = "linux")]
use x11rb::{CURRENT_TIME, connection::Connection};

#[cfg(target_os = "linux")]
pub fn close(runner: u32, exe: &Path) -> Result<()> {
    let exe = exe.canonicalize()?;
    let started = process_start(runner).context("The XXMI launch process has exited")?;
    let (conn, screen) = x11rb::connect(None).context("Cannot connect to the XXMI X11 window")?;
    let atom = |name: &[u8]| -> Result<u32> { Ok(conn.intern_atom(false, name)?.reply()?.atom) };
    let clients = atom(b"_NET_CLIENT_LIST_STACKING")?;
    let window_pid = atom(b"_NET_WM_PID")?;
    let protocols = atom(b"WM_PROTOCOLS")?;
    let delete = atom(b"WM_DELETE_WINDOW")?;
    let root = conn.setup().roots[screen].root;
    let windows = conn.get_property(false, root, clients, AtomEnum::WINDOW, 0, 4096)?.reply()?;
    let windows = windows.value32().context("No X11 windows are available")?;
    let mut target = None;
    for window in windows {
        let Ok(property) = conn.get_property(false, window, window_pid, AtomEnum::CARDINAL, 0, 1)?.reply() else {
            continue;
        };
        let Some(pid) = property.value32().and_then(|mut values| values.next()) else {
            continue;
        };
        // Wine may reparent the Windows process. Match its mapped executable and launch age.
        if !process_start(pid).is_some_and(|time| time >= started) || !maps_executable(pid, &exe) {
            continue;
        }
        let supported = conn.get_property(false, window, protocols, AtomEnum::ATOM, 0, 64)?.reply()?;
        if supported.value32().is_some_and(|mut atoms| atoms.any(|value| value == delete)) {
            target = Some(window);
        }
    }
    let window = target.context(
        "No XXMI window to close yet. If it is open, close it from its own window.",
    )?;
    conn.send_event(
        false,
        window,
        EventMask::NO_EVENT,
        ClientMessageEvent::new(32, window, protocols, [delete, CURRENT_TIME, 0, 0, 0]),
    )?
    .check()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_start(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(target_os = "linux")]
fn maps_executable(pid: u32, exe: &Path) -> bool {
    let Ok(maps) = fs::read_to_string(format!("/proc/{pid}/maps")) else {
        return false;
    };
    maps.lines().any(|line| {
        line.find('/').is_some_and(|start| Path::new(&line[start..]) == exe)
    })
}

#[cfg(target_os = "windows")]
pub fn close(pid: u32, _: &Path) -> Result<()> {
    let status = Command::new("taskkill.exe").args(["/PID", &pid.to_string()]).status()
        .context("Could not request XXMI to close")?;
    anyhow::ensure!(status.success(), "XXMI did not accept the close request ({status})");
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn close(_: u32, _: &Path) -> Result<()> {
    anyhow::bail!("Close XXMI from its own window on this platform")
}
