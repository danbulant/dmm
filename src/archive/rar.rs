use std::ffi::CStr;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, ensure};

#[cfg(unix)]
use super::ArchiveLocale;

pub(super) fn is_rar(path: &Path) -> anyhow::Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut signature = [0_u8; 8];
    let count = file.read(&mut signature)?;
    let signature = &signature[..count];
    Ok(
        signature.starts_with(b"Rar!\x1a\x07\x00")
            || signature.starts_with(b"Rar!\x1a\x07\x01\x00"),
    )
}

use super::{ArchiveReader, ExtractState, NodeKind, account_and_write, archive_portable_path};

struct Entry {
    name: String,
    portable: String,
    is_dir: bool,
    size: Option<u64>,
}

pub(super) fn extract(input: &Path, state: &mut ExtractState<'_>) -> anyhow::Result<()> {
    let entries = inspect(input, state)?;
    for entry in entries {
        if entry.is_dir {
            state.add_directory(&entry.portable)?;
            continue;
        }
        state.begin_file()?;
        let destination = state.register(&entry.portable, NodeKind::File)?;
        let parent = destination
            .parent()
            .context("extracted RAR file has no parent")?;
        fs::create_dir_all(parent)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .with_context(|| format!("cannot create extracted file {}", destination.display()))?;
        let mut child = Command::new("unrar")
            .arg("p")
            .arg("-cfg-")
            .arg("-inul")
            .arg("-p-")
            .arg("-c-")
            .arg("-@")
            .arg("--")
            .arg(input)
            .arg(&entry.name)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("RAR fallback requires the unrar executable")?;
        let mut stdout = child
            .stdout
            .take()
            .context("unrar did not provide an output stream")?;
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        let copy_result = (|| -> anyhow::Result<()> {
            loop {
                let count = stdout.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                total = total
                    .checked_add(count as u64)
                    .context("RAR entry size overflow")?;
                ensure!(
                    entry.size.is_none_or(|size| total <= size),
                    "unrar output exceeds the declared RAR entry size"
                );
                account_and_write(state, &mut output, &buffer[..count])?;
            }
            if let Some(size) = entry.size {
                ensure!(
                    total == size,
                    "unrar output does not match the declared RAR entry size"
                );
            }
            Ok(())
        })();
        if copy_result.is_err() {
            drop(stdout);
            let _ = child.kill();
        }
        let status = child.wait().context("cannot wait for unrar")?;
        copy_result?;
        ensure!(
            status.success(),
            "unrar failed while verifying RAR entry {:?}; download the archive again and retry, or manually repackage it only after it passes `unrar t`",
            entry.name
        );
        output.flush()?;
        state.files.push(PathBuf::from(entry.portable));
    }
    Ok(())
}

fn inspect(input: &Path, state: &ExtractState<'_>) -> anyhow::Result<Vec<Entry>> {
    #[cfg(unix)]
    let _locale = ArchiveLocale::utf8();
    let mut archive = ArchiveReader::open(input)
        .with_context(|| format!("cannot inspect RAR metadata in {}", input.display()))?;
    let mut validation = ExtractState::new(
        state.root,
        state.max_files,
        state.max_entries,
        state.max_bytes,
    );
    let mut entries = Vec::new();
    let mut declared_bytes = 0_u64;
    let mut file_count = 0_usize;
    while let Some(entry) = archive.next_entry()? {
        let entry = entry.as_ptr();
        let pathname = unsafe { super::archive_entry_pathname(entry) };
        ensure!(!pathname.is_null(), "RAR entry has no pathname");
        let name = unsafe { CStr::from_ptr(pathname) }
            .to_str()
            .context("RAR entry pathname is not UTF-8")?
            .to_owned();
        ensure!(
            unsafe { super::archive_entry_symlink(entry) }.is_null(),
            "symbolic links are not allowed in RAR archives"
        );
        ensure!(
            unsafe { super::archive_entry_hardlink(entry) }.is_null(),
            "hard links are not allowed in RAR archives"
        );
        let mode = unsafe { super::archive_entry_strmode(entry) };
        ensure!(!mode.is_null(), "missing RAR entry type");
        let file_type = unsafe { *mode } as u8;
        let is_dir = file_type == b'd';
        ensure!(
            is_dir || file_type == b'-',
            "RAR contains an unsupported special entry: {name:?}"
        );
        let portable =
            archive_portable_path(&name).with_context(|| format!("unsafe RAR path {name:?}"))?;
        ensure!(
            !name.chars().any(|c| matches!(c, '*' | '?' | '[' | ']')) && !name.starts_with('@'),
            "RAR entry name cannot be selected literally by unrar: {name:?}"
        );
        if is_dir {
            validation.register(&portable, NodeKind::Directory)?;
            validation.record_entry()?;
        } else {
            file_count += 1;
            ensure!(
                file_count <= state.max_files,
                "archive exceeds the file-count limit"
            );
            validation.begin_file()?;
            validation.register(&portable, NodeKind::File)?;
        }
        let size = if unsafe { super::archive_entry_size_is_set(entry) } != 0 {
            let size = unsafe { super::archive_entry_size(entry) };
            ensure!(size >= 0, "RAR entry has a negative size");
            let size = u64::try_from(size)?;
            declared_bytes = declared_bytes
                .checked_add(size)
                .context("RAR declared size overflow")?;
            ensure!(
                declared_bytes <= state.max_bytes,
                "archive exceeds the uncompressed-size limit"
            );
            Some(size)
        } else {
            None
        };
        archive.skip_data()?;
        entries.push(Entry {
            name,
            portable,
            is_dir,
            size,
        });
    }
    ensure!(!entries.is_empty(), "RAR archive contains no entries");
    Ok(entries)
}
