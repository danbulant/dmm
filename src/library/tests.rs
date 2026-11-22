use super::*;

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
