use super::*;
use dmm::library::Library;
use dmm::sources::Source;

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
    let third = requests.try_recv()?;
    assert_eq!(third.id.id, 3);
    queue.cancel(third.index);
    assert!(third.control.is_cancelled());
    events.send(Event::Finished(third.index, Ok(Completion::Cancelled)))?;
    queue.poll();
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
