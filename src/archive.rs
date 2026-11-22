mod rar;

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::CStr;
#[cfg(not(windows))]
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::raw::{c_char, c_int};
#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::FileTypeExt};
#[cfg(windows)]
use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use anyhow::{Context, bail, ensure};
use tempfile::{Builder, TempDir};

const MAX_FILES: usize = 20_000;
const MAX_UNPACKED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const COPY_BUFFER_SIZE: usize = 64 * 1024;

const MAX_ENTRIES: usize = 100_000;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_PATH_DEPTH: usize = 128;

#[repr(C)]
struct ArchiveHandle {
    _private: [u8; 0],
}

#[repr(C)]
struct ArchiveEntry {
    _private: [u8; 0],
}

#[link(name = "archive")]
unsafe extern "C" {
    fn archive_read_new() -> *mut ArchiveHandle;
    fn archive_read_free(archive: *mut ArchiveHandle) -> c_int;
    fn archive_read_support_filter_none(archive: *mut ArchiveHandle) -> c_int;
    fn archive_read_support_filter_gzip(archive: *mut ArchiveHandle) -> c_int;
    fn archive_read_support_format_zip(archive: *mut ArchiveHandle) -> c_int;
    fn archive_read_support_format_tar(archive: *mut ArchiveHandle) -> c_int;
    fn archive_read_support_format_rar(archive: *mut ArchiveHandle) -> c_int;
    fn archive_read_support_format_rar5(archive: *mut ArchiveHandle) -> c_int;
    fn archive_read_support_format_7zip(archive: *mut ArchiveHandle) -> c_int;
    #[cfg(not(windows))]
    fn archive_read_open_filename(
        archive: *mut ArchiveHandle,
        filename: *const c_char,
        block_size: usize,
    ) -> c_int;
    fn archive_read_next_header(
        archive: *mut ArchiveHandle,
        entry: *mut *mut ArchiveEntry,
    ) -> c_int;
    fn archive_read_data(archive: *mut ArchiveHandle, buffer: *mut u8, size: usize) -> isize;
    fn archive_read_data_skip(archive: *mut ArchiveHandle) -> c_int;
    fn archive_error_string(archive: *mut ArchiveHandle) -> *const c_char;
    fn archive_format(archive: *mut ArchiveHandle) -> c_int;
    fn archive_entry_pathname(entry: *mut ArchiveEntry) -> *const c_char;
    fn archive_entry_pathname_utf8(entry: *mut ArchiveEntry) -> *const c_char;
    fn archive_entry_strmode(entry: *mut ArchiveEntry) -> *const c_char;
    fn archive_entry_size_is_set(entry: *mut ArchiveEntry) -> c_int;
    fn archive_entry_size(entry: *mut ArchiveEntry) -> i64;
    fn archive_entry_symlink(entry: *mut ArchiveEntry) -> *const c_char;
    fn archive_entry_hardlink(entry: *mut ArchiveEntry) -> *const c_char;
    #[cfg(windows)]
    fn archive_read_open_filename_w(
        archive: *mut ArchiveHandle,
        filename: *const u16,
        block_size: usize,
    ) -> c_int;
}

const ARCHIVE_OK: c_int = 0;
const ARCHIVE_EOF: c_int = 1;
const ARCHIVE_FORMAT_BASE_MASK: c_int = 0xFF_0000;
const ARCHIVE_FORMAT_ZIP: c_int = 0x05_0000;

pub struct ArchiveTree {
    _temp_dir: TempDir,
    pub files: Vec<PathBuf>,
}

impl ArchiveTree {
    pub fn root(&self) -> &Path {
        self._temp_dir.path()
    }
}

/// Convert a relative path into a portable, case-preserving `/`-separated key.
/// Backslashes are treated as separators so Windows-style traversal is rejected
/// consistently on every host.
pub fn portable_path(path: &Path) -> anyhow::Result<String> {
    let raw = path.to_str().context("path is not valid UTF-8")?;
    ensure!(!raw.is_empty(), "path is empty");
    ensure!(
        !raw.starts_with('/') && !raw.starts_with('\\'),
        "absolute paths are not allowed"
    );
    // Windows drive-relative and drive-absolute paths are not portable either.
    ensure!(!raw.contains(':'), "colon is not allowed in portable paths");

    ensure!(
        raw.len() <= MAX_PATH_BYTES,
        "path exceeds the portable path limit"
    );
    let components = raw.split(['/', '\\']).collect::<Vec<_>>();
    let end = components
        .len()
        .saturating_sub(usize::from(components.last() == Some(&"")));
    ensure!(end > 0, "path has no components");
    ensure!(
        end <= MAX_PATH_DEPTH,
        "path exceeds the component-depth limit"
    );
    let mut normalized = Vec::with_capacity(end);
    for component in &components[..end] {
        ensure!(!component.is_empty(), "empty path component");
        ensure!(
            *component != "." && *component != "..",
            "dot path components are not allowed"
        );
        validate_windows_component(component)?;
        normalized.push(*component);
    }
    Ok(normalized.join("/"))
}

fn validate_windows_component(component: &str) -> anyhow::Result<()> {
    ensure!(
        !component.ends_with('.') && !component.ends_with(' '),
        "Windows paths cannot end a component with a dot or space"
    );
    ensure!(
        !component
            .chars()
            .any(|c| c.is_control() || "<>\"|?*".contains(c)),
        "path component contains a character forbidden on Windows"
    );
    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.trim_end_matches(' ').to_ascii_uppercase();
    let reserved = matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        upper.strip_prefix(prefix).is_some_and(|suffix| {
            (suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
                || matches!(suffix, "¹" | "²" | "³")
        })
    });
    ensure!(!reserved, "Windows device names are not allowed");
    Ok(())
}

pub fn unpack(input: &Path, scratch: &Path) -> anyhow::Result<ArchiveTree> {
    unpack_with_limits(input, scratch, MAX_FILES, MAX_ENTRIES, MAX_UNPACKED_BYTES)
}

fn unpack_with_limits(
    input: &Path,
    scratch: &Path,
    max_files: usize,
    max_entries: usize,
    max_bytes: u64,
) -> anyhow::Result<ArchiveTree> {
    let metadata = fs::symlink_metadata(input)
        .with_context(|| format!("cannot inspect input {}", input.display()))?;
    ensure!(
        !is_reparse_or_symlink(&metadata),
        "input cannot be a symlink or reparse point"
    );
    if metadata.is_dir() {
        ensure!(
            !scratch.canonicalize()?.starts_with(input.canonicalize()?),
            "import scratch directory cannot be inside the input folder"
        );
    }

    let temp = || {
        Builder::new()
            .prefix("dmm-import-")
            .tempdir_in(scratch)
            .with_context(|| {
                format!(
                    "cannot create private import directory in {}",
                    scratch.display()
                )
            })
    };
    let mut temp_dir = temp()?;
    let mut files = if metadata.is_dir() {
        let mut state = ExtractState::new(temp_dir.path(), max_files, max_entries, max_bytes);
        copy_directory(input, Path::new(""), &mut state)?;
        state.files
    } else {
        ensure!(
            metadata.is_file(),
            "input must be a regular archive file or directory"
        );
        let mut state = ExtractState::new(temp_dir.path(), max_files, max_entries, max_bytes);
        match extract_archive(input, &mut state) {
            Ok(()) => state.files,
            Err(libarchive_error) if rar::is_rar(input)? => {
                drop(state);
                temp_dir = temp()?;
                let mut retry =
                    ExtractState::new(temp_dir.path(), max_files, max_entries, max_bytes);
                rar::extract(input, &mut retry).with_context(|| {
                    format!("libarchive failed ({libarchive_error:#}); UnRAR fallback failed")
                })?;
                retry.files
            }
            Err(error) => return Err(error),
        }
    };
    files.sort();
    Ok(ArchiveTree {
        _temp_dir: temp_dir,
        files,
    })
}

struct ExtractState<'a> {
    root: &'a Path,
    max_files: usize,
    max_entries: usize,
    max_bytes: u64,
    entries: usize,
    files: Vec<PathBuf>,
    bytes: u64,
    paths: HashMap<String, PathNode>,
}

struct PathNode {
    spelling: String,
    kind: NodeKind,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    ImpliedDirectory,
    Directory,
    File,
}

impl<'a> ExtractState<'a> {
    fn new(root: &'a Path, max_files: usize, max_entries: usize, max_bytes: u64) -> Self {
        Self {
            root,
            max_files,
            max_entries,
            max_bytes,
            entries: 0,
            files: Vec::new(),
            bytes: 0,
            paths: HashMap::new(),
        }
    }
    fn record_entry(&mut self) -> anyhow::Result<()> {
        self.entries = self
            .entries
            .checked_add(1)
            .context("archive entry count overflow")?;
        ensure!(
            self.entries <= self.max_entries,
            "archive exceeds the total-entry limit"
        );
        Ok(())
    }

    fn register(&mut self, path: &str, kind: NodeKind) -> anyhow::Result<PathBuf> {
        self.record_entry()?;
        let mut prefix = String::new();
        let components = path.split('/').collect::<Vec<_>>();
        for (index, component) in components.iter().enumerate() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            let final_component = index + 1 == components.len();
            let current_kind = if final_component {
                kind
            } else {
                NodeKind::ImpliedDirectory
            };
            let key = prefix.to_lowercase();
            if let Some(existing) = self.paths.get(&key) {
                ensure!(
                    existing.spelling == prefix,
                    "path component collides case-insensitively: {prefix}"
                );
                match (existing.kind, current_kind) {
                    (NodeKind::File, _) | (_, NodeKind::File) => {
                        bail!("file and directory paths collide: {prefix}");
                    }
                    (NodeKind::Directory, NodeKind::Directory) => {
                        bail!("duplicate directory path: {prefix}");
                    }
                    (NodeKind::Directory, NodeKind::ImpliedDirectory) => {}
                    (NodeKind::ImpliedDirectory, NodeKind::Directory) => {
                        self.paths.insert(
                            key,
                            PathNode {
                                spelling: prefix.clone(),
                                kind: NodeKind::Directory,
                            },
                        );
                    }
                    (NodeKind::ImpliedDirectory, NodeKind::ImpliedDirectory) => {}
                }
            } else {
                self.paths.insert(
                    key,
                    PathNode {
                        spelling: prefix.clone(),
                        kind: current_kind,
                    },
                );
            }
        }
        Ok(self.root.join(path))
    }

    fn begin_file(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.files.len() < self.max_files,
            "archive exceeds the file-count limit"
        );
        Ok(())
    }

    fn write_file<R: Read>(&mut self, path: &str, reader: &mut R) -> anyhow::Result<()> {
        self.begin_file()?;
        let destination = self.register(path, NodeKind::File)?;
        let parent = destination
            .parent()
            .context("extracted file has no parent")?;
        fs::create_dir_all(parent)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .with_context(|| format!("cannot create extracted file {}", destination.display()))?;
        let mut buffer = [0_u8; COPY_BUFFER_SIZE];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            self.bytes = self
                .bytes
                .checked_add(count as u64)
                .context("unpacked size overflow")?;
            ensure!(
                self.bytes <= self.max_bytes,
                "archive exceeds the uncompressed-size limit"
            );
            output.write_all(&buffer[..count])?;
        }
        self.files.push(PathBuf::from(path));
        Ok(())
    }

    fn add_directory(&mut self, path: &str) -> anyhow::Result<()> {
        let destination = self.register(path, NodeKind::Directory)?;
        fs::create_dir_all(destination)?;
        Ok(())
    }
}

struct ArchiveReader(NonNull<ArchiveHandle>);

impl ArchiveReader {
    fn open(path: &Path) -> anyhow::Result<Self> {
        let raw = NonNull::new(unsafe { archive_read_new() })
            .context("libarchive could not allocate a reader")?;
        let reader = Self(raw);
        for (result, format) in [
            (
                unsafe { archive_read_support_filter_none(raw.as_ptr()) },
                "no compression",
            ),
            (
                unsafe { archive_read_support_filter_gzip(raw.as_ptr()) },
                "gzip",
            ),
            (
                unsafe { archive_read_support_format_zip(raw.as_ptr()) },
                "ZIP",
            ),
            (
                unsafe { archive_read_support_format_tar(raw.as_ptr()) },
                "TAR",
            ),
            (
                unsafe { archive_read_support_format_rar(raw.as_ptr()) },
                "RAR",
            ),
            (
                unsafe { archive_read_support_format_rar5(raw.as_ptr()) },
                "RAR5",
            ),
            (
                unsafe { archive_read_support_format_7zip(raw.as_ptr()) },
                "7z",
            ),
        ] {
            reader.check(result, &format!("enable {format} support"))?;
        }
        reader.open_filename(path)?;
        Ok(reader)
    }

    fn open_filename(&self, path: &Path) -> anyhow::Result<()> {
        #[cfg(unix)]
        let status = {
            let filename = CString::new(path.as_os_str().as_bytes())
                .context("archive path contains a NUL byte")?;
            unsafe { archive_read_open_filename(self.0.as_ptr(), filename.as_ptr(), 10 * 1024) }
        };
        #[cfg(windows)]
        let status = {
            let mut filename = path.as_os_str().encode_wide().collect::<Vec<_>>();
            filename.push(0);
            unsafe { archive_read_open_filename_w(self.0.as_ptr(), filename.as_ptr(), 10 * 1024) }
        };
        #[cfg(not(any(unix, windows)))]
        let status = {
            let filename = CString::new(path.to_string_lossy().as_bytes())
                .context("archive path contains a NUL byte")?;
            unsafe { archive_read_open_filename(self.0.as_ptr(), filename.as_ptr(), 10 * 1024) }
        };
        self.check(status, "open archive")
    }

    fn next_entry(&mut self) -> anyhow::Result<Option<NonNull<ArchiveEntry>>> {
        let mut entry = std::ptr::null_mut();
        let status = unsafe { archive_read_next_header(self.0.as_ptr(), &mut entry) };
        if status == ARCHIVE_EOF {
            return Ok(None);
        }
        self.check(status, "read archive header")?;
        NonNull::new(entry)
            .map(Some)
            .context("libarchive returned a null entry")
    }

    fn read_data(&mut self, buffer: &mut [u8]) -> anyhow::Result<usize> {
        let count =
            unsafe { archive_read_data(self.0.as_ptr(), buffer.as_mut_ptr(), buffer.len()) };
        ensure!(
            count >= 0,
            "{}",
            Self::integrity_guidance(self.error_message("read archive data"))
        );
        usize::try_from(count).context("invalid archive read length")
    }

    fn integrity_guidance(message: String) -> String {
        let lower = message.to_ascii_lowercase();
        if lower.contains("checksum") || lower.contains("crc") {
            format!(
                "{message}; archive integrity verification failed. Download the archive again; if the error persists, test it with the original archive tool and repackage only after it verifies."
            )
        } else {
            message
        }
    }

    fn skip_data(&mut self) -> anyhow::Result<()> {
        self.check(
            unsafe { archive_read_data_skip(self.0.as_ptr()) },
            "skip archive entry",
        )
    }

    fn check(&self, status: c_int, action: &str) -> anyhow::Result<()> {
        if status == ARCHIVE_OK {
            Ok(())
        } else {
            bail!("{}", Self::integrity_guidance(self.error_message(action)));
        }
    }

    fn error_message(&self, action: &str) -> String {
        let pointer = unsafe { archive_error_string(self.0.as_ptr()) };
        if pointer.is_null() {
            format!("libarchive failed to {action}")
        } else {
            format!(
                "libarchive failed to {action}: {}",
                unsafe { CStr::from_ptr(pointer) }.to_string_lossy()
            )
        }
    }
}

impl Drop for ArchiveReader {
    fn drop(&mut self) {
        unsafe {
            archive_read_free(self.0.as_ptr());
        }
    }
}

#[cfg(unix)]
struct ArchiveLocale {
    previous: libc::locale_t,
    utf8: libc::locale_t,
}

#[cfg(unix)]
impl ArchiveLocale {
    fn utf8() -> Option<Self> {
        // libarchive uses LC_CTYPE for pathname conversions, even when asked for UTF-8.
        // Keep the locale thread-local so imports never change other app threads.
        for name in [c"C.UTF-8", c"UTF-8", c"en_US.UTF-8"] {
            let utf8 = unsafe {
                libc::newlocale(libc::LC_CTYPE_MASK, name.as_ptr(), std::ptr::null_mut())
            };
            if utf8.is_null() {
                continue;
            }
            let previous = unsafe { libc::uselocale(utf8) };
            if previous.is_null() {
                unsafe { libc::freelocale(utf8) };
                return None;
            }
            return Some(Self { previous, utf8 });
        }
        None
    }
}

#[cfg(unix)]
impl Drop for ArchiveLocale {
    fn drop(&mut self) {
        unsafe {
            libc::uselocale(self.previous);
            libc::freelocale(self.utf8);
        }
    }
}

fn extract_archive(input: &Path, state: &mut ExtractState<'_>) -> anyhow::Result<()> {
    #[cfg(unix)]
    let _locale = ArchiveLocale::utf8();
    let mut archive = ArchiveReader::open(input)
        .with_context(|| format!("cannot open archive {}", input.display()))?;
    while let Some(entry) = archive.next_entry()? {
        let entry = entry.as_ptr();
        let pathname = unsafe { archive_entry_pathname_utf8(entry) };
        let name = if !pathname.is_null() {
            Cow::Borrowed(
                unsafe { CStr::from_ptr(pathname) }
                    .to_str()
                    .context("archive entry pathname is not UTF-8")?,
            )
        } else {
            let pathname = unsafe { archive_entry_pathname(entry) };
            ensure!(
                !pathname.is_null(),
                "archive entry has no recoverable pathname"
            );
            let bytes = unsafe { CStr::from_ptr(pathname) }.to_bytes();
            if let Ok(text) = std::str::from_utf8(bytes) {
                Cow::Borrowed(text)
            } else {
                ensure!(
                    unsafe { archive_format(archive.0.as_ptr()) } & ARCHIVE_FORMAT_BASE_MASK
                        == ARCHIVE_FORMAT_ZIP,
                    "archive entry pathname is not UTF-8"
                );
                Cow::Owned(decode_cp437(bytes))
            }
        };
        ensure!(
            unsafe { archive_entry_symlink(entry) }.is_null(),
            "symbolic links are not allowed in archives"
        );
        ensure!(
            unsafe { archive_entry_hardlink(entry) }.is_null(),
            "hard links are not allowed in archives"
        );
        // strmode's first byte describes the type without depending on platform mode_t.
        let mode = unsafe { archive_entry_strmode(entry) };
        ensure!(!mode.is_null(), "missing archive entry type");
        let file_type = unsafe { *mode } as u8;
        if matches!(name.as_ref(), "." | "./" | ".\\") && file_type == b'd' {
            state.record_entry()?;
            archive.skip_data()?;
            continue;
        }
        let portable = archive_portable_path(&name)
            .with_context(|| format!("unsafe archive path {name:?}"))?;
        match file_type {
            b'-' => {
                state.begin_file()?;
                let destination = state.register(&portable, NodeKind::File)?;
                let parent = destination
                    .parent()
                    .context("extracted file has no parent")?;
                fs::create_dir_all(parent)?;
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .with_context(|| {
                        format!("cannot create extracted file {}", destination.display())
                    })?;
                let expected_size = if unsafe { archive_entry_size_is_set(entry) } != 0 {
                    let size = unsafe { archive_entry_size(entry) };
                    ensure!(size >= 0, "archive entry has a negative size");
                    Some(u64::try_from(size)?)
                } else {
                    None
                };
                let mut entry_bytes = 0_u64;
                let mut buffer = [0_u8; COPY_BUFFER_SIZE];
                loop {
                    let count = archive.read_data(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    entry_bytes = entry_bytes
                        .checked_add(count as u64)
                        .context("archive entry size overflow")?;
                    account_and_write(state, &mut output, &buffer[..count])?;
                }
                if let Some(expected_size) = expected_size {
                    ensure!(
                        entry_bytes == expected_size,
                        "archive entry size does not match its declared size"
                    );
                }
                state.files.push(PathBuf::from(portable));
            }
            b'd' => {
                state.add_directory(&portable)?;
                archive.skip_data()?;
            }
            _ => bail!("archive contains an unsupported special entry: {name:?}"),
        }
    }
    Ok(())
}

fn decode_cp437(bytes: &[u8]) -> String {
    let mut decoded = String::with_capacity(bytes.len());
    for &byte in bytes {
        if byte < 0x80 {
            decoded.push(char::from(byte));
        } else {
            decoded.push(CP437[usize::from(byte - 0x80)]);
        }
    }
    decoded
}

const CP437: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ',
    'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ',
    'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕',
    '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦',
    '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐',
    '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', '≡', '±',
    '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', ' ',
];

fn archive_portable_path(name: &str) -> anyhow::Result<String> {
    ensure!(
        name.len() <= MAX_PATH_BYTES,
        "archive path exceeds the path-length limit"
    );
    let mut name = name;
    while let Some(rest) = name.strip_prefix("./").or_else(|| name.strip_prefix(".\\")) {
        name = rest;
    }
    portable_path(Path::new(name))
}

fn account_and_write(
    state: &mut ExtractState<'_>,
    output: &mut File,
    bytes: &[u8],
) -> anyhow::Result<()> {
    state.bytes = state
        .bytes
        .checked_add(bytes.len() as u64)
        .context("unpacked size overflow")?;
    ensure!(
        state.bytes <= state.max_bytes,
        "archive exceeds the uncompressed-size limit"
    );
    output.write_all(bytes)?;
    Ok(())
}

fn copy_directory(
    input: &Path,
    relative: &Path,
    state: &mut ExtractState<'_>,
) -> anyhow::Result<()> {
    let entries = fs::read_dir(input)
        .with_context(|| format!("cannot read input directory {}", input.display()))?;
    for entry in entries {
        let entry = entry?;
        let source = entry.path();
        let metadata = fs::symlink_metadata(&source)?;
        ensure!(
            !is_reparse_or_symlink(&metadata),
            "directory import contains a symlink or reparse point"
        );
        let name = entry.file_name();
        let mut relative_path = relative.to_path_buf();
        relative_path.push(name);
        let portable = portable_path(&relative_path)
            .with_context(|| format!("unsafe directory import path {}", relative_path.display()))?;
        if metadata.is_dir() {
            state.add_directory(&portable)?;
            copy_directory(&source, Path::new(&portable), state)?;
        } else {
            ensure!(
                metadata.is_file(),
                "directory import contains a special file"
            );
            let mut source_file = File::open(&source)?;
            state.write_file(&portable, &mut source_file)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    let file_type = metadata.file_type();
    file_type.is_symlink()
        || file_type.is_block_device()
        || file_type.is_char_device()
        || file_type.is_fifo()
        || file_type.is_socket()
}

#[cfg(windows)]
pub(crate) fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::{archive_portable_path, portable_path, unpack, unpack_with_limits};
    use std::fs;
    use std::path::Path;
    use tempfile::Builder;

    fn workspace_tempdir() -> tempfile::TempDir {
        Builder::new()
            .prefix("dmm-archive-test-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap()
    }

    #[test]
    fn extracts_zip_with_a_legacy_cp437_filename() -> anyhow::Result<()> {
        let temp = workspace_tempdir();
        let path = temp.path().join("legacy.zip");
        fs::write(&path, b"PK\x03\x04\x14\0\0\0\0\0\x24\x9e\x76\x5d\x83\x16\xdc\x8c\x01\0\0\0\x01\0\0\0\x0c\0\0\0caf\x82/mod.inix\
PK\x01\x02\x14\x03\x14\0\0\0\0\0\x24\x9e\x76\x5d\x83\x16\xdc\x8c\x01\0\0\0\x01\0\0\0\x0c\0\0\0\0\0\0\0\0\0\x80\x01\0\0\0\0caf\x82/mod.ini\
PK\x05\x06\0\0\0\0\x01\0\x01\0\x3a\0\0\0\x2b\0\0\0\0\0")?;
        let archive = unpack(&path, temp.path())?;
        assert_eq!(archive.files, [Path::new("café/mod.ini")]);
        assert_eq!(fs::read(archive.root().join("café/mod.ini"))?, b"x");
        Ok(())
    }

    #[test]
    fn normalizes_portable_paths_and_preserves_case() {
        assert_eq!(
            portable_path(Path::new("Mods\\Example/file.dll")).unwrap(),
            "Mods/Example/file.dll"
        );
    }

    #[test]
    fn archive_paths_accept_tar_dot_prefix_without_accepting_traversal() {
        assert_eq!(
            archive_portable_path("././Mods/file.dll").unwrap(),
            "Mods/file.dll"
        );
        assert!(archive_portable_path("./../outside").is_err());
        let long_path = "x".repeat(4_097);
        assert!(portable_path(Path::new(&long_path)).is_err());
    }
    #[test]
    fn rejects_traversal_and_windows_unsafe_names() {
        for path in [
            "../outside",
            "folder/../outside",
            "/absolute",
            "C:\\outside",
            "folder\\..\\outside",
            "CON.txt",
            "name. ",
            "bad?.dll",
        ] {
            assert!(portable_path(Path::new(path)).is_err(), "accepted {path:?}");
        }
    }

    #[test]
    fn rejects_case_collisions_across_components() {
        let temp = workspace_tempdir();
        let mut state = super::ExtractState::new(temp.path(), 10, 10, 100);
        state
            .register("Mods/one.dll", super::NodeKind::File)
            .unwrap();
        assert!(
            state
                .register("mods/two.dll", super::NodeKind::File)
                .is_err()
        );
    }

    #[test]
    fn directory_import_enforces_limits_while_copying() {
        let temp = workspace_tempdir();
        let source = temp.path().join("source");
        let scratch = temp.path().join("scratch");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&scratch).unwrap();
        fs::write(source.join("one.dll"), b"123").unwrap();
        fs::write(source.join("two.dll"), b"4").unwrap();

        assert!(unpack_with_limits(&source, &scratch, 1, 10, 100).is_err());
        assert!(unpack_with_limits(&source, &scratch, 10, 10, 2).is_err());
    }

    #[test]
    fn directory_import_limits_empty_entries_and_path_components() {
        let temp = workspace_tempdir();
        let source = temp.path().join("source");
        let scratch = temp.path().join("scratch");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&scratch).unwrap();
        for name in ["one", "two", "three"] {
            fs::create_dir(source.join(name)).unwrap();
        }

        assert!(unpack_with_limits(&source, &scratch, 10, 2, 100).is_err());
        let deep_path = format!("{}file", "a/".repeat(129));
        assert!(portable_path(Path::new(&deep_path)).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn directory_import_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = workspace_tempdir();
        let source = temp.path().join("source");
        let scratch = temp.path().join("scratch");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&scratch).unwrap();
        symlink(temp.path(), source.join("link")).unwrap();

        assert!(unpack_with_limits(&source, &scratch, 10, 10, 100).is_err());
    }
}
