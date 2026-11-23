use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::archive::portable_path;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct FileState {
    pub hash: String,
    #[cfg(unix)]
    mode: u32,
    #[cfg(not(unix))]
    readonly: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FileChange {
    pub path: String,
    pub before: Option<FileState>,
    pub after: Option<FileState>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Deployment {
    Files(Vec<FileChange>),
    RenameFolder {
        before: String,
        after: String,
        files: Vec<(String, FileState)>,
    },
}

impl Deployment {
    pub(crate) fn apply(&self, root: &Path, blobs: &Path, reverse: bool) -> Result<()> {
        checked_root(root)?;
        match self {
            Self::Files(changes) => {
                if reverse {
                    let mut first_error = None;
                    for change in changes.iter().rev() {
                        if let Err(err) = change.apply(root, blobs, true) {
                            first_error.get_or_insert(err);
                        }
                    }
                    if let Some(err) = first_error {
                        return Err(err);
                    }
                } else {
                    for change in changes {
                        change.apply(root, blobs, false)?;
                    }
                }
            }
            Self::RenameFolder {
                before,
                after,
                files,
            } => {
                let (from, to) = if reverse {
                    (after, before)
                } else {
                    (before, after)
                };
                let from = target(root, from, false)?;
                let to = target(root, to, false)?;
                match (metadata(&from)?, metadata(&to)?) {
                    (Some(_), None) => {
                        verify_folder(&from, files)?;
                        fs::rename(&from, &to).with_context(|| {
                            format!("renaming {} to {}", from.display(), to.display())
                        })?;
                    }
                    (None, Some(_)) => verify_folder(&to, files)?,
                    _ => bail!(
                        "expected exactly one of {} or {}",
                        from.display(),
                        to.display()
                    ),
                }
            }
        }
        Ok(())
    }
}

impl FileChange {
    fn apply(&self, root: &Path, blobs: &Path, reverse: bool) -> Result<()> {
        let (before, after) = if reverse {
            (&self.after, &self.before)
        } else {
            (&self.before, &self.after)
        };
        let path = target(root, &self.path, false)?;
        let current = fingerprint(&path)?;
        if &current == after {
            return Ok(());
        }
        ensure!(
            &current == before,
            "file changed outside DMM: {}",
            path.display()
        );
        if let Some(state) = after {
            let path = target(root, &self.path, true)?;
            let parent = path.parent().context("deployment path has no parent")?;
            let mut temp = NamedTempFile::new_in(parent)?;
            let mut input = File::open(blobs.join(&state.hash))
                .with_context(|| format!("missing library blob {}", state.hash))?;
            let hash = stream_hash(&mut input, &mut temp)?;
            ensure!(hash == state.hash, "corrupt library blob {}", state.hash);
            state.set_permissions(temp.as_file())?;
            temp.as_file().sync_all()?;
            if before.is_some() {
                ensure!(
                    &fingerprint(&path)? == before,
                    "file changed outside DMM: {}",
                    path.display()
                );
                temp.persist(&path)?;
            } else {
                temp.persist_noclobber(&path)?;
            }
        } else {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        Ok(())
    }
}

impl FileState {
    fn new(hash: String, meta: &Metadata) -> Self {
        Self {
            hash,
            #[cfg(unix)]
            mode: meta.permissions().mode() & 0o777,
            #[cfg(not(unix))]
            readonly: meta.permissions().readonly(),
        }
    }

    #[cfg(unix)]
    fn set_permissions(&self, file: &File) -> io::Result<()> {
        file.set_permissions(fs::Permissions::from_mode(self.mode))
    }

    #[cfg(not(unix))]
    fn set_permissions(&self, file: &File) -> io::Result<()> {
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(self.readonly);
        file.set_permissions(permissions)
    }
}

pub(crate) fn store_blob(path: &Path, blobs: &Path) -> Result<FileState> {
    let meta = regular_file(path)?.with_context(|| format!("missing file {}", path.display()))?;
    let mut input = File::open(path)?;
    let mut temp = NamedTempFile::new_in(blobs)?;
    let hash = stream_hash(&mut input, &mut temp)?;
    let blob = blobs.join(&hash);
    if !blob.is_file() {
        temp.as_file().sync_all()?;
        temp.persist_noclobber(blob)?;
    }
    Ok(FileState::new(hash, &meta))
}

pub(crate) fn fingerprint(path: &Path) -> Result<Option<FileState>> {
    regular_file(path)?
        .map(|meta| {
            let hash = stream_hash(&mut File::open(path)?, &mut io::sink())?;
            Ok(FileState::new(hash, &meta))
        })
        .transpose()
}

fn stream_hash(input: &mut impl Read, output: &mut impl Write) -> io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut chunk = [0; 64 * 1024];
    loop {
        let n = input.read(&mut chunk)?;
        if n == 0 {
            return Ok(hasher.finalize().to_hex().to_string());
        }
        hasher.update(&chunk[..n]);
        output.write_all(&chunk[..n])?;
    }
}

fn regular_file(path: &Path) -> Result<Option<Metadata>> {
    let meta = metadata(path)?;
    ensure!(
        meta.as_ref().is_none_or(Metadata::is_file),
        "not a regular file: {}",
        path.display()
    );
    Ok(meta)
}

pub(crate) fn metadata(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            #[cfg(windows)]
            {
                ensure!(
                    meta.file_attributes() & 0x400 == 0,
                    "reparse point at {}",
                    path.display()
                );
            }
            ensure!(!meta.is_symlink(), "symlink at {}", path.display());
            Ok(Some(meta))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("inspecting {}", path.display())),
    }
}

pub(crate) fn checked_root(root: &Path) -> Result<()> {
    ensure!(
        metadata(root)?.is_some_and(|meta| meta.is_dir()) && root.canonicalize()? == root,
        "game root is missing or no longer resolves to the configured directory: {}",
        root.display()
    );
    Ok(())
}

pub(crate) fn target(root: &Path, relative: &str, create_parents: bool) -> Result<PathBuf> {
    let relative = portable_path(Path::new(relative))?;
    let mut path = root.to_path_buf();
    let mut components = relative.split('/').peekable();
    while let Some(component) = components.next() {
        let candidate = path.join(component);
        let candidate_meta = metadata(&candidate)?;
        // On a case-sensitive filesystem, refuse an existing spelling that would collide on Windows.
        if candidate_meta.is_none() && path.is_dir() {
            let key = component.to_lowercase();
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                ensure!(
                    entry.file_name().to_string_lossy().to_lowercase() != key,
                    "case collision: {} and {}",
                    candidate.display(),
                    entry.path().display()
                );
            }
        }
        if components.peek().is_some() {
            if let Some(meta) = candidate_meta {
                ensure!(meta.is_dir(), "not a directory: {}", candidate.display());
            } else if create_parents {
                fs::create_dir(&candidate)?;
            }
        }
        path = candidate;
    }
    Ok(path)
}

pub(crate) fn verify_folder(root: &Path, files: &[(String, FileState)]) -> Result<()> {
    let expected: std::collections::HashMap<_, _> = files
        .iter()
        .map(|(path, state)| (path.as_str(), state))
        .collect();
    let mut found = 0;
    if metadata(root)?.is_some() {
        let mut dirs = vec![root.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            for entry in fs::read_dir(dir)? {
                let path = entry?.path();
                let meta = metadata(&path)?.context("mod folder changed during inspection")?;
                if meta.is_dir() {
                    dirs.push(path);
                } else {
                    let relative = portable_path(path.strip_prefix(root)?)?;
                    let state = expected.get(relative.as_str()).with_context(|| {
                        format!("unmanaged file in mod folder: {}", path.display())
                    })?;
                    ensure!(
                        fingerprint(&path)?.as_ref() == Some(*state),
                        "file changed outside DMM: {}",
                        path.display()
                    );
                    found += 1;
                }
            }
        }
    }
    ensure!(
        found == expected.len(),
        "managed files are missing from {}",
        root.display()
    );
    Ok(())
}

pub(crate) fn clear_empty_folder(root: &Path) -> Result<()> {
    if metadata(root)?.is_none() {
        return Ok(());
    }
    let mut dirs = vec![root.to_path_buf()];
    let mut next = 0;
    while next < dirs.len() {
        for entry in fs::read_dir(&dirs[next])? {
            let path = entry?.path();
            ensure!(
                metadata(&path)?.is_some_and(|meta| meta.is_dir()),
                "unmanaged file in XXMI destination: {}",
                path.display()
            );
            dirs.push(path);
        }
        next += 1;
    }
    for dir in dirs.iter().rev() {
        fs::remove_dir(dir)
            .with_context(|| format!("removing empty directory {}", dir.display()))?;
    }
    Ok(())
}
