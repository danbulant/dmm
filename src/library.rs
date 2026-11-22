use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tempfile::{Builder, TempDir};

use crate::archive::{ArchiveTree, portable_path, unpack};
use crate::deploy::{
    Deployment, FileChange, FileState, checked_root, clear_empty_folder, fingerprint, metadata,
    store_blob, target, verify_folder,
};
use crate::games::{GameKind, GameProfile, InstallPlan, parse_profile, plan, profile};
use crate::sources::{RemoteMod, SearchQuery, Source};

pub struct Library {
    db: Connection,
    _lock: File,
    state: PathBuf,
    blobs: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instance {
    pub name: String,
    pub kind: GameKind,
    pub root: PathBuf,
    pub profile: Option<PathBuf>,
}

impl Instance {
    pub fn profile(&self) -> Result<GameProfile> {
        if let Some(path) = &self.profile {
            parse_profile(self.kind, &fs::read_to_string(path)?)
        } else {
            profile(self.kind)
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct LaunchSettings {
    pub xxmi: Option<PathBuf>,
    pub wine_prefix: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub id: RemoteMod,
    pub source_url: Option<String>,
    pub file_id: u64,
    pub filename: String,
    pub version: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version {
    pub release: Option<Release>,
    pub archive_hash: Option<String>,
    pub destination: Option<PathBuf>,
    files: BTreeMap<String, FileState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManagedMod {
    pub slug: String,
    pub enabled: bool,
    pub group: Option<String>,
    pub current: usize,
    pub versions: Vec<Version>,
    backups: BTreeMap<String, FileState>,
}

impl ManagedMod {
    pub fn release(&self) -> Option<&Release> {
        self.versions[self.current].release.as_ref()
    }

    pub fn files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.versions[self.current]
            .files
            .iter()
            .map(|(path, state)| (path.as_str(), state.hash.as_str()))
    }
}

#[derive(Default)]
pub struct InstallOptions<'a> {
    pub destination: Option<&'a Path>,
    pub keep_disabled: bool,
    pub replace_unmanaged: bool,
    pub group: Option<&'a str>,
    pub allow_multiple: bool,
}

#[derive(Debug, Serialize)]
pub struct Conflict {
    pub path: String,
    pub owner: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Inspection {
    pub plan: InstallPlan,
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SavedSearch {
    pub source: Source,
    pub game: String,
    pub query: SearchQuery,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingDownload {
    pub instance: String,
    pub slug: String,
    pub release: Release,
    pub destination: Option<PathBuf>,
    pub keep_disabled: bool,
    pub replace_unmanaged: bool,
    pub group: Option<String>,
    pub allow_multiple: bool,
}

impl PendingDownload {
    fn key(id: &RemoteMod, file_id: u64) -> Result<String> {
        Ok(serde_json::to_string(&(id, file_id))?)
    }
}

#[derive(Serialize, Deserialize)]
struct Pending {
    root: PathBuf,
    deployment: Deployment,
}

impl Library {
    pub fn open(state: &Path) -> Result<Self> {
        fs::create_dir_all(state)?;
        let state = state.canonicalize()?;
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(state.join("lock"))?;
        lock.try_lock()
            .context("DMM state is in use by another process")?;
        let db = Connection::open(state.join("library.sqlite3"))?;
        db.execute_batch(include_str!("schema.sql"))?;
        let blobs = state.join("blobs");
        fs::create_dir_all(&blobs)?;
        let library = Self {
            db,
            _lock: lock,
            state,
            blobs,
        };
        library.recover()?;
        Ok(library)
    }

    pub fn scratch(&self) -> Result<TempDir> {
        Ok(Builder::new().prefix("scratch-").tempdir_in(&self.state)?)
    }

    pub fn register(
        &self,
        name: &str,
        kind: GameKind,
        root: &Path,
        descriptor: Option<&Path>,
    ) -> Result<Instance> {
        validate_slug(name)?;
        let root = root
            .canonicalize()
            .with_context(|| format!("locating {}", root.display()))?;
        checked_root(&root)?;
        ensure!(
            !trees_overlap(&self.state, &root),
            "library and game root must be separate directories"
        );
        for instance in self.instances()? {
            ensure!(
                !trees_overlap(&instance.root, &root),
                "game root overlaps instance {}",
                instance.name
            );
        }
        let instance = Instance {
            name: name.to_owned(),
            kind,
            root,
            profile: descriptor.map(Path::canonicalize).transpose()?,
        };
        instance.profile()?;
        self.db.execute(
            "INSERT INTO instances VALUES (?1, ?2)",
            params![name, serde_json::to_string(&instance)?],
        )?;
        Ok(instance)
    }

    pub fn instances(&self) -> Result<Vec<Instance>> {
        self.json_rows("SELECT data FROM instances ORDER BY name", [])
    }

    pub fn active_installation(&self, kind: GameKind) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT instance FROM active_installations WHERE kind = ?1",
                [kind.to_string()],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn use_installation(&self, name: &str) -> Result<()> {
        let instance = self.instance(name)?;
        instance.profile()?;
        checked_root(&instance.root)?;
        self.db.execute(
            "INSERT INTO active_installations VALUES (?1, ?2)
             ON CONFLICT(kind) DO UPDATE SET instance=excluded.instance",
            params![instance.kind.to_string(), name],
        )?;
        Ok(())
    }

    pub fn launch_settings(&self, name: &str) -> Result<LaunchSettings> {
        let data: Option<String> = self
            .db
            .query_row(
                "SELECT data FROM launch_settings WHERE instance = ?1",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        Ok(data
            .map(|value| serde_json::from_str(&value))
            .transpose()?
            .unwrap_or_default())
    }

    pub fn set_launch_settings(&self, name: &str, settings: &LaunchSettings) -> Result<()> {
        self.db.execute(
            "INSERT INTO launch_settings VALUES (?1, ?2)
             ON CONFLICT(instance) DO UPDATE SET data=excluded.data",
            params![name, serde_json::to_string(settings)?],
        )?;
        Ok(())
    }

    pub fn instance(&self, name: &str) -> Result<Instance> {
        self.db
            .query_row("SELECT data FROM instances WHERE name = ?1", [name], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .map(|row| serde_json::from_str(&row))
            .transpose()?
            .with_context(|| format!("unknown instance {name}"))
    }

    pub fn mods(&self, instance: &str) -> Result<Vec<ManagedMod>> {
        self.instance(instance)?;
        self.json_rows(
            "SELECT data FROM mods WHERE instance = ?1 ORDER BY slug",
            [instance],
        )
    }

    pub fn managed_mod(&self, instance: &str, slug: &str) -> Result<ManagedMod> {
        self.lookup(instance, slug)?
            .with_context(|| format!("mod {slug} is not installed in {instance}"))
    }

    pub fn inspect(
        &self,
        instance: &str,
        slug: &str,
        input: &Path,
        destination: Option<&Path>,
    ) -> Result<Inspection> {
        validate_slug(slug)?;
        let instance = self.instance(instance)?;
        checked_root(&instance.root)?;
        let scratch = self.scratch()?;
        let tree = unpack(input, scratch.path())?;
        let plan = plan(&instance.profile()?, &tree.files, slug, destination)?;
        let mods = self.mods(&instance.name)?;
        let existing = mods.iter().find(|record| record.slug == slug);
        let claims = Claims::from_mods(&mods, slug);
        let mut conflicts = Vec::new();
        for mapping in &plan.files {
            let path = portable_path(&mapping.destination)?;
            if let Some(owner) = claims.conflict(&path) {
                conflicts.push(Conflict {
                    path,
                    owner: Some(owner.to_owned()),
                });
            } else if !existing
                .is_some_and(|record| record.versions[record.current].files.contains_key(&path))
                && metadata(&target(&instance.root, &path, false)?)?.is_some()
            {
                conflicts.push(Conflict { path, owner: None });
            }
        }
        Ok(Inspection { plan, conflicts })
    }

    pub fn install(
        &mut self,
        instance: &str,
        slug: &str,
        input: &Path,
        release: Option<Release>,
        options: InstallOptions<'_>,
    ) -> Result<()> {
        validate_slug(slug)?;
        ensure!(
            !slug.starts_with("disabled"),
            "mod name cannot start with the ZZMI disabled prefix"
        );
        let instance = self.instance(instance)?;
        checked_root(&instance.root)?;
        let old = self.lookup(&instance.name, slug)?;
        if let Some(old) = &old {
            ensure!(
                old.release().map(|r| &r.id) == release.as_ref().map(|r| &r.id),
                "existing mod has a different source; use another name or remove it first"
            );
            ensure!(
                !options.keep_disabled || !old.enabled || instance.kind != GameKind::Zzmi,
                "disable the ZZMI mod before updating it as disabled"
            );
        }
        let scratch = self.scratch()?;
        let tree = unpack(input, scratch.path())?;
        let destination = options.destination.or_else(|| {
            old.as_ref()
                .and_then(|m| m.versions[m.current].destination.as_deref())
        });
        let plan = plan(&instance.profile()?, &tree.files, slug, destination)?;
        let mut next = old.clone().unwrap_or_else(|| ManagedMod {
            slug: slug.to_owned(),
            enabled: !options.keep_disabled,
            group: None,
            current: 0,
            versions: Vec::new(),
            backups: BTreeMap::new(),
        });
        if options.keep_disabled {
            next.enabled = false;
        }
        if let Some(group) = options.group {
            validate_slug(group)?;
            next.group = Some(group.to_owned());
        }
        let version = self.stage_version(&tree, input, plan, release, destination)?;
        next.current = next.versions.len();
        next.versions.push(version);
        let deployment = self.prepare(
            &instance,
            old.as_ref(),
            Some(&mut next),
            options.replace_unmanaged,
            options.allow_multiple,
        )?;
        self.commit_change(&instance, slug, Some(&next), deployment)
    }

    pub fn toggle(
        &mut self,
        instance: &str,
        slug: &str,
        enabled: bool,
        replace_unmanaged: bool,
        allow_multiple: bool,
    ) -> Result<()> {
        let instance = self.instance(instance)?;
        let old = self.managed_mod(&instance.name, slug)?;
        if enabled == old.enabled {
            return Ok(());
        }
        let mut next = old.clone();
        next.enabled = enabled;
        let deployment = if instance.kind == GameKind::Zzmi {
            checked_root(&instance.root)?;
            self.check_claims(&instance, &next, allow_multiple)?;
            let before = folder(slug, old.enabled);
            let after = folder(slug, enabled);
            let files = folder_files(&old)?;
            verify_folder(&target(&instance.root, &before, false)?, &files)?;
            clear_empty_folder(&target(&instance.root, &after, false)?)?;
            Deployment::RenameFolder {
                before,
                after,
                files,
            }
        } else {
            self.prepare(
                &instance,
                Some(&old),
                Some(&mut next),
                replace_unmanaged,
                allow_multiple,
            )?
        };
        self.commit_change(&instance, slug, Some(&next), deployment)
    }

    pub fn rollback(
        &mut self,
        instance: &str,
        slug: &str,
        version: usize,
        replace_unmanaged: bool,
    ) -> Result<()> {
        let instance = self.instance(instance)?;
        let old = self.managed_mod(&instance.name, slug)?;
        let mut next = old.clone();
        ensure!(version < next.versions.len(), "version index out of range");
        if version == next.current {
            return Ok(());
        }
        next.current = version;
        let deployment = self.prepare(
            &instance,
            Some(&old),
            Some(&mut next),
            replace_unmanaged,
            true,
        )?;
        self.commit_change(&instance, slug, Some(&next), deployment)
    }

    pub fn remove(&mut self, instance: &str, slug: &str) -> Result<()> {
        let instance = self.instance(instance)?;
        let old = self.managed_mod(&instance.name, slug)?;
        let deployment = self.prepare(&instance, Some(&old), None, false, true)?;
        self.commit_change(&instance, slug, None, deployment)
    }

    pub fn favorite(&self, item: &RemoteMod, save: bool) -> Result<()> {
        let key = serde_json::to_string(item)?;
        if save {
            self.db
                .execute("INSERT OR IGNORE INTO favorites VALUES (?1)", [&key])?;
        } else {
            self.db
                .execute("DELETE FROM favorites WHERE data = ?1", [&key])?;
        }
        Ok(())
    }

    pub fn favorites(&self) -> Result<Vec<RemoteMod>> {
        self.json_rows("SELECT data FROM favorites ORDER BY data", [])
    }

    pub fn save_search(&self, name: &str, search: &SavedSearch) -> Result<()> {
        validate_slug(name)?;
        self.db.execute("INSERT INTO searches VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET data=excluded.data",
            params![name, serde_json::to_string(search)?])?;
        Ok(())
    }

    pub fn saved_search(&self, name: &str) -> Result<SavedSearch> {
        self.db
            .query_row("SELECT data FROM searches WHERE name = ?1", [name], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .map(|row| serde_json::from_str(&row))
            .transpose()?
            .with_context(|| format!("unknown saved search {name}"))
    }

    pub fn await_download(&self, request: &PendingDownload) -> Result<()> {
        validate_slug(&request.slug)?;
        if let Some(group) = &request.group {
            validate_slug(group)?;
        }
        self.instance(&request.instance)?;
        let key = PendingDownload::key(&request.release.id, request.release.file_id)?;
        self.db.execute("INSERT INTO downloads VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET data=excluded.data",
            params![key, serde_json::to_string(request)?])?;
        Ok(())
    }

    pub fn pending_download(&self, id: &RemoteMod, file_id: u64) -> Result<PendingDownload> {
        let key = PendingDownload::key(id, file_id)?;
        self.db
            .query_row("SELECT data FROM downloads WHERE key = ?1", [&key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .map(|row| serde_json::from_str(&row))
            .transpose()?
            .context("no matching installation request; start with install-remote")
    }

    pub fn finish_download(&self, id: &RemoteMod, file_id: u64) -> Result<()> {
        self.db.execute(
            "DELETE FROM downloads WHERE key = ?1",
            [PendingDownload::key(id, file_id)?],
        )?;
        Ok(())
    }

    fn stage_version(
        &self,
        tree: &ArchiveTree,
        input: &Path,
        install: InstallPlan,
        release: Option<Release>,
        destination: Option<&Path>,
    ) -> Result<Version> {
        let mut files = BTreeMap::new();
        let mut claims = Claims::default();
        for mapping in install.files {
            let path = portable_path(&mapping.destination)?;
            ensure!(
                claims.conflict(&path).is_none(),
                "duplicate or overlapping destination: {path}"
            );
            claims.insert(&path, "installation plan");
            let state = store_blob(&tree.root().join(&mapping.source), &self.blobs)?;
            files.insert(path, state);
        }
        ensure!(!files.is_empty(), "installation plan is empty");
        let archive_hash = if input.is_file() {
            Some(store_blob(input, &self.blobs)?.hash)
        } else {
            None
        };
        Ok(Version {
            release,
            archive_hash,
            destination: destination.map(Path::to_path_buf),
            files,
        })
    }

    fn check_claims(
        &self,
        instance: &Instance,
        next: &ManagedMod,
        allow_multiple: bool,
    ) -> Result<()> {
        let mods = self.mods(&instance.name)?;
        if next.enabled
            && !allow_multiple
            && let Some(group) = &next.group
        {
            if let Some(other) = mods.iter().find(|other| {
                other.enabled && other.slug != next.slug && other.group.as_ref() == Some(group)
            }) {
                bail!(
                    "{} is active in group {group}; disable it or pass --allow-multiple",
                    other.slug
                );
            }
        }
        let claims = Claims::from_mods(&mods, &next.slug);
        for path in next.versions[next.current].files.keys() {
            if let Some(owner) = claims.conflict(path) {
                bail!("{path} conflicts with {owner}; remove it or replan to another destination");
            }
        }
        Ok(())
    }

    fn prepare(
        &self,
        instance: &Instance,
        before: Option<&ManagedMod>,
        mut after: Option<&mut ManagedMod>,
        replace_unmanaged: bool,
        allow_multiple: bool,
    ) -> Result<Deployment> {
        checked_root(&instance.root)?;
        if let Some(next) = after.as_deref() {
            let already_active = before.is_some_and(|old| old.enabled && old.group == next.group);
            self.check_claims(instance, next, allow_multiple || already_active)?;
        }
        let mut backups = after
            .as_deref_mut()
            .map(|next| std::mem::take(&mut next.backups));
        let zzmi = instance.kind == GameKind::Zzmi;
        let old_files = deployed(before, zzmi)?;
        let new_files = deployed(after.as_deref(), zzmi)?;
        if zzmi {
            let record = before.or(after.as_deref()).context("no mod to deploy")?;
            for enabled in [false, true] {
                let name = folder(&record.slug, enabled);
                let expected = if let Some(old) = before.filter(|old| old.enabled == enabled) {
                    folder_files(old)?
                } else {
                    Vec::new()
                };
                verify_folder(&target(&instance.root, &name, false)?, &expected)?;
            }
        }
        let mut changes = Vec::new();
        let keys: BTreeSet<_> = old_files.keys().chain(new_files.keys()).collect();
        for relative in keys {
            let path = target(&instance.root, relative, false)?;
            let deployed_before = old_files.get(relative).copied();
            let deployed_after = new_files.get(relative).copied();
            let existing = fingerprint(&path)?;
            let mut baseline = before.and_then(|old| old.backups.get(relative)).cloned();
            if let Some(expected) = deployed_before {
                ensure!(
                    existing.as_ref() == Some(expected),
                    "file changed outside DMM: {}",
                    path.display()
                );
            } else if existing != baseline {
                if existing.is_some() {
                    ensure!(
                        replace_unmanaged && !zzmi,
                        "unmanaged file {}; pass --replace-unmanaged to back it up and replace it",
                        path.display()
                    );
                    baseline = Some(store_blob(&path, &self.blobs)?);
                    ensure!(
                        baseline == existing,
                        "file changed during backup: {}",
                        path.display()
                    );
                } else {
                    baseline = None;
                }
            }
            let replacement = deployed_after.cloned().or_else(|| baseline.clone());
            if let Some(backups) = &mut backups {
                if let Some(base) = baseline {
                    backups.insert(relative.clone(), base);
                } else {
                    backups.remove(relative);
                }
            }
            if existing != replacement {
                changes.push(FileChange {
                    path: relative.clone(),
                    before: existing,
                    after: replacement,
                });
            }
        }
        if let (Some(next), Some(mut backups)) = (after, backups) {
            let files = &next.versions[next.current].files;
            backups.retain(|path, _| files.contains_key(path));
            next.backups = backups;
        }
        Ok(Deployment::Files(changes))
    }

    fn commit_change(
        &mut self,
        instance: &Instance,
        slug: &str,
        after: Option<&ManagedMod>,
        deployment: Deployment,
    ) -> Result<()> {
        let pending = Pending {
            root: instance.root.clone(),
            deployment,
        };
        let row = after.map(serde_json::to_string).transpose()?;
        self.db.execute(
            "INSERT INTO pending VALUES (1, ?1)",
            [serde_json::to_string(&pending)?],
        )?;
        let result = pending.deployment.apply(&pending.root, &self.blobs, false).and_then(|()| {
            let tx = self.db.transaction()?;
            if let Some(data) = row {
                tx.execute("INSERT INTO mods VALUES (?1, ?2, ?3) ON CONFLICT(instance, slug) DO UPDATE SET data=excluded.data",
                    params![instance.name, slug, data])?;
            } else {
                tx.execute("DELETE FROM mods WHERE instance = ?1 AND slug = ?2", params![instance.name, slug])?;
            }
            tx.execute("DELETE FROM pending WHERE id = 1", [])?;
            tx.commit()?;
            Ok(())
        });
        if let Err(err) = result {
            self.recover()
                .with_context(|| format!("operation failed ({err:#}); could not roll back"))?;
            return Err(err);
        }
        Ok(())
    }

    fn recover(&self) -> Result<()> {
        let pending: Option<String> = self
            .db
            .query_row("SELECT data FROM pending WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        if let Some(pending) = pending {
            let pending: Pending = serde_json::from_str(&pending)?;
            pending
                .deployment
                .apply(&pending.root, &self.blobs, true)
                .context(
                    "recovering interrupted deployment; restore the reported path before retrying",
                )?;
            self.db.execute("DELETE FROM pending WHERE id = 1", [])?;
        }
        Ok(())
    }

    fn lookup(&self, instance: &str, slug: &str) -> Result<Option<ManagedMod>> {
        Ok(self
            .db
            .query_row(
                "SELECT data FROM mods WHERE instance = ?1 AND slug = ?2",
                params![instance, slug],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|row| serde_json::from_str(&row))
            .transpose()?)
    }

    fn json_rows<T: serde::de::DeserializeOwned, P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
    ) -> Result<Vec<T>> {
        let mut stmt = self.db.prepare(sql)?;
        let rows = stmt.query_map(params, |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
}

fn trees_overlap(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        let left = left.to_string_lossy().to_lowercase();
        let right = right.to_string_lossy().to_lowercase();
        let (left, right) = (Path::new(&left), Path::new(&right));
        left.starts_with(right) || right.starts_with(left)
    } else {
        left.starts_with(right) || right.starts_with(left)
    }
}

fn validate_slug(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 80
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
        "use a lowercase name of up to 80 letters, digits, hyphens or underscores"
    );
    portable_path(Path::new(name))?;
    Ok(())
}

#[derive(Default)]
struct Claims<'a> {
    paths: BTreeMap<String, Claim<'a>>,
}

enum Claim<'a> {
    Directory { spelling: String, owner: &'a str },
    File(&'a str),
}

impl<'a> Claims<'a> {
    fn from_mods(mods: &'a [ManagedMod], except: &str) -> Self {
        let mut claims = Self::default();
        for record in mods.iter().filter(|record| record.slug != except) {
            for path in record.versions[record.current].files.keys() {
                claims.insert(path, &record.slug);
            }
        }
        claims
    }

    fn insert(&mut self, path: &str, owner: &'a str) {
        for directory in ancestors(path) {
            self.paths
                .entry(directory.to_lowercase())
                .or_insert_with(|| Claim::Directory {
                    spelling: directory.to_owned(),
                    owner,
                });
        }
        self.paths.insert(path.to_lowercase(), Claim::File(owner));
    }

    fn conflict(&self, path: &str) -> Option<&'a str> {
        for directory in ancestors(path) {
            match self.paths.get(&directory.to_lowercase()) {
                Some(Claim::File(owner)) => return Some(owner),
                Some(Claim::Directory { spelling, owner }) if spelling != directory => {
                    return Some(owner);
                }
                _ => {}
            }
        }
        self.paths
            .get(&path.to_lowercase())
            .map(|claim| match claim {
                Claim::Directory { owner, .. } | Claim::File(owner) => *owner,
            })
    }
}

fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/')
        .map(move |(index, _)| &path[..index])
}

fn folder(slug: &str, enabled: bool) -> String {
    if enabled {
        slug.to_owned()
    } else {
        format!("DISABLED_{slug}")
    }
}

fn folder_files(record: &ManagedMod) -> Result<Vec<(String, FileState)>> {
    let prefix = format!("{}/", record.slug);
    record.versions[record.current]
        .files
        .iter()
        .map(|(name, state)| {
            Ok((
                name.strip_prefix(&prefix)
                    .context("invalid ZZMI manifest")?
                    .to_owned(),
                state.clone(),
            ))
        })
        .collect()
}

fn deployed(record: Option<&ManagedMod>, zzmi: bool) -> Result<BTreeMap<String, &FileState>> {
    let Some(record) = record.filter(|record| record.enabled || zzmi) else {
        return Ok(BTreeMap::new());
    };
    record.versions[record.current]
        .files
        .iter()
        .map(|(path, state)| {
            let path = if record.enabled {
                path.clone()
            } else {
                format!("DISABLED_{path}")
            };
            Ok((path, state))
        })
        .collect()
}

#[cfg(test)]
mod tests;
