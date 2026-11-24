use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use dmm::games::{GameKind, Placement};
use dmm::library::{InstallOptions, Library, Release, SavedSearch};
use dmm::sources::{ModDetails, RemoteMod, SearchQuery, Sort, Source};
use tempfile::TempDir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

struct Fixture {
    dir: TempDir,
    root: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        fs::create_dir_all(&base)?;
        let dir = tempfile::tempdir_in(base)?;
        let root = dir.path().join("game");
        let state = dir.path().join("state");
        fs::create_dir(&root)?;
        Ok(Self { dir, root, state })
    }

    fn input(&self, name: &str, files: &[(&str, &str)]) -> Result<PathBuf> {
        let root = self.dir.path().join(name);
        for (path, content) in files {
            put(&root, path, content)?;
        }
        Ok(root)
    }

    fn library(&self, kind: GameKind) -> Result<Library> {
        let library = Library::open(&self.state)?;
        library.register("game", kind, &self.root)?;
        Ok(library)
    }
}

fn put(root: &Path, relative: &str, content: &str) -> Result<()> {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(path, content)?;
    Ok(())
}

#[test]
fn ff7_pak_archive_installs_and_toggles_without_touching_stock_paks() -> Result<()> {
    let f = Fixture::new()?;
    let stock = "End/Content/Paks/pakchunk0-WindowsNoEditor.pak";
    let deployed = f.root.join("End/Content/Paks/~mods/Cloud_P.PAK");
    let invalid = f.root.join("End");
    fs::create_dir(&invalid)?;
    let library = Library::open(&f.state)?;
    assert!(library.register("invalid", GameKind::Ff7Remake, &invalid).is_err());
    drop(library);
    put(&f.root, stock, "stock")?;
    let mut library = f.library(GameKind::Ff7Remake)?;
    let archive = f.dir.path().join("cloud.zip");
    let mut zip = ZipWriter::new(fs::File::create(&archive)?);
    for (name, contents) in [
        ("Release/end/content/paks/~Mods/Cloud_P.PAK", "cloud"),
        ("Release/end/content/paks/~Mods/Textures.pak", "textures"),
        ("Release/readme.txt", "instructions"),
    ] {
        zip.start_file(name, SimpleFileOptions::default())?;
        zip.write_all(contents.as_bytes())?;
    }
    zip.finish()?;
    let preview = library.inspect("game", "cloud", &archive, None)?;
    assert_eq!(preview.plan.placement, Placement::Layout);
    assert_eq!(preview.plan.files.len(), 2);
    assert_eq!(preview.plan.warnings.len(), 1);
    library.install("game", "cloud", &archive, None, InstallOptions::default())?;
    assert_eq!(fs::read_to_string(&deployed)?, "cloud");
    assert_eq!(
        fs::read_to_string(deployed.with_file_name("Textures.pak"))?,
        "textures"
    );
    assert!(!deployed.with_file_name("readme.txt").exists());
    library.toggle("game", "cloud", false, false, false)?;
    assert!(!deployed.exists());
    library.toggle("game", "cloud", true, false, false)?;
    assert_eq!(fs::read_to_string(&deployed)?, "cloud");
    library.remove("game", "cloud")?;
    assert!(!deployed.exists());
    assert_eq!(fs::read_to_string(f.root.join(stock))?, "stock");
    Ok(())
}

#[test]
fn ff7_pak_import_rejects_ambiguous_layouts_and_unsupported_payloads() -> Result<()> {
    let f = Fixture::new()?;
    fs::create_dir_all(f.root.join("End/Content/Paks"))?;
    let mut library = f.library(GameKind::Ff7Remake)?;
    let variants = f.input("variants", &[("A/Cloud.pak", "a"), ("B/CloudAlt.pak", "b")])?;
    let hook = f.input(
        "hook",
        &[
            ("Cloud.pak", "pak"),
            ("End/Binaries/Win64/xinput1_3.dll", "dll"),
        ],
    )?;
    let companion = f.input("companion", &[("Cloud.pak", "pak"), ("Cloud.utoc", "toc")])?;
    for (input, reason) in [
        (variants, "multiple folders"),
        (hook, "xinput1_3.dll"),
        (companion, "Cloud.utoc"),
    ] {
        let err = library
            .install("game", "cloud", &input, None, InstallOptions::default())
            .unwrap_err();
        assert!(err.to_string().contains(reason), "{err:#}");
    }
    assert!(library.mods("game")?.is_empty());
    assert!(!f.root.join("End/Content/Paks/~mods").exists());
    Ok(())
}

#[test]
fn ff7_loose_paks_use_local_and_provider_filenames() -> Result<()> {
    let f = Fixture::new()?;
    let mods = f.root.join("End/Content/Paks/~mods");
    fs::create_dir_all(mods.parent().unwrap())?;
    let mut library = f.library(GameKind::Ff7Remake)?;
    let input = f.dir.path().join("Local_P.pak");
    fs::write(&input, "local")?;
    library.install("game", "local", &input, None, InstallOptions::default())?;
    assert_eq!(fs::read_to_string(mods.join("Local_P.pak"))?, "local");

    let details: ModDetails = serde_json::from_str(
        r#"{"id":{"source":"nexus","game":"finalfantasy7remake","id":1},"name":"Cloud","images":[],"requirements":[],"files":[]}"#,
    )?;
    let download = f.dir.path().join("download");
    fs::write(&download, "remote")?;
    let mut release = Release {
        id: details.id.clone(),
        source_url: None,
        file_id: 2,
        filename: "../Remote_P.pak".into(),
        version: None,
        updated_at: None,
    };
    assert!(
        library
            .install_remote_bundle(
                "game",
                "remote",
                &[(download.clone(), release.clone())],
                &details,
                InstallOptions::default(),
            )
            .is_err()
    );
    release.filename = "Remote_P.pak".into();
    library.install_remote_bundle(
        "game",
        "remote",
        &[(download, release)],
        &details,
        InstallOptions::default(),
    )?;
    assert_eq!(fs::read_to_string(mods.join("Remote_P.pak"))?, "remote");
    Ok(())
}

#[test]
fn cyberpunk_update_rollback_and_remove_restore_original() -> Result<()> {
    let f = Fixture::new()?;
    let a = "archive/pc/mod/remote.archive";
    let b = "r6/tweaks/options.yaml";
    put(&f.root, a, "external installation")?;
    let first = f.input("first", &[(a, "first version"), (b, "options")])?;
    let second = f.input("second", &[(a, "second version")])?;
    let mut library = f.library(GameKind::Cyberpunk)?;
    assert!(
        library
            .install("game", "mod", &first, None, InstallOptions::default())
            .is_err()
    );
    assert_eq!(fs::read_to_string(f.root.join(a))?, "external installation");
    assert!(!f.root.join(b).exists());
    assert!(library.mods("game")?.is_empty());

    library.install(
        "game",
        "mod",
        &first,
        None,
        InstallOptions {
            replace_unmanaged: true,
            ..Default::default()
        },
    )?;
    assert_eq!(fs::read_to_string(f.root.join(a))?, "first version");
    assert_eq!(fs::read_to_string(f.root.join(b))?, "options");
    library.toggle("game", "mod", false, false, false)?;
    assert_eq!(fs::read_to_string(f.root.join(a))?, "external installation");
    assert!(!f.root.join(b).exists());
    drop(library);

    let mut library = Library::open(&f.state)?;
    library.toggle("game", "mod", true, false, false)?;
    library.install("game", "mod", &second, None, InstallOptions::default())?;
    assert_eq!(fs::read_to_string(f.root.join(a))?, "second version");
    assert!(!f.root.join(b).exists());
    library.rollback("game", "mod", 0, false)?;
    assert_eq!(fs::read_to_string(f.root.join(a))?, "first version");
    assert_eq!(fs::read_to_string(f.root.join(b))?, "options");
    library.remove("game", "mod")?;
    assert_eq!(fs::read_to_string(f.root.join(a))?, "external installation");
    assert!(!f.root.join(b).exists());
    Ok(())
}

#[test]
fn reserved_destinations_and_external_edits_are_not_overwritten() -> Result<()> {
    let f = Fixture::new()?;
    let relative = "archive/pc/mod/Example.archive";
    let a = f.input("first", &[(relative, "deployed")])?;
    let b = f.input("second", &[("archive/pc/mod/example.archive", "collision")])?;
    let mut library = f.library(GameKind::Cyberpunk)?;
    library.install(
        "game",
        "first",
        &a,
        None,
        InstallOptions {
            keep_disabled: true,
            ..Default::default()
        },
    )?;
    let conflict = library
        .install(
            "game",
            "second",
            &b,
            None,
            InstallOptions {
                replace_unmanaged: true,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(format!("{conflict:#}").contains("conflicts with first"));
    assert!(!f.root.join(relative).exists());
    library.toggle("game", "first", true, false, false)?;
    put(&f.root, relative, "manual edit")?;
    assert!(library.remove("game", "first").is_err());
    assert!(
        library
            .toggle("game", "first", false, false, false)
            .is_err()
    );
    assert_eq!(fs::read_to_string(f.root.join(relative))?, "manual edit");
    assert!(library.managed_mod("game", "first")?.enabled);
    Ok(())
}

#[test]
fn zzmi_switches_whole_folders_and_preserves_unmanaged_assets() -> Result<()> {
    xxmi_folder_lifecycle(GameKind::Zzmi)
}

#[test]
fn efmi_switches_whole_folders_and_preserves_unmanaged_assets() -> Result<()> {
    xxmi_folder_lifecycle(GameKind::Efmi)
}

fn xxmi_folder_lifecycle(kind: GameKind) -> Result<()> {
    let f = Fixture::new()?;
    let input = f.input(
        "skin",
        &[
            ("wrapper/skin.ini", "x"),
            ("wrapper/textures/body.dds", "asset"),
        ],
    )?;
    let mut library = f.library(kind)?;
    library.install(
        "game",
        "ellen",
        &input,
        None,
        InstallOptions {
            group: Some("ellen"),
            ..Default::default()
        },
    )?;
    library.install(
        "game",
        "ellen-alt",
        &input,
        None,
        InstallOptions {
            keep_disabled: true,
            group: Some("ellen"),
            ..Default::default()
        },
    )?;
    assert!(f.root.join("ellen/textures/body.dds").is_file());
    assert!(f.root.join("DISABLED_ellen-alt/skin.ini").is_file());
    assert!(
        library
            .toggle("game", "ellen-alt", true, false, false)
            .is_err()
    );
    put(&f.root, "ellen/skin.ini", "user-edited settings")?;
    library.toggle("game", "ellen", false, false, false)?;
    library.toggle("game", "ellen-alt", true, false, false)?;
    assert!(!f.root.join("ellen").exists());
    assert!(f.root.join("DISABLED_ellen/textures/body.dds").is_file());
    assert_eq!(
        fs::read_to_string(f.root.join("DISABLED_ellen/skin.ini"))?,
        "user-edited settings"
    );
    assert!(library.remove("game", "ellen").is_err());
    assert!(f.root.join("ellen-alt/skin.ini").is_file());
    put(&f.root, "ellen-alt/notes.ini", "unmanaged")?;
    assert!(
        library
            .toggle("game", "ellen-alt", false, false, false)
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(f.root.join("ellen-alt/notes.ini"))?,
        "unmanaged"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn never_deploys_through_game_symlink() -> Result<()> {
    use std::os::unix::fs::symlink;
    let f = Fixture::new()?;
    let outside = f.dir.path().join("outside");
    fs::create_dir(&outside)?;
    symlink(&outside, f.root.join("archive"))?;
    let input = f.input("input", &[("example.archive", "mod")])?;
    let mut library = f.library(GameKind::Cyberpunk)?;
    assert!(
        library
            .install("game", "mod", &input, None, InstallOptions::default())
            .is_err()
    );
    assert_eq!(fs::read_dir(outside)?.count(), 0);
    assert!(library.mods("game")?.is_empty());
    Ok(())
}

#[test]
fn local_bookmarks_survive_reopening() -> Result<()> {
    let f = Fixture::new()?;
    let library = Library::open(&f.state)?;
    let id = RemoteMod {
        source: Source::GameBanana,
        game: "1".into(),
        id: 2,
    };
    library.save_favorite(&id, "Example mod", None)?;
    library.save_search(
        "browse",
        &SavedSearch {
            source: Source::GameBanana,
            game: "1".into(),
            query: SearchQuery {
                text: "example".into(),
                page: 3,
                category: Some("5".into()),
                sort: Sort::Newest,
                ..SearchQuery::default()
            },
        },
    )?;
    drop(library);
    let library = Library::open(&f.state)?;
    let bookmarks = library.saved_favorites()?;
    assert_eq!(bookmarks[0].item, id);
    assert_eq!(bookmarks[0].name, "Example mod");
    let saved = library.saved_search("browse")?;
    assert_eq!(saved.query.category.as_deref(), Some("5"));
    assert_eq!(saved.query.page, 3);
    library.unsave(&id)?;
    assert!(library.saved_favorites()?.is_empty());
    Ok(())
}
