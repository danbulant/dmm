mod portable;
mod rar;

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail, ensure};
use tempfile::{Builder, TempDir};

const MAX_FILES: usize = 20_000;
const MAX_UNPACKED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const COPY_BUFFER_SIZE: usize = 64 * 1024;

const MAX_ENTRIES: usize = 100_000;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_PATH_DEPTH: usize = 128;

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
    let temp_dir = temp()?;
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
        if rar::is_rar(input)? {
            rar::extract(input, &mut state)?;
        } else {
            portable::extract(input, &mut state)?;
        }
        state.files
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

    fn write_file(
        &mut self,
        path: &str,
        reader: &mut (impl Read + ?Sized),
        size: Option<u64>,
    ) -> anyhow::Result<()> {
        self.begin_file()?;
        let remaining = self.max_bytes - self.bytes;
        let budget = size.unwrap_or(remaining);
        ensure!(
            budget <= remaining,
            "archive exceeds the uncompressed-size limit"
        );
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
        let mut written = 0;
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            written += count as u64;
            ensure!(
                written <= budget,
                "archive entry exceeds its declared size or the uncompressed-size limit"
            );
            output.write_all(&buffer[..count])?;
        }
        ensure!(
            size.is_none_or(|size| size == written),
            "archive entry size does not match its declared size"
        );
        self.bytes += written;
        self.files.push(PathBuf::from(path));
        Ok(())
    }

    fn add_directory(&mut self, path: &str) -> anyhow::Result<()> {
        let destination = self.register(path, NodeKind::Directory)?;
        fs::create_dir_all(destination)?;
        Ok(())
    }
}

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
            state.write_file(&portable, &mut source_file, None)?;
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
    use flate2::{Compression, write::GzEncoder};
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use tempfile::Builder;
    use zip::{ZipWriter, write::SimpleFileOptions};

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
PK\x01\x02\x14\x03\x14\0\0\0\0\0\x24\x9e\x76\x5d\x83\x16\xdc\x8c\x01\0\0\0\x01\0\0\0\x0c\0\0\0\0\0\0\0\0\0\0\0\x80\x01\0\0\0\0caf\x82/mod.ini\
PK\x05\x06\0\0\0\0\x01\0\x01\0\x3a\0\0\0\x2b\0\0\0\0\0")?;
        let archive = unpack(&path, temp.path())?;
        assert_eq!(archive.files, [Path::new("café/mod.ini")]);
        assert_eq!(fs::read(archive.root().join("café/mod.ini"))?, b"x");
        Ok(())
    }

    #[test]
    fn rejects_zip_duplicates_and_special_files() -> anyhow::Result<()> {
        let temp = workspace_tempdir();
        let path = temp.path().join("download");
        let mut zip = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for name in ["one.ini", "two.ini"] {
            zip.start_file(name, SimpleFileOptions::default())?;
            zip.write_all(b"mod")?;
        }
        let mut duplicate = zip.finish()?.into_inner();
        for index in 0..=duplicate.len() - b"two.ini".len() {
            if duplicate[index..].starts_with(b"two.ini") {
                duplicate[index..index + 7].copy_from_slice(b"one.ini");
            }
        }
        fs::write(&path, duplicate)?;
        let error = unpack(&path, temp.path()).err().unwrap();
        assert!(format!("{error:#}").contains("duplicate entry"));

        let mut zip = ZipWriter::new(fs::File::create(&path)?);
        zip.add_symlink("link", "target", SimpleFileOptions::default())?;
        drop(zip.finish()?);
        let error = unpack(&path, temp.path()).err().unwrap();
        assert!(format!("{error:#}").contains("unsupported special entry"));
        Ok(())
    }

    #[test]
    fn validates_tar_gzip_checksum_in_an_extensionless_download() -> anyhow::Result<()> {
        let temp = workspace_tempdir();
        let path = temp.path().join("download");
        let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(3);
        tar.append_data(&mut header, "nested/mod.ini", b"mod".as_slice())?;
        let mut compressed = tar.into_inner()?.finish()?;
        fs::write(&path, &compressed)?;
        let tree = unpack(&path, temp.path())?;
        assert_eq!(fs::read(tree.root().join("nested/mod.ini"))?, b"mod");
        let crc = compressed.len() - 8;
        compressed[crc] ^= 1;
        fs::write(&path, compressed)?;
        assert!(unpack(&path, temp.path()).is_err());
        Ok(())
    }

    #[test]
    fn extracts_both_files_from_a_solid_7z_block() -> anyhow::Result<()> {
        const SOLID_7Z: &[u8] = b"\
            \x37\x7a\xbc\xaf\x27\x1c\x00\x04\xb5\x1f\x36\xd3\x5d\x00\x00\x00\
            \x00\x00\x00\x00\x20\x00\x00\x00\x00\x00\x00\x00\xd0\xc8\xf6\xbb\
            \x01\x00\x07\x6f\x6e\x65\x0a\x74\x77\x6f\x0a\x00\x00\x00\x81\x33\
            \x07\xae\x0f\xce\x9c\x06\xc5\x09\x2a\xae\xd3\xaa\x85\x26\x28\x13\
            \x18\xbb\x03\x25\x67\x1c\x69\x40\x99\x0c\x9d\x35\x80\xf5\x93\xa1\
            \x73\xc6\x5d\x34\x1e\xaf\x70\xc5\xc2\x1c\x28\x0f\xea\x65\x48\x14\
            \xea\x82\xa6\xb3\x97\xf7\xc0\xde\x5d\xdc\xac\x79\xfe\x03\x1a\x59\
            \x28\x1f\xd8\x9e\x41\x07\x69\x75\xd7\x09\x45\x92\x00\x17\x06\x0c\
            \x01\x09\x51\x00\x07\x0b\x01\x00\x01\x23\x03\x01\x01\x05\x5d\x00\
            \x10\x00\x00\x0c\x5e\x0a\x01\xe9\x69\x06\xda\x00\x00";
        let temp = workspace_tempdir();
        let path = temp.path().join("download");
        fs::write(&path, SOLID_7Z)?;
        assert!(unpack_with_limits(&path, temp.path(), 2, 2, 7).is_err());
        let tree = unpack_with_limits(&path, temp.path(), 2, 2, 8)?;
        assert_eq!(tree.files, [Path::new("one.ini"), Path::new("two.ini")]);
        assert_eq!(fs::read(tree.root().join("one.ini"))?, b"one\n");
        assert_eq!(fs::read(tree.root().join("two.ini"))?, b"two\n");
        Ok(())
    }

    #[test]
    fn extracts_solid_rar_and_cleans_failed_limited_imports() -> anyhow::Result<()> {
        // Upstream RARS Apache-2.0 fixture: crates/rars/tests/fixtures/rar50/solid.rar.
        const SOLID_RAR: &[u8] = b"\
            \x52\x61\x72\x21\x1a\x07\x01\x00\x20\xb6\xfa\x11\x0a\x01\x05\x06\
            \x04\x05\x01\x01\x80\x80\x00\x5b\xf2\xd9\x43\x44\x02\x03\x2e\xaf\
            \x00\x00\x9e\x00\x20\x80\x1b\x00\x09\x68\x65\x6c\x6c\x6f\x2e\x74\
            \x78\x74\x22\x02\x00\x2c\x89\x7a\xb5\x81\x6d\x5d\x13\x34\x59\x58\
            \x40\xd6\xce\x57\x29\x07\x6c\x0b\xe2\x47\xb2\x20\x2d\xba\xb1\xb8\
            \x95\x7b\x91\x9c\x52\x0a\x03\x02\x34\xa8\xe1\xeb\x05\xd6\xdc\x01\
            \xc6\xb0\x2c\x24\x04\x32\xfa\x33\xbe\x61\x5f\x80\x24\x1a\x5c\x02\
            \x56\xdd\x30\x66\x20\x84\x08\x10\x51\x77\xfb\x5c\x6f\xbb\x6f\xbb\
            \x3b\xc5\x02\xa2\x8b\x1d\x46\x75\xed\xf2\x58\x76\x25\xf0\x64\x46\
            \x1b\x49\x71\x43\x02\x03\x2e\x85\x00\x00\x89\x00\x20\xc0\x1b\x00\
            \x08\x74\x69\x6e\x79\x2e\x74\x78\x74\x22\x02\x00\x92\x0c\x14\x84\
            \xc2\x19\x7c\x1f\xe8\x54\x9a\xef\x42\x89\xb9\x93\xc8\x8f\x5d\x01\
            \x33\xbd\x20\xd4\xd6\x59\x5d\xef\x26\xd3\x38\xb6\x0a\x03\x02\xd0\
            \xb1\xe1\xeb\x05\xd6\xdc\x01\x45\x1d\x02\x4f\x88\x1d\x77\x56\x51\
            \x03\x05\x04\x00";
        let temp = workspace_tempdir();
        let input = temp.path().join("solid.rar");
        fs::write(&input, SOLID_RAR)?;
        let scratch = temp.path().join("scratch");
        fs::create_dir(&scratch)?;

        let archive = unpack(&input, &scratch)?;
        assert_eq!(
            archive.files,
            [Path::new("hello.txt"), Path::new("tiny.txt")]
        );
        assert_eq!(
            fs::read(archive.root().join("hello.txt"))?,
            b"Hello, RAR 5.0 fixture world.\n"
        );
        assert_eq!(fs::read(archive.root().join("tiny.txt"))?, b"AAAAAAAA\n");
        drop(archive);

        assert!(unpack_with_limits(&input, &scratch, 10, 100, 1).is_err());
        assert_eq!(fs::read_dir(&scratch)?.count(), 0);

        let mut damaged = SOLID_RAR.to_vec();
        damaged[0xd9] ^= 0x80;
        fs::write(&input, damaged)?;
        assert!(unpack(&input, &scratch).is_err());
        assert_eq!(fs::read_dir(&scratch)?.count(), 0);
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
