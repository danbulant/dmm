use super::*;
use crate::acquisition::pending_pages;

#[test]
fn bookmark_migration_drops_only_unnamed_legacy_rows() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = Connection::open(dir.path().join("library.sqlite3"))?;
    db.execute_batch(
        "CREATE TABLE favorites (data TEXT PRIMARY KEY NOT NULL) STRICT;
         CREATE TABLE favorite_metadata (
             id TEXT PRIMARY KEY NOT NULL REFERENCES favorites(data) ON DELETE CASCADE,
             name TEXT,
             image TEXT
         ) STRICT;",
    )?;
    let unnamed = RemoteMod {
        source: Source::GameBanana,
        game: "game".to_owned(),
        id: 42,
    };
    let named = RemoteMod {
        source: Source::Nexus,
        game: "game".to_owned(),
        id: 84,
    };
    for item in [&unnamed, &named] {
        db.execute(
            "INSERT INTO favorites VALUES (?1)",
            [serde_json::to_string(item)?],
        )?;
    }
    db.execute(
        "INSERT INTO favorite_metadata VALUES (?1, ?2, ?3)",
        params![
            serde_json::to_string(&named)?,
            "Named mod",
            "https://images.example/named.png"
        ],
    )?;
    drop(db);

    let lib = Library::open(dir.path())?;
    assert_eq!(
        lib.saved_favorites()?,
        [SavedFavorite {
            item: named.clone(),
            name: "Named mod".to_owned(),
            image: Some("https://images.example/named.png".to_owned()),
        }]
    );
    lib.unsave(&named)?;
    assert!(lib.saved_favorites()?.is_empty());
    assert!(lib.save_favorite(&unnamed, "", None).is_err());
    Ok(())
}

#[test]
fn id_only_database_opens_with_no_bookmarks() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = Connection::open(dir.path().join("library.sqlite3"))?;
    db.execute_batch("CREATE TABLE favorites (data TEXT PRIMARY KEY NOT NULL) STRICT")?;
    db.execute(
        "INSERT INTO favorites VALUES (?1)",
        [r#"{"source":"nexus","game":"game","id":1}"#],
    )?;
    drop(db);
    assert!(Library::open(dir.path())?.saved_favorites()?.is_empty());
    Ok(())
}

#[test]
fn reopening_rolls_back_a_partially_applied_deployment() -> Result<()> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    fs::create_dir_all(&base)?;
    let dir = tempfile::tempdir_in(base)?;
    let root = dir.path().join("game");
    let state = dir.path().join("state");
    fs::create_dir(&root)?;
    let lib = Library::open(&state)?;
    let instance = lib.register("game", GameKind::Generic, &root, None)?;
    let path = instance.root.join("first");
    let payload = dir.path().join("payload");
    fs::write(&path, b"unmanaged original")?;
    fs::write(&payload, b"new mod data")?;
    let original = store_blob(&path, &lib.blobs)?;
    let installed = store_blob(&payload, &lib.blobs)?;
    let first = FileChange {
        path: "first".to_owned(),
        before: Some(original),
        after: Some(installed.clone()),
    };
    let second = FileChange {
        path: "second".to_owned(),
        before: None,
        after: Some(installed),
    };
    let pending = Pending {
        root: instance.root,
        deployment: Deployment::Files(vec![first, second]),
    };
    lib.db.execute(
        "INSERT INTO pending VALUES (1, ?1)",
        [serde_json::to_string(&pending)?],
    )?;
    fs::copy(&payload, &path)?;
    drop(lib);

    assert_eq!(fs::read(&path)?, b"new mod data");
    let lib = Library::open(&state)?;
    assert_eq!(fs::read(path)?, b"unmanaged original");
    assert!(!root.join("second").exists());
    assert!(lib.mods("game")?.is_empty());
    Ok(())
}

#[test]
fn remote_bundle_plans_each_archive_and_records_offline_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let root = temp.path().join("game");
    fs::create_dir_all(&root)?;
    let base = temp.path().join("base.tar");
    let patch = temp.path().join("patch.tar");
    tar_fixture(&base, "wrapper/archive/pc/mod/base.archive", b"base")?;
    tar_fixture(&patch, "different-wrapper/patch.yaml", b"patch")?;

    let mut library = Library::open(&state)?;
    library.register("game", GameKind::Cyberpunk, &root, None)?;
    let id = RemoteMod {
        source: Source::Nexus,
        game: "cyberpunk2077".to_owned(),
        id: 7,
    };
    let release = |file_id, filename: &str| Release {
        id: id.clone(),
        source_url: None,
        file_id,
        filename: filename.to_owned(),
        version: Some("1".to_owned()),
        updated_at: None,
    };
    let selected = vec![
        (base.clone(), release(10, "base.tar")),
        (patch.clone(), release(11, "patch.tar")),
    ];
    let details: ModDetails = serde_json::from_str(
        r#"{"id":{"source":"nexus","game":"cyberpunk2077","id":7},"name":"Bundle","author":null,"category":null,"summary":null,"description":"Description while offline","version":null,"updated_at":null,"created_at":null,"url":null,"tags":["Textures"],"images":[],"requirements":[],"files":[{"id":10,"name":"base.tar","primary":true,"url":"https://example.com/file?token=secret"}]}"#,
    )?;

    library.install_remote_bundle(
        "game",
        "bundle",
        &selected,
        &details,
        InstallOptions::default(),
    )?;
    let downloaded_at = library
        .managed_mod("game", "bundle")?
        .downloaded_at_ms()
        .context("remote bundle is missing its download date")?;

    assert_eq!(fs::read(root.join("archive/pc/mod/base.archive"))?, b"base");
    assert_eq!(fs::read(root.join("r6/tweaks/patch.yaml"))?, b"patch");
    let conflicting = temp.path().join("conflicting.tar");
    tar_fixture(&conflicting, "base.archive", b"conflict")?;
    assert!(
        library
            .install_remote_bundle(
                "game",
                "bundle",
                &[
                    (base.clone(), release(12, "base-update.tar")),
                    (conflicting, release(13, "conflicting-patch.tar")),
                ],
                &details,
                InstallOptions::default(),
            )
            .is_err()
    );
    assert_eq!(fs::read(root.join("archive/pc/mod/base.archive"))?, b"base");
    assert_eq!(fs::read(root.join("r6/tweaks/patch.yaml"))?, b"patch");
    drop(library);
    let library = Library::open(&state)?;
    assert_eq!(library.remote_slug("game", &id)?.as_deref(), Some("bundle"));
    let installed = library.managed_mod("game", "bundle")?;
    assert_eq!(installed.versions.len(), 1);
    assert_eq!(installed.downloaded_at_ms(), Some(downloaded_at));
    assert_eq!(
        installed
            .releases()
            .iter()
            .map(|release| release.file_id)
            .collect::<Vec<_>>(),
        [10, 11]
    );
    let stored = installed.details().context("missing offline mod details")?;
    assert_eq!(
        stored.description.as_deref(),
        Some("Description while offline")
    );
    assert_eq!(stored.tags, ["Textures"]);
    assert!(stored.files[0].url.is_none());
    assert_eq!(
        installed.versions[installed.current].archive_hashes.len(),
        2
    );

    assert_eq!(
        installed.versions[installed.current]
            .provenance
            .values()
            .map(|release| release.file_id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([10, 11])
    );
    let zzmi = profile(GameKind::Zzmi)?;
    let packages = [
        vec![PathBuf::from("main/base.ini")],
        vec![PathBuf::from("patch/textures.dds")],
    ];
    let packages = packages.iter().map(Vec::as_slice).collect::<Vec<_>>();
    assert!(plan_bundle(&zzmi, &packages, "bundle", None).is_ok());
    Ok(())
}

#[test]
fn nexus_bundle_retains_received_parts_across_restart_and_can_be_replaced() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let game = temp.path().join("game");
    let other = temp.path().join("other");
    fs::create_dir(&game)?;
    fs::create_dir(&other)?;
    let library = Library::open(&state)?;
    library.register("cp", GameKind::Cyberpunk, &game, None)?;
    library.register("other", GameKind::Cyberpunk, &other, None)?;
    let details: ModDetails = serde_json::from_str(
        r#"{"id":{"source":"nexus","game":"cyberpunk2077","id":9},"name":"Bundle","images":[],"requirements":[],"files":[]}"#,
    )?;
    let release = |file_id| Release {
        id: details.id.clone(),
        source_url: None,
        file_id,
        filename: format!("part-{file_id}.tar"),
        version: None,
        updated_at: None,
    };
    let bundle = [release(11), release(22)];
    let mut request = PendingDownload {
        instance: "cp".into(),
        slug: "bundle".into(),
        release: bundle[0].clone(),
        bundle: Vec::new(),
        details: None,
        received: BTreeMap::new(),
        destination: None,
        keep_disabled: true,
        replace_unmanaged: false,
        group: None,
        allow_multiple: false,
    };
    library.await_bundle(&request, &bundle, &details)?;
    assert_eq!(pending_pages(&library, "cp")?.len(), 2);
    let input = temp.path().join("part.tar");
    tar_fixture(&input, "base.archive", b"bundle data")?;
    let pending = library.record_pending_archive(&details.id, 11, &input)?;
    let stored = pending.received[&11].clone();
    drop(library);

    let library = Library::open(&state)?;
    library.await_bundle(&request, &bundle, &details)?;
    assert_eq!(fs::read(&stored)?, fs::read(input)?);
    let remaining = pending_pages(&library, "cp")?;
    assert_eq!(remaining.len(), 1);
    assert!(
        remaining[0]
            .query_pairs()
            .any(|(key, value)| key == "file_id" && value == "22")
    );
    assert!(library.mods("cp")?.is_empty());

    request.instance = "other".into();
    assert!(library.await_bundle(&request, &bundle, &details).is_err());
    request.instance = "cp".into();
    library.await_bundle(&request, &bundle[1..], &details)?;
    assert!(!stored.exists());
    assert!(library.pending_download(&details.id, 11).is_err());
    assert_eq!(pending_pages(&library, "cp")?, remaining);
    Ok(())
}

fn tar_fixture(path: &Path, name: &str, payload: &[u8]) -> Result<()> {
    let mut archive = vec![0; 512 + payload.len().div_ceil(512) * 512 + 1024];
    let header = &mut archive[..512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..108].copy_from_slice(b"0000644\0");
    header[124..136].copy_from_slice(format!("{:011o}\0", payload.len()).as_bytes());
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum = header.iter().map(|&byte| u32::from(byte)).sum::<u32>();
    header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
    archive[512..512 + payload.len()].copy_from_slice(payload);
    fs::write(path, archive)?;
    Ok(())
}
