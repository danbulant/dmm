# DMM

DMM is a Rust mod manager for games that load mods from known directories. The Rune desktop app and CLI share archive, source, game-planning, and SQLite library code.

## Build

Install a recent Rust toolchain, a native C linker and **system libarchive** (development package on Linux, or the library and linker import library on Windows). On NixOS use `nix develop`. If building the optional Rune feature on Linux, also install the Wayland and xkbcommon development packages.

```sh
cargo build --locked -j 1
cargo test --locked -j 1 -- --test-threads=1
cargo check --locked -j 1 --features rune-ui
```

Run `./target/debug/dmm --help` to see the CLI. The examples below use `dmm` for that binary. Run from a dedicated working directory or pass `--state /path/to/library` on every invocation. The default is `.dmm` in the current directory. Keep that library separate from your game installations.

## Desktop app

```sh
nix develop --command cargo run --locked --features rune-ui -- gui
```

The **Game** dropdown lists games, not installation paths. **Game setup** merges detected and manually added paths. DMM automatically uses a single valid installation; if there are several, choose the main one with its radio button. The choice is saved per game. Use **+ Add manually** for paths detection misses. **Launch game** is in the sidebar. For Cyberpunk it opens the copy registered in Steam; the active folder determines where DMM deploys mods.

Zenless Zone Zero's vanilla Steam folder does not load mods by itself. DMM looks for the [ZZMI importer's `Mods` folder](https://github.com/leotorrez/ZZMI-Package/blob/main/ZZMI/d3dx.ini) under common XXMI locations, Wine prefixes, and the Steam game folder if it contains a configured ZZMI package. **Set up XXMI** opens the [official instructions](https://github.com/SpectrumQT/XXMI-Launcher#installation). Install the importer in XXMI, then rescan. On Linux, use the same Wine environment to launch; you can save an executable and `WINEPREFIX` override per installation.

**Explore** searches on open and starts in **SFW only**. While a request runs, previous results dim in place and the pager stays visible. **Clear filters** searches again without changing **SFW only** or **All**; it resets **NSFW only** to **All**. On Nexus, **Published** filters the last 7, 30, or 365 days. Combining it with **Popular** ranks recently published mods by lifetime endorsements, not endorsements earned within the window. GameBanana does not expose a date filter on its mod listing endpoint.

GameBanana categories follow the site's hierarchy, with Character Skins, Bangboo Skins, and provider icons when available. Hover or focus a result to **Save** it or open the **Install** file chooser. Cards show provider metrics and dates when available, and the pager shows the total when known. In **Local mods**, use **+ Add local mod** to import a file or folder. Installed mods support enable/disable and rollback; saved bookmarks do not deploy files.

For Cyberpunk, enabling a mod copies its staged files to the paths in its manifest. Disabling it removes those files and restores any backed-up originals. DMM refuses to overwrite files owned by another enabled mod or undo files changed outside DMM. It does not install Cyberpunk frameworks or change Wine DLL overrides.

The desktop uses the same `--state` path as the CLI. Mod details list file metadata and link to requirements provided by the source; dependencies are not installed automatically. Click a screenshot to expand it. Use the zoom buttons or Ctrl+wheel, then drag to pan. A cached thumbnail displays until the full image arrives. Use **Install** in the mod header to select a file. Imports default to disabled. On Linux, the file chooser requires an XDG Desktop Portal; you can enter a path manually. For Nexus handoffs, use the **Nexus link** panel with the same `NEXUS_API_KEY` as the CLI.

Public catalog responses are cached in `source-cache.sqlite3` under `--state` (search: 5 minutes; details: 2 minutes; filters: 1 hour). Preview workers share a pooled HTTP client, fetch thumbnails in parallel, and reuse a bounded decoded-image cache. Downloads and Nexus handoff credentials are not cached. Game icons come from [Cyberpunk 2077](https://gamebanana.com/games/8722) and [Zenless Zone Zero](https://gamebanana.com/games/19567); source icons come from [GameBanana](https://gamebanana.com/) and [Nexus Mods](https://graphql.nexusmods.com/).

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
dmm install-remote zzz ellen gamebanana 527697 <file-id> --disabled
dmm search nexus cyberpunk2077 atelier
dmm details nexus cyberpunk2077 <mod-id>
dmm install-remote cp atelier nexus <mod-id> <file-id>
dmm updates cp
dmm updates cp --apply
dmm save nexus cyberpunk2077 <mod-id>
dmm saved
dmm save-search clothing nexus cyberpunk2077 clothing
dmm run-search clothing
```

[GameBanana](https://gamebanana.com/apiv11/Mod/ListFilterConfig) applies search, sorting, category, release type, and a single content tag server-side. When several tags are selected, DMM uses the rarest selected tag to narrow the server query and checks the rest against each mod's metadata. GameBanana does not expose a single server-side SFW/NSFW predicate. DMM applies visibility and multiple tags before filling the displayed page, retaining a cursor through the server pages. The final count is known when the candidate list is exhausted; selective filters can still yield short pages within the per-request scan limit. [Nexus GraphQL](https://graphql.nexusmods.com/) applies sort, text, category, author, language, tag, and adult filters server-side. `updates --apply` only applies unambiguous direct downloads.

Use `NEXUS_API_KEY` for Nexus download calls. The key is read from the environment, not stored in SQLite. For free accounts, `install-remote` prints/opens the Nexus file page. Click **Mod Manager Download**, then pass the resulting URI to `dmm accept-nxm 'nxm://…'` with the same state and key. If you register a protocol handler yourself, have it forward the URI to that command; DMM does not change OS protocol settings. An unsolicited link cannot deploy to a game without a matching queued install. Public application credentials, SSO, and tracked/endorsed account lists are not part of this prototype.

## Deployment boundaries

- The library retains original archives and content-addressed mod files. `installed` includes manifest hashes and version indexes. Reinstall the same slug and source to add a version; `rollback` selects a retained version.
- Paths claimed by another managed mod, even a disabled mod, must be resolved before installing. To replace an **unmanaged** file explicitly, pass `--replace-unmanaged`; DMM backs it up and restores it on disable or remove. Hash mismatches stop a change rather than deleting a manual edit.
- A SQLite journal is written before changing deployed files. Interrupted operations roll back on the next open. Close the game and other mod managers before deployment; the library lock does not synchronize with unrelated processes or protect against concurrent edits to the game tree. This is process-crash recovery, not a cross-filesystem power-loss guarantee.
- Keep the state directory private. Read and review third-party mods before enabling them; DMM validates filesystem paths, not the behavior of game plugins. Removing mods does not garbage-collect blobs or clean unrelated directories.
