use std::cell::Cell;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use anyhow::{Context, ensure};
use flate2::read::MultiGzDecoder;
use sevenz_rust2::{ArchiveReader, Password};
use zip::ZipArchive;

use super::{ExtractState, archive_portable_path};

const SEVENZ_MEMORY_LIMIT_KIB: usize = 256 * 1024;
const TAR_METADATA_LIMIT: usize = 4 * 1024 * 1024;

pub(super) fn extract(input: &Path, state: &mut ExtractState<'_>) -> anyhow::Result<()> {
    let mut reader = BufReader::new(File::open(input)?);
    let magic = reader.fill_buf()?;
    if magic.starts_with(b"7z\xbc\xaf\x27\x1c") {
        extract_7z(reader, state).context("extracting 7z archive")
    } else if matches!(
        magic.get(..4),
        Some(b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08")
    ) {
        extract_zip(reader, state).context("extracting ZIP archive")
    } else if magic.starts_with(b"\x1f\x8b") {
        extract_tar(MultiGzDecoder::new(reader), state)
            .context("extracting gzip-compressed TAR archive")
    } else {
        extract_tar(reader, state).context("extracting TAR archive")
    }
}

fn extract_zip(reader: BufReader<File>, state: &mut ExtractState<'_>) -> anyhow::Result<()> {
    let mut archive = ZipArchive::new(reader)?;
    ensure!(
        archive.len() <= state.max_entries,
        "archive exceeds the total-entry limit"
    );
    ensure!(
        !archive.has_duplicate_names(),
        "ZIP contains duplicate entry names"
    );
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let directory = entry.is_dir();
        let mode = entry.unix_mode().unwrap_or_default();
        ensure!(
            !entry.is_symlink() && is_regular_mode(mode, directory),
            "ZIP contains an unsupported special entry"
        );
        if let Some(path) = prepare_entry(state, &entry.name()?, directory)? {
            let size = entry.size();
            state.write_file(&path, &mut entry, Some(size))?;
        }
    }
    Ok(())
}

fn extract_7z(reader: BufReader<File>, state: &mut ExtractState<'_>) -> anyhow::Result<()> {
    let mut archive =
        ArchiveReader::new_with_memory_limit(reader, Password::empty(), SEVENZ_MEMORY_LIMIT_KIB)?;
    ensure!(
        archive.archive().files.len() <= state.max_entries,
        "archive exceeds the total-entry limit"
    );
    archive.for_each_entries(|entry, data| {
        if entry.is_anti_item
            || entry.windows_attributes & 0x400 != 0
            || !is_regular_mode(entry.windows_attributes >> 16, entry.is_directory)
        {
            return Err(io::Error::other("7z contains an unsupported special entry").into());
        }
        if let Some(path) =
            prepare_entry(state, &entry.name, entry.is_directory).map_err(io::Error::other)?
        {
            state
                .write_file(&path, data, Some(entry.size))
                .map_err(io::Error::other)?;
        }
        Ok(true)
    })?;
    Ok(())
}

fn is_regular_mode(mode: u32, directory: bool) -> bool {
    let kind = mode & 0o170000;
    kind == 0 || kind == if directory { 0o040000 } else { 0o100000 }
}

fn extract_tar(mut reader: impl Read, state: &mut ExtractState<'_>) -> anyhow::Result<()> {
    let remaining = Cell::new(None);
    let mut archive = tar::Archive::new(TarInput {
        reader: &mut reader,
        remaining: &remaining,
    });
    let mut entries = archive.entries()?;
    loop {
        // tar resolves PAX and GNU metadata internally before yielding an entry.
        remaining.set(Some(TAR_METADATA_LIMIT));
        let entry = entries.next().transpose()?;
        let header_bytes = TAR_METADATA_LIMIT - remaining.replace(None).unwrap();
        let Some(mut entry) = entry else {
            ensure!(
                state.entries > 0 || header_bytes >= 512,
                "not a TAR archive"
            );
            break;
        };
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            state.record_entry()?;
            continue;
        }
        ensure!(
            kind.is_file() || kind.is_dir() || kind.is_gnu_sparse(),
            "TAR contains an unsupported special entry"
        );
        let path = {
            let bytes = entry.path_bytes();
            prepare_entry(state, std::str::from_utf8(&bytes)?, kind.is_dir())?
        };
        if let Some(path) = path {
            let size = entry.size();
            state.write_file(&path, &mut entry, Some(size))?;
        }
    }
    // Drive the gzip reader to EOF so it verifies the trailer, including CRC.
    // Limit the work after the TAR end marker independently of the file budget.
    let trailing = io::copy(
        &mut reader.take(TAR_METADATA_LIMIT as u64 + 1),
        &mut io::sink(),
    )?;
    ensure!(
        trailing <= TAR_METADATA_LIMIT as u64,
        "TAR exceeds the trailing-data limit"
    );
    Ok(())
}

struct TarInput<'a, R> {
    reader: &'a mut R,
    remaining: &'a Cell<Option<usize>>,
}

impl<R: Read> Read for TarInput<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let Some(remaining) = self.remaining.get() else {
            return self.reader.read(buffer);
        };
        if remaining == 0 && !buffer.is_empty() {
            return Err(io::Error::other("TAR exceeds the metadata-size limit"));
        }
        let limit = remaining.min(buffer.len());
        let count = self.reader.read(&mut buffer[..limit])?;
        self.remaining.set(Some(remaining - count));
        Ok(count)
    }
}

fn prepare_entry(
    state: &mut ExtractState<'_>,
    name: &str,
    directory: bool,
) -> anyhow::Result<Option<String>> {
    if directory && matches!(name, "." | "./" | ".\\") {
        state.record_entry()?;
        return Ok(None);
    }
    let path =
        archive_portable_path(name).with_context(|| format!("unsafe archive path {name:?}"))?;
    if directory {
        state.add_directory(&path)?;
        return Ok(None);
    }
    Ok(Some(path))
}
