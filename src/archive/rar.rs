use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Context, ensure};
use rars::{ArchiveReadOptions, ArchiveReader as RarReader};

use super::{ExtractState, NodeKind, archive_portable_path};

const MAX_RAR_HEADER_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RAR_READER_WORKSPACE: u64 = 256 * 1024 * 1024;
const MAX_RAR50_DICTIONARY: u64 = 256 * 1024 * 1024;

pub(super) fn is_rar(path: &Path) -> anyhow::Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut signature = [0_u8; 8];
    let count = file.read(&mut signature)?;
    let signature = &signature[..count];
    Ok(
        signature.starts_with(b"Rar!\x1a\x07\x00")
            || signature.starts_with(b"Rar!\x1a\x07\x01\x00")
            || signature.starts_with(b"RE~^"),
    )
}

struct Entry {
    portable: String,
    is_dir: bool,
}

pub(super) fn extract(input: &Path, state: &mut ExtractState<'_>) -> anyhow::Result<()> {
    let options = ArchiveReadOptions::new()
        .with_max_header_count(state.max_entries as u64)
        .with_max_header_bytes(MAX_RAR_HEADER_BYTES)
        .with_max_reader_workspace_bytes(MAX_RAR_READER_WORKSPACE)
        .with_rar50_dictionary_size_limit(MAX_RAR50_DICTIONARY)
        .with_max_member_output_bytes(state.max_bytes)
        .with_max_total_output_bytes(state.max_bytes)
        .with_threads(1);
    let archive = RarReader::read_reader_with_options(fs::File::open(input)?, options)
        .with_context(|| format!("cannot read RAR archive {}", input.display()))?;
    ensure!(!archive.is_volume(), "split RAR volumes are not supported");
    let entries = inspect(&archive, state)?;
    let total_bytes = Rc::new(Cell::new(0_u64));
    let mut next_entry = 0;
    archive
        .extract_to_with_options(options, |metadata| {
            let name = rars::filename::decoded_name(
                metadata.name_bytes(),
                metadata.name_is_unicode,
                None,
            )
            .map_err(io::Error::other)?;
            let name = std::str::from_utf8(name.as_ref()).map_err(io::Error::other)?;
            let is_root_dir = metadata.is_directory && matches!(name, "." | "./" | ".\\");
            let portable = if is_root_dir {
                String::new()
            } else {
                archive_portable_path(name).map_err(io::Error::other)?
            };
            let entry = entries.get(next_entry).ok_or_else(|| {
                io::Error::other("RAR extraction produced an unexpected entry")
            })?;
            if entry.portable != portable || entry.is_dir != metadata.is_directory {
                return Err(io::Error::other("RAR entries changed between passes").into());
            }
            next_entry += 1;
            let destination = state.root.join(&entry.portable);
            if entry.is_dir {
                fs::create_dir_all(&destination)?;
                return Ok(Box::new(io::sink()));
            }
            let parent = destination
                .parent()
                .context("extracted RAR file has no parent")
                .map_err(io::Error::other)?;
            fs::create_dir_all(parent)?;
            let output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            Ok(Box::new(AccountingWriter {
                total_bytes: Rc::clone(&total_bytes),
                max_bytes: state.max_bytes,
                output,
            }))
        })
        .context("RAR extraction or integrity verification failed")?;
    ensure!(
        next_entry == entries.len(),
        "RAR extraction did not produce all validated entries"
    );
    state.bytes = total_bytes.get();
    state.files.extend(
        entries
            .into_iter()
            .filter(|entry| !entry.is_dir)
            .map(|entry| PathBuf::from(entry.portable)),
    );
    Ok(())
}

fn inspect(
    archive: &rars::Archive,
    state: &ExtractState<'_>,
) -> anyhow::Result<Vec<Entry>> {
    let mut validation = ExtractState::new(
        state.root,
        state.max_files,
        state.max_entries,
        state.max_bytes,
    );
    let mut entries = Vec::new();
    let mut declared_bytes = 0_u64;
    let mut file_count = 0_usize;
    for member in archive.members() {
        ensure!(
            !member.meta.is_redirection,
            "links and redirected RAR entries are not allowed"
        );
        ensure!(
            !member.meta.is_split_before && !member.meta.is_split_after,
            "split RAR members are not supported"
        );
        if member.meta.attr_source() == rars::AttrSource::Unix {
            let file_type = member.meta.file_attr & 0o170_000;
            ensure!(
                file_type == 0
                    || (member.meta.is_directory && file_type == 0o040_000)
                    || (!member.meta.is_directory && file_type == 0o100_000),
                "RAR contains an unsupported special entry"
            );
        }
        let name = member
            .decoded_name(None)
            .context("cannot decode RAR entry name")?;
        let name = std::str::from_utf8(name.as_ref()).context("RAR entry pathname is not UTF-8")?;
        if member.meta.is_directory && matches!(name, "." | "./" | ".\\") {
            validation.record_entry()?;
            entries.push(Entry {
                portable: String::new(),
                is_dir: true,
            });
            continue;
        }
        let portable = archive_portable_path(name)
            .with_context(|| format!("unsafe RAR path {name:?}"))?;
        if member.meta.is_directory {
            validation.register(&portable, NodeKind::Directory)?;
        } else {
            file_count += 1;
            ensure!(
                file_count <= state.max_files,
                "archive exceeds the file-count limit"
            );
            validation.begin_file()?;
            validation.register(&portable, NodeKind::File)?;
            declared_bytes = declared_bytes
                .checked_add(member.meta.unpacked_size)
                .context("RAR declared size overflow")?;
            ensure!(
                declared_bytes <= state.max_bytes,
                "archive exceeds the uncompressed-size limit"
            );
        }
        entries.push(Entry {
            portable,
            is_dir: member.meta.is_directory,
        });
    }
    ensure!(!entries.is_empty(), "RAR archive contains no entries");
    Ok(entries)
}

struct AccountingWriter {
    total_bytes: Rc<Cell<u64>>,
    max_bytes: u64,
    output: fs::File,
}

impl Write for AccountingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let total = self
            .total_bytes
            .get()
            .checked_add(buffer.len() as u64)
            .filter(|total| *total <= self.max_bytes)
            .ok_or_else(|| io::Error::other("archive exceeds the uncompressed-size limit"))?;
        let written = self.output.write(buffer)?;
        self.total_bytes
            .set(total - (buffer.len() - written) as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}
