use super::*;
use std::fs;

use dmm::games::GameKind;
use dmm::library::{Library, Release};
use dmm::sources::Source;
use rune::{Env, Runtime};

use crate::desktop::{Backend, Worker as LibraryWorker, view as desktop_view};

fn request(id: u64) -> Result<Request> {
    let details: ModDetails = serde_json::from_value(serde_json::json!({
        "id": RemoteMod { source: Source::GameBanana, game: "19567".into(), id },
        "name": format!("Mod {id}"), "images":[], "requirements":[],
        "files":[{"id":id, "name":format!("mod-{id}.zip"), "size":4096, "primary":true}],
    }))?;
    Ok(Request {
        files: details.files.clone(),
        details,
        destination: None,
        keep_disabled: true,
    })
}

#[test]
fn queue_only_dispatches_one_transfer_and_saves_a_cancelled_item() -> Result<()> {
    let state = tempfile::tempdir()?;
    let (jobs, requests) = mpsc::channel();
    let (events, receiver) = mpsc::channel();
    let mut queue = Queue::default();
    queue.worker = Some(Worker {
        jobs,
        events: receiver,
    });
    queue.recovery = Some(Arc::new(Recovery::open(state.path())?));
    for id in 1..=3 {
        queue.enqueue("zzz".into(), request(id)?)?;
    }
    let first = requests.try_recv()?;
    assert_eq!(first.id.id, 1);
    assert!(requests.try_recv().is_err());

    queue.cancel(1);
    assert!(matches!(queue.items[1].phase, Phase::Cancelled));
    assert_eq!(queue.save.len(), 1);
    let library = Library::open(state.path())?;
    download_recovery::save(&library, &queue.save)?;
    assert_eq!(library.saved_favorites()?[0].item.id, 2);
    events.send(Event::Progress(
        first.index,
        InstallProgress::Downloading {
            current: 1,
            total: 1,
            file_name: "mod-1.zip".into(),
            received_bytes: 1024,
            total_bytes: Some(4096),
            bytes_per_second: 2048,
        },
    ))?;
    queue.poll();
    let transfer = queue.items[0]
        .transfer
        .as_ref()
        .context("missing active transfer")?;
    assert_eq!(transfer.file_name, "mod-1.zip");
    assert_eq!(transfer.received_bytes, 1024);
    assert_eq!(transfer.bytes_per_second, 2048);

    download_recovery::discard(first.recovery_path.as_deref().unwrap())?;
    events.send(Event::Finished(first.index, Ok(Completion::Installed)))?;
    queue.poll();
    assert!(matches!(queue.items[0].phase, Phase::Complete));
    #[cfg(any(target_os = "linux", windows))]
    assert_eq!(queue.take_finished_batch(), None);
    let third = requests.try_recv()?;
    assert_eq!(third.id.id, 3);
    queue.cancel(third.index);
    assert!(third.control.is_cancelled());
    events.send(Event::Finished(third.index, Ok(Completion::Cancelled)))?;
    queue.poll();
    #[cfg(any(target_os = "linux", windows))]
    {
        assert_eq!(queue.take_finished_batch(), Some(1));
        assert_eq!(queue.take_finished_batch(), None);
    }
    download_recovery::save(&library, &queue.save)?;
    assert_eq!(
        library
            .saved_favorites()?
            .into_iter()
            .map(|entry| entry.item.id)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    Ok(())
}

#[test]
fn completed_download_switch_changes_only_the_downloads_installation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_dir = dir.path().join("state");
    let selected = dir.path().join("selected");
    let target = dir.path().join("target");
    let input = dir.path().join("input");
    for folder in [&selected, &target, &input] {
        fs::create_dir(folder)?;
    }
    fs::write(input.join("skin.ini"), b"mod")?;
    let details = request(14)?.details;
    let release = Release {
        id: details.id.clone(),
        source_url: None,
        file_id: 14,
        filename: "skin.zip".into(),
        version: None,
        updated_at: None,
    };
    let mut library = Library::open(&state_dir)?;
    for (name, root) in [("selected", &selected), ("target", &target)] {
        library.register(name, GameKind::Zzmi, root)?;
        library.install(name, "skin", &input, Some(release.clone()), Default::default())?;
    }
    let mut state = State::new(state_dir.clone());
    state.screen = Screen::Downloads;
    state.snapshot = Snapshot::load(&library, Some("selected"))?;
    drop(library);
    state.queue.items.push(Entry {
        id: details.id,
        instance: "target".into(),
        name: details.name,
        image: None,
        file_count: 1,
        estimated_bytes: None,
        received_before_file: 0,
        transfer: None,
        recovery_path: None,
        phase: Phase::Complete,
    });
    let (jobs, requests) = mpsc::channel();
    let (responses, results) = mpsc::channel();
    state.worker = Some(LibraryWorker { jobs, results });
    let mut backend = Backend { state_dir, online: None };
    let mut runtime = Runtime::new(state, desktop_view, (960.0, 620.0), Env::new(true, 1.0));
    runtime.poll = Some(State::poll);
    runtime.frame(0.0);
    for enabled in [false, true] {
        let (x, y) = runtime.find("queue-enabled-0").context("missing completed download switch")?;
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(1.0 / 60.0);
        let job = requests.try_recv().with_context(|| format!("switch did not request enabled={enabled}"))?;
        responses.send(job(&mut backend))?;
        runtime.frame(1.0 / 60.0);
        assert_eq!(runtime.state.snapshot.selected.as_ref().unwrap().name, "selected");
        assert!(selected.join("skin/skin.ini").is_file());
        assert_eq!(target.join("skin/skin.ini").is_file(), enabled);
        assert_eq!(target.join("DISABLED_skin/skin.ini").is_file(), !enabled);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn downloads_notification_switch_persists_even_with_an_empty_queue() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut state = State::new(dir.path().to_owned());
    state.screen = Screen::Downloads;
    let mut runtime = Runtime::new(state, desktop_view, (960.0, 620.0), Env::new(true, 1.0));
    runtime.frame(0.0);
    for expected in [true, false] {
        let (x, y) = runtime.find("queue-notify-complete").context("missing notification switch")?;
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(1.0 / 60.0);
        let saved = crate::desktop::Preferences::load(dir.path())?;
        assert_eq!(saved.notify_downloads_complete, expected);
    }
    Ok(())
}
