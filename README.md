# DMM

DMM is a Rust mod manager for games that load mods from known directories. The Rune desktop app and CLI share archive, source, game-planning, and SQLite library code.

## Build

Use `nix develop` for the project's Rust toolchain and system libraries. Outside Nix, install Rust, a native linker and libarchive development files (the library plus its import library on Windows). The default Rune build also needs the Wayland and xkbcommon development packages on Linux.

```sh
nix develop --command cargo build --locked
nix develop --command cargo test --locked
nix develop --command cargo build --locked --no-default-features # CLI only
```

Run `./target/debug/dmm --help` to see the CLI. The examples below use `dmm` for that binary. Run from a dedicated working directory or pass `--state /path/to/library` on every invocation. The default is `.dmm` in the current directory. Keep that library separate from your game installations.

## Desktop app

```sh
nix develop --command cargo run --locked
```

With Rune enabled, run `dmm` (or `dmm --state /path/to/library`) to open the desktop. The explicit `gui` subcommand remains available.

The **Game** dropdown lists games, not installation paths, and remembers the last game you used. **Game setup** merges detected and manually added paths. DMM automatically uses a single valid installation; if there are several, choose the main one with its radio button. The path choice is saved per game. Use **+ Add manually** for paths detection misses. **Launch game** is in the sidebar. For Cyberpunk it opens the copy registered in Steam; the active folder determines where DMM deploys mods.

Zenless Zone Zero's vanilla Steam folder does not load mods by itself. DMM looks for the [ZZMI importer's `Mods` folder](https://github.com/leotorrez/ZZMI-Package/blob/main/ZZMI/d3dx.ini) under common XXMI locations, Wine prefixes, and the Steam game folder if it contains a configured ZZMI package. **Set up XXMI** opens the [official instructions](https://github.com/SpectrumQT/XXMI-Launcher#installation). Install the importer in XXMI, then rescan. On Linux, use the same Wine environment to launch; you can save an executable and `WINEPREFIX` override per installation.

On Linux with Steam, **Set up XXMI for Steam…** downloads the official portable launcher and installs Microsoft's x64 Visual C++ runtime in the detected ZZZ Proton prefix. You need [Protontricks](https://github.com/Matoking/protontricks#installation) and a prefix already created by launching ZZZ once through Steam. Close ZZZ before setup. The setup dialog stays open and shows progress until installation completes or fails. Then use **Start XXMI**, install the ZZMI importer in its GUI, and **Rescan** in DMM. **Launch game** remains the headless modded launch.

**Explore** searches on open. It starts with **Recent** and remembers the sort and filters for each game and source. While a request runs, previous results dim in place and the pager stays visible. GameBanana can sort by likes or downloads; Nexus uses endorsements. Nexus starts in **SFW only**; **Clear filters** preserves **SFW only** or **All** and resets **NSFW only** to **All**. GameBanana starts in **Unrated**; clearing filters preserves **Unrated** or **All** and clears any other selected tags. On Nexus, **Published** filters the last 7, 30, or 365 days. Combining it with **Most endorsed** ranks recently published mods by lifetime endorsements, not endorsements earned within the window.

GameBanana categories follow the site's hierarchy, with Character Skins, Bangboo Skins, and provider icons when available. Explore and Local mods use the same cards: use the bookmark icon to save or unsave a mod, and use the bottom action to install it, view its download, or enable and disable an installed copy. Cards show provider metrics and dates when available, and Explore shows the total page count when known.

**Local mods** combines installed, queued, and bookmarked mods in a searchable grid. Installed cards show deployed file size; tag and character filters list values from the local library. Open an installed card for file history and rollback. Removing an installation requires confirmation. Bookmarks keep the provider's name and thumbnail URL without deploying files. Older ID-only bookmarks are removed when the library opens.

For Cyberpunk, enabling a mod copies its staged files to the paths in its manifest. Disabling it removes those files and restores any backed-up originals. DMM refuses to overwrite files owned by another enabled mod or undo files changed outside DMM. It does not install Cyberpunk frameworks or change Wine DLL overrides.

ZZMI mods deploy into separate directories. DMM detects file collisions and can enforce explicitly assigned exclusive groups; it does not infer rendering conflicts from INI hashes. The same hash can be intentional or subject to different conditions and priorities.

The desktop uses the same `--state` path as the CLI. **Install** opens the file picker without leaving the current grid. Check one or more files and choose **Queue install**. **Downloads**, above **Launch game**, shows thumbnails, the current file, transferred and total bytes when known, and a rolling download speed. You can launch while a download is running, but it may subsequently deploy files while you play. Launching is disabled during deployment to the selected game. Open Downloads to cancel pending transfers or see completed installs from this session. If you quit before an install completes, DMM saves the mod as a bookmark on the next launch rather than resuming the transfer.

**Enable after installing** is remembered for future imports. Some Nexus downloads require approval from the website. When one is waiting, **Finish Nexus downloads** appears in Downloads: open the file on Nexus, choose **Mod Manager Download**, and paste the resulting `nxm://` link. This hands the authorization from the browser to DMM; use the same `NEXUS_API_KEY` as the CLI. **+ Add local mod** imports an archive or folder; on Linux, the file chooser uses an XDG Desktop Portal, and you can enter a path manually instead.

On Linux, **Game setup → Nexus links** can register a per-user `nxm://` handler through `xdg-mime`, including on NixOS. It asks before taking over the existing handler. Keep DMM running with `NEXUS_API_KEY` configured so it can receive the link and continue the waiting install. Manual paste remains available.

Select every part of numbered split archives such as `.7z.001` and `.7z.002`. Split RAR and `.z01` ZIP sets must be reassembled separately. If selected archives contain overlapping destination paths, DMM rejects the bundle without replacing the existing installation.

If libarchive cannot unpack a RAR file, DMM can retry using `unrar` from `PATH`. It still validates member paths and limits, and UnRAR must verify the file contents before anything is deployed.

New remote installations retain a snapshot of the provider details, which Local mods can open without network access. Dependencies are not installed automatically. Click a screenshot to expand it; use the zoom buttons or Ctrl+wheel and drag to pan. Cached thumbnails can display offline or while a full-size image loads.

Public catalog responses are cached in `source-cache.sqlite3` under `--state` (search: 5 minutes; details: 2 minutes; filters: 1 hour). The HTTP clients reuse connections; reqwest does not cache response bodies automatically. Category icons and thumbnails are saved to a bounded `thumbnails.sqlite3` disk cache and reused according to the server's freshness information. Full-size previews use the decoded memory cache. Downloads are staged in the library, and Nexus handoff credentials are not saved. Game icons come from [Cyberpunk 2077](https://gamebanana.com/games/8722) and [Zenless Zone Zero](https://gamebanana.com/games/19567); source icons come from [GameBanana](https://gamebanana.com/) and [Nexus Mods](https://graphql.nexusmods.com/).

## Code layout

- `src/games/`: game profiles and the shared planner interface; `cp` owns Cyberpunk behavior and `zzz` owns ZZMI discovery, launch, and Steam Proton setup.
- `src/archive.rs`, `src/acquisition.rs`, `src/sources/`: archive inspection, remote acquisition, and provider APIs. They don't render UI.
- `src/library/`: installation manifests, deployment and recovery, and SQLite persistence. Explicit SQL keeps the on-disk schema visible; rusqlite transactions coordinate database writes with deployment recovery.
- `src/desktop/`: Rune shell, views, shared components, and desktop background workers. `src/main.rs` dispatches commands to the same core.

## Local files

```sh
dmm detect cyberpunk
dmm add-game cp cyberpunk '/path/to/Cyberpunk 2077'
dmm inspect cp archive-xl ArchiveXL.zip
dmm install cp archive-xl ArchiveXL.zip --disabled
dmm installed cp
dmm enable cp archive-xl
dmm disable cp archive-xl
dmm rollback cp archive-xl 0
dmm remove cp archive-xl
```

ZIP, TAR/TAR.GZ, 7z, RAR and directories are accepted. An installation preview extracts in the library's temporary workspace and never deploys files. Cyberpunk preserves recognized root layouts and infers common loose file types. If a file is ambiguous, inspect again with `--into <relative-directory>` and use the same option on install. Check that you're importing the intended variant of any archive that offers multiple packages.

Cyberpunk and generic installations copy into the game; `--disabled` stages them without writing to the game. ZZMI manages a folder in the selected `Mods` directory and uses `DISABLED_` folder renames. Use `--group <character>` to prevent a second mod in the same group from being enabled unless explicitly allowed.

```sh
dmm add-game zzz zzmi '/path/to/ZZMI/Mods'
dmm install zzz ellen ./Ellen.7z --group ellen
dmm launch zzz --plan --xxmi '/path/to/XXMI Launcher.exe' --wine-prefix '/path/to/prefix'
dmm launch cp --plan
dmm diagnose cp
```

Drop `--plan` to launch. Cyberpunk uses Steam's URL handler with its configured Proton settings. Linux ZZMI uses Wine (`--wine` to choose its binary), runs the XXMI launcher and does not start the game directly. To use another launcher, pass a program and its arguments after `--` in `dmm launch <instance> -- <program> <arguments...>`. See `profiles/generic.toml` for a custom generic TOML profile passed with `add-game --profile`; its install root is relative to the registered game root.

## Online sources

```sh
dmm search gamebanana 19567 ellen --sort popularity --instance zzz
dmm categories gamebanana 19567
dmm details gamebanana 19567 527697
dmm install-remote zzz gamebanana 527697 <file-id> [<additional-file-id>...] --disabled
dmm search nexus cyberpunk2077 atelier
dmm details nexus cyberpunk2077 <mod-id>
dmm install-remote cp nexus <mod-id> <file-id>
dmm updates cp
dmm updates cp --apply
dmm save nexus cyberpunk2077 <mod-id>
dmm saved
dmm save-search clothing nexus cyberpunk2077 clothing
dmm run-search clothing
```

[GameBanana](https://gamebanana.com/apiv11/Mod/ListFilterConfig) applies search, sorting, category, release type, **Unrated**, and combinations of content-rating tags server-side. Multiple tags match all selected tags. **Unrated** means no tags, not a verified SFW rating. The API does not expose a working negation or “any rating” filter. DMM uses the server's count and page availability and does not fetch per-mod profiles to filter search results. [Nexus GraphQL](https://graphql.nexusmods.com/) applies sort, text, category, author, language, tag, and adult filters server-side. `updates --apply` only applies unambiguous direct downloads.

Use `NEXUS_API_KEY` for Nexus download calls. The key is read from the environment, not stored in SQLite. For free accounts, `install-remote` prints or opens the Nexus file pages. Click **Mod Manager Download** for each selected file and pass each URI to `dmm accept-nxm 'nxm://…'` with the same state and key. The batch is not installed until all selected parts arrive. If you register a protocol handler yourself, have it forward the URI to that command; DMM does not change OS protocol settings.

## Deployment boundaries

- The library retains original archives and content-addressed mod files. `installed` includes manifest hashes and version indexes. Reinstall the same provider mod (or the same local slug) to add a version; `rollback` selects a retained version.
- Paths claimed by another managed mod, even a disabled mod, must be resolved before installing. To replace an **unmanaged** file explicitly, pass `--replace-unmanaged`; DMM backs it up and restores it on disable or remove. Hash mismatches stop a change rather than deleting a manual edit.
- A SQLite journal is written before changing deployed files. Interrupted operations roll back on the next open. Close the game and other mod managers before deployment; the library lock does not synchronize with unrelated processes or protect against concurrent edits to the game tree. This is process-crash recovery, not a cross-filesystem power-loss guarantee.
- Keep the state directory private. Read and review third-party mods before enabling them; DMM validates filesystem paths, not the behavior of game plugins. Removing mods does not garbage-collect blobs or clean unrelated directories.
