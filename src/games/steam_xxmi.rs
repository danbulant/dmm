use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use ring::digest::{Context as Digest, SHA256};
use serde::Deserialize;
use tempfile::Builder;
use url::Url;

use super::LaunchCommand;
use crate::archive;

const APP_ID: u32 = 4_162_040;
const FLATPAK_ID: &str = "com.github.Matoking.protontricks";
const RELEASE: &str = "https://api.github.com/repos/SpectrumQT/XXMI-Launcher/releases/latest";
const RELEASE_DOWNLOAD: &str = "https://github.com/SpectrumQT/XXMI-Launcher/releases/download/";
const VC_REDIST: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";
const MIB: u64 = 1024 * 1024;
pub(super) const PORTABLE_DIR: &str = "dmm-xxmi";
const PORTABLE_EXE: &str = "Resources/Bin/XXMI Launcher.exe";

#[derive(Clone)]
pub struct SteamXxmi {
    pub game: PathBuf,
    pub compatdata: PathBuf,
    pub initialized: bool,
    pub installed: bool,
    steam: PathBuf,
}

#[derive(Clone, Copy)]
pub enum InstallStage {
    Preparing,
    DownloadingLauncher,
    UnpackingLauncher,
    DownloadingRuntime,
    InstallingRuntime,
    SavingLauncher,
}

#[derive(Clone)]
pub enum Protontricks {
    Native(PathBuf),
    Flatpak(PathBuf),
}

impl Protontricks {
    pub fn find() -> Option<Self> {
        if let Some(native) = executable("protontricks-launch") {
            return Some(Self::Native(native));
        }
        let flatpak = executable("flatpak")?;
        Command::new(&flatpak)
            .args(["info", FLATPAK_ID])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()
            .filter(|status| status.success())?;
        Some(Self::Flatpak(flatpak))
    }
}

fn executable(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| {
            fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .and_then(|path| path.canonicalize().ok())
}

pub(super) fn compatdata_dir(library: &Path) -> PathBuf {
    library
        .join("steamapps/compatdata")
        .join(APP_ID.to_string())
}

pub fn discover() -> Result<Vec<SteamXxmi>> {
    let mut installs = Vec::new();
    if !cfg!(target_arch = "x86_64") {
        return Ok(installs);
    }
    for steam in steamlocate::locate_all().context("could not locate Steam")? {
        let steam_root = steam.path().canonicalize()?;
        for library in steam
            .libraries()
            .context("could not inspect Steam libraries")?
            .filter_map(Result::ok)
        {
            let Some(Ok(app)) = library.app(APP_ID) else {
                continue;
            };
            let Ok(game) = library.resolve_app_dir(&app).canonicalize() else {
                continue;
            };
            if !game.is_dir() {
                continue;
            }
            let path = compatdata_dir(library.path());
            let compatdata = path.canonicalize().unwrap_or(path);
            let initialized = compatdata.join("pfx/drive_c/windows").is_dir();
            let installed =
                initialized && compatdata.join(PORTABLE_DIR).join(PORTABLE_EXE).is_file();
            installs.push(SteamXxmi {
                game,
                compatdata,
                initialized,
                installed,
                steam: steam_root.clone(),
            });
        }
    }
    installs.sort_unstable_by(|a, b| (&a.game, &a.compatdata).cmp(&(&b.game, &b.compatdata)));
    installs.dedup_by(|a, b| a.compatdata == b.compatdata);
    Ok(installs)
}

impl SteamXxmi {
    fn executable(&self) -> PathBuf {
        self.compatdata.join(PORTABLE_DIR).join(PORTABLE_EXE)
    }

    fn command(
        &self,
        runner: &Protontricks,
        exe: &Path,
        args: impl IntoIterator<Item = OsString>,
    ) -> LaunchCommand {
        let env = vec![
            (
                OsString::from("STEAM_DIR"),
                self.steam.clone().into_os_string(),
            ),
            (
                OsString::from("STEAM_COMPAT_DATA_PATH"),
                self.compatdata.clone().into_os_string(),
            ),
        ];
        let mut command = LaunchCommand {
            program: OsString::new(),
            args: Vec::new(),
            cwd: exe.parent().map(Path::to_owned),
            env,
            url: false,
        };
        match runner {
            Protontricks::Native(path) => command.program = path.as_os_str().to_owned(),
            Protontricks::Flatpak(path) => {
                command.program = path.as_os_str().to_owned();
                command
                    .args
                    .extend(["run".into(), "--command=protontricks-launch".into()]);
                for (key, value) in &command.env {
                    let mut flag = OsString::from("--env=");
                    flag.push(key);
                    flag.push("=");
                    flag.push(value);
                    command.args.push(flag);
                }
                command.args.push(FLATPAK_ID.into());
            }
        }
        command.args.extend([
            "--appid".into(),
            APP_ID.to_string().into(),
            exe.as_os_str().to_owned(),
        ]);
        command.args.extend(args);
        command
    }

    pub fn start_launcher(&self, runner: &Protontricks) -> Result<()> {
        let exe = self.executable();
        ensure!(exe.is_file(), "The Steam XXMI launcher is not installed");
        self.command(runner, &exe, []).spawn()
    }

    pub fn install(
        &self,
        runner: &Protontricks,
        mut progress: impl FnMut(InstallStage),
    ) -> Result<()> {
        progress(InstallStage::Preparing);
        ensure!(
            self.compatdata.join("pfx/drive_c/windows").is_dir(),
            "Launch Zenless Zone Zero once in Steam with Proton, close it, then rescan"
        );
        let _lock = install_lock(&self.compatdata)?;
        if self.executable().is_file() {
            return Ok(());
        }
        let dest = self.compatdata.join(PORTABLE_DIR);
        match dest.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::bail!(
                "{} already exists without a launcher; it was not overwritten",
                dest.display()
            ),
        }
        let scratch = Builder::new()
            .prefix(".dmm-xxmi-")
            .tempdir_in(&self.compatdata)?;
        let client = download_client()?;
        let portable = scratch.path().join("xxmi.zip");
        progress(InstallStage::DownloadingLauncher);
        download_portable(&client, &portable)?;
        progress(InstallStage::UnpackingLauncher);
        let tree = archive::unpack(&portable, scratch.path())
            .context("unpacking the portable XXMI launcher")?;
        ensure!(
            tree.root().join(PORTABLE_EXE).is_file(),
            "The official archive has no portable launcher at {PORTABLE_EXE}"
        );
        let redist = scratch.path().join("VC_redist.x64.exe");
        progress(InstallStage::DownloadingRuntime);
        download_redist(&client, &redist)?;
        progress(InstallStage::InstallingRuntime);
        let spec = self.command(
            runner,
            &redist,
            ["/install".into(), "/quiet".into(), "/norestart".into()],
        );
        let status = spec
            .process()
            .status()
            .context("running Microsoft's Visual C++ installer through Protontricks")?;
        // Windows codes 1638 (newer version installed) and 3010 (success; restart requested)
        // can only reach us modulo 256 on Linux. No machine restart is requested.
        ensure!(
            matches!(status.code(), Some(0 | 102 | 194)),
            "Visual C++ setup reported {status}; the XXMI launcher was not installed"
        );
        progress(InstallStage::SavingLauncher);
        fs::rename(tree.root(), &dest)
            .with_context(|| format!("installing portable launcher to {}", dest.display()))?;
        Ok(())
    }
}

fn install_lock(compatdata: &Path) -> Result<File> {
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(compatdata.join(".dmm-xxmi.lock"))?;
    lock.try_lock()
        .context("another DMM XXMI setup is using this prefix")?;
    Ok(lock)
}

pub(super) fn managed_launch(exe: &Path, args: &[OsString]) -> Result<Option<LaunchCommand>> {
    if !exe.ends_with(Path::new(PORTABLE_DIR).join(PORTABLE_EXE)) {
        return Ok(None);
    }
    let target = discover()?
        .into_iter()
        .find(|target| target.executable() == exe)
        .context(
            "Steam could not locate the installed XXMI launcher's game prefix; rescan Game setup",
        )?;
    let runner =
        Protontricks::find().context("Install Protontricks to run the Steam XXMI launcher")?;
    Ok(Some(target.command(&runner, exe, args.iter().cloned())))
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    size: u64,
    digest: Option<String>,
}

fn download_portable(client: &Client, path: &Path) -> Result<()> {
    let release = client.get(RELEASE).send()?.error_for_status()?;
    let Release { tag_name, assets } = serde_json::from_reader(release.take(MIB))?;
    ensure!(
        tag_name.starts_with('v')
            && tag_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')),
        "unexpected XXMI version tag"
    );
    let name = format!("XXMI-Launcher-Portable-{tag_name}.zip");
    let asset = assets
        .into_iter()
        .find(|asset| asset.name == name)
        .context("the official XXMI release has no portable ZIP")?;
    ensure!(
        (1..=256 * MIB).contains(&asset.size),
        "unexpected XXMI portable ZIP size"
    );
    let digest = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .context("the official XXMI asset has no SHA-256 digest")?;
    let response = client
        .get(format!("{RELEASE_DOWNLOAD}{tag_name}/{name}"))
        .send()?
        .error_for_status()?;
    verified_download(response, path, digest, asset.size, Some(asset.size))
        .context("downloading the official portable XXMI release")
}

fn download_redist(client: &Client, path: &Path) -> Result<()> {
    let response = client.get(VC_REDIST).send()?.error_for_status()?;
    let digest = redist_digest(response.url())
        .context("Microsoft's download did not provide the expected SHA-256 URL")?
        .to_owned();
    verified_download(response, path, &digest, 64 * MIB, None)
        .context("downloading the official Visual C++ x64 Redistributable")
}

fn redist_digest(url: &Url) -> Option<&str> {
    if url.host_str() != Some("download.visualstudio.microsoft.com") {
        return None;
    }
    let mut path = url.path_segments()?;
    if !path.next_back()?.eq_ignore_ascii_case("VC_redist.x64.exe") {
        return None;
    }
    path.next_back().filter(|part| is_sha256(part))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn verified_download(
    mut response: impl Read,
    path: &Path,
    expected: &str,
    limit: u64,
    exact: Option<u64>,
) -> Result<()> {
    ensure!(is_sha256(expected), "invalid expected SHA-256 digest");
    let mut file = File::create_new(path)?;
    let mut digest = Digest::new(&SHA256);
    let mut bytes = 0;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        ensure!(bytes <= limit, "download exceeds its size limit");
        let chunk = &buffer[..count];
        digest.update(chunk);
        file.write_all(chunk)?;
    }
    ensure!(
        bytes != 0 && exact.is_none_or(|size| size == bytes),
        "unexpected download size"
    );
    let hash: String = digest
        .finish()
        .as_ref()
        .iter()
        .flat_map(|&byte| {
            const HEX: &[u8] = b"0123456789abcdef";
            [
                HEX[(byte >> 4) as usize] as char,
                HEX[(byte & 15) as usize] as char,
            ]
        })
        .collect();
    ensure!(
        hash.eq_ignore_ascii_case(expected),
        "download did not match its publisher's SHA-256 digest"
    );
    Ok(())
}

fn download_client() -> Result<Client> {
    fn trusted(url: &Url) -> bool {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && matches!(
                url.host_str(),
                Some(
                    "api.github.com"
                        | "github.com"
                        | "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                        | "aka.ms"
                        | "download.visualstudio.microsoft.com"
                )
            )
    }
    Ok(Client::builder()
        .user_agent(concat!("dmm/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .timeout(Duration::from_secs(300))
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && trusted(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("unexpected installer download redirect")
            }
        }))
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn steam_runner_uses_the_exact_game_prefix_and_passes_paths_as_arguments() {
        let target = SteamXxmi {
            game: "/steam/common/Zenless Zone Zero".into(),
            compatdata: "/steam library/compatdata/4162040".into(),
            steam: "/steam".into(),
            initialized: true,
            installed: true,
        };
        let spec = target.command(
            &Protontricks::Native("/bin/protontricks-launch".into()),
            &target.executable(),
            ["--nogui".into(), "--xxmi".into(), "ZZMI".into()],
        );
        let process = spec.process();
        let args = process.get_args().collect::<Vec<_>>();
        let exe = target.executable();
        assert_eq!(
            args,
            [
                OsStr::new("--appid"),
                OsStr::new("4162040"),
                exe.as_os_str(),
                OsStr::new("--nogui"),
                OsStr::new("--xxmi"),
                OsStr::new("ZZMI")
            ]
        );
        assert!(
            process
                .get_envs()
                .any(|(key, value)| key == "STEAM_COMPAT_DATA_PATH"
                    && value == Some(target.compatdata.as_os_str()))
        );
    }

    #[test]
    fn redist_checksum_is_accepted_only_from_the_verified_vendor_path() {
        let digest = "CC0FF0EB1DC3F5188AE6300FAEF32BF5BEEBA4BDD6E8E445A9184072096B713B";
        let path = format!("/download/pr/uuid/{digest}/VC_redist.x64.exe");
        assert_eq!(
            redist_digest(
                &Url::parse(&format!(
                    "https://download.visualstudio.microsoft.com{path}"
                ))
                .unwrap()
            ),
            Some(digest)
        );
        assert_eq!(
            redist_digest(&Url::parse(&format!("https://other.example{path}")).unwrap()),
            None
        );
    }

    #[test]
    fn downloads_reject_changed_and_oversized_bytes_before_use() {
        let scratch = tempfile::tempdir().unwrap();
        let sha_of_abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let pass = scratch.path().join("verified");
        verified_download(&b"abc"[..], &pass, sha_of_abc, 3, Some(3)).unwrap();
        assert_eq!(fs::read(pass).unwrap(), b"abc");
        assert!(
            verified_download(
                &b"abd"[..],
                &scratch.path().join("changed"),
                sha_of_abc,
                3,
                Some(3)
            )
            .is_err()
        );
        assert!(
            verified_download(
                &b"abcd"[..],
                &scratch.path().join("oversized"),
                sha_of_abc,
                3,
                None
            )
            .is_err()
        );
    }
}
