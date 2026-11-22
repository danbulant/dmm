use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};

use super::{Library, ModDetails, PendingDownload, Release, RemoteMod, validate_slug};

impl Library {
    pub fn await_download(&self, request: &PendingDownload) -> Result<()> {
        validate_slug(&request.slug)?;
        if let Some(group) = &request.group {
            validate_slug(group)?;
        }
        self.instance(&request.instance)?;
        let key = PendingDownload::key(&request.release.id, request.release.file_id)?;
        self.db.execute(
            "INSERT INTO downloads VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET data=excluded.data",
            params![key, serde_json::to_string(request)?],
        )?;
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

    pub fn pending_downloads(&self, instance: &str) -> Result<Vec<PendingDownload>> {
        let mut pending: Vec<PendingDownload> = self.json_rows("SELECT data FROM downloads", [])?;
        pending.retain(|download| download.instance == instance);
        Ok(pending)
    }

    pub fn cancel_pending(&self, instance: &str, id: &RemoteMod) -> Result<()> {
        let pending = self.pending_downloads(instance)?;
        let matching: Vec<_> = pending
            .into_iter()
            .filter(|entry| entry.release.id == *id)
            .collect();
        let mut paths = BTreeSet::new();
        let tx = self.db.unchecked_transaction()?;
        for entry in matching {
            paths.extend(entry.received.into_values());
            tx.execute(
                "DELETE FROM downloads WHERE key = ?1",
                [PendingDownload::key(id, entry.release.file_id)?],
            )?;
        }
        tx.commit()?;
        let still_referenced: BTreeSet<PathBuf> = self
            .json_rows::<PendingDownload, _>("SELECT data FROM downloads", [])?
            .into_iter()
            .flat_map(|entry| entry.received.into_values())
            .collect();
        for path in paths.difference(&still_referenced) {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }

    pub fn finish_download(&self, id: &RemoteMod, file_id: u64) -> Result<()> {
        self.db.execute(
            "DELETE FROM downloads WHERE key = ?1",
            [PendingDownload::key(id, file_id)?],
        )?;
        Ok(())
    }

    pub fn await_bundle(
        &self,
        request: &PendingDownload,
        bundle: &[Release],
        details: &ModDetails,
    ) -> Result<()> {
        ensure!(!bundle.is_empty(), "download bundle cannot be empty");
        ensure!(
            bundle.iter().all(|release| release.id == details.id),
            "download bundle contains a different source identity"
        );
        self.instance(&request.instance)?;
        validate_slug(&request.slug)?;
        let mut selected_ids = BTreeSet::new();
        ensure!(
            bundle
                .iter()
                .all(|release| selected_ids.insert(release.file_id)),
            "download bundle contains duplicate file IDs"
        );
        let mut details = details.clone();
        for file in &mut details.files {
            file.url = None;
        }
        let tx = self.db.unchecked_transaction()?;
        let mut statement = tx.prepare("SELECT key, data FROM downloads")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut superseded = false;
        let mut obsolete = BTreeSet::new();
        let mut unchanged = false;
        for (key, data) in rows {
            let old: PendingDownload = serde_json::from_str(&data)?;
            if old.release.id != details.id {
                continue;
            }
            ensure!(
                old.instance == request.instance,
                "this provider mod is already awaiting download for {}",
                old.instance
            );
            if old.slug == request.slug
                && old.bundle == bundle
                && old.destination == request.destination
                && old.keep_disabled == request.keep_disabled
                && old.replace_unmanaged == request.replace_unmanaged
                && old.group == request.group
                && old.allow_multiple == request.allow_multiple
            {
                unchanged = true;
            } else {
                superseded = true;
                obsolete.extend(old.received.into_values());
                tx.execute("DELETE FROM downloads WHERE key = ?1", [key])?;
            }
        }
        if unchanged {
            ensure!(!superseded, "inconsistent pending download records");
            tx.commit()?;
            return Ok(());
        }
        for release in bundle {
            let pending = PendingDownload {
                release: release.clone(),
                bundle: bundle.to_vec(),
                details: Some(details.clone()),
                received: BTreeMap::new(),
                ..request.clone()
            };
            let key = PendingDownload::key(&release.id, release.file_id)?;
            tx.execute(
                "INSERT INTO downloads VALUES (?1, ?2)",
                params![key, serde_json::to_string(&pending)?],
            )?;
        }
        tx.commit()?;
        for path in obsolete {
            match fs::remove_file(path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error).context("cleaning superseded download parts");
                }
                Ok(()) | Err(_) => {}
            }
        }
        Ok(())
    }

    pub fn record_pending_archive(
        &self,
        id: &RemoteMod,
        file_id: u64,
        input: &Path,
    ) -> Result<PendingDownload> {
        let mut pending = self.pending_download(id, file_id)?;
        ensure!(
            pending
                .bundle
                .iter()
                .any(|release| release.file_id == file_id),
            "download is not part of the outstanding bundle"
        );
        if pending.received.contains_key(&file_id) {
            return Ok(pending);
        }
        let downloads = self.state.join("downloads");
        fs::create_dir_all(&downloads)?;
        let game = pending
            .release
            .id
            .game
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let stored = downloads.join(format!(
            "{}-{game}-{}-{file_id}.archive",
            pending.release.id.source, pending.release.id.id
        ));
        fs::copy(input, &stored)?;
        pending.received.insert(file_id, stored.clone());
        let tx = self.db.unchecked_transaction()?;
        let mut statement = tx.prepare("SELECT key, data FROM downloads")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for (key, data) in rows {
            let mut sibling: PendingDownload = serde_json::from_str(&data)?;
            if sibling.release.id == *id && sibling.bundle == pending.bundle {
                sibling.received.insert(file_id, stored.clone());
                tx.execute(
                    "UPDATE downloads SET data = ?2 WHERE key = ?1",
                    params![key, serde_json::to_string(&sibling)?],
                )?;
            }
        }
        tx.commit()?;
        Ok(pending)
    }

    pub fn finish_bundle(&self, id: &RemoteMod, bundle: &[Release]) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        let mut paths = BTreeSet::new();
        for release in bundle {
            let key = PendingDownload::key(id, release.file_id)?;
            if let Some(path) = tx
                .query_row("SELECT data FROM downloads WHERE key = ?1", [&key], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?
                .map(|data| serde_json::from_str::<PendingDownload>(&data))
                .transpose()?
                .and_then(|pending| pending.received.get(&release.file_id).cloned())
            {
                paths.insert(path);
            }
            tx.execute("DELETE FROM downloads WHERE key = ?1", [&key])?;
        }
        tx.commit()?;
        for path in paths {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}
