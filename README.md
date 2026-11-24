# DMM

A simple graphical mod manager for Zenless Zone Zero, Arknights: Endfield and Cyberpunk 2077, on Windows and Linux. Supports Game Banana and Nexus Mods.

![](./screenshots/zzz.png)

Features:

- Online mod download from Game Banana and Nexus Mods (requires opening the browser)
- Setup ZZMI/EFMI/XXMI (ZZZ/Endfield mod loader)
- Linux support (currently only tested on NixOS and Steam versions)

Check releases for downloads.

## Disclaimers

Currently only tested on Linux. Windows should work, given both games are running under Windows compatibility layer, and tests are running for it.

Code has been largely AI generated but human read.

## Build

Use `nix develop` for the project's Rust toolchain and system libraries.

```sh
nix develop --command cargo build --locked
nix develop --command cargo test --locked
nix develop --command cargo build --locked --no-default-features # CLI only, see ./CLI.md
```

## Desktop app

```sh
nix develop --command cargo run --locked -- --state .dmm
```

By default, the desktop and CLI store state in `$XDG_STATE_HOME/dmm` on Linux (`~/.local/state/dmm` if unset or relative) and `%LOCALAPPDATA%\dmm` on Windows. Use `--state .dmm` for local development or `--state /path/to/old/library` to keep using an existing library.

## Code layout

- `src/games/`: game profiles and the shared planner interface; `cp` owns Cyberpunk behavior and `zzz` owns ZZMI discovery, launch, and Steam Proton setup.
- `src/archive.rs`, `src/acquisition.rs`, `src/sources/`: archive inspection, remote acquisition, and provider APIs. They don't render UI.
- `src/library/`: installation manifests, deployment and recovery, and SQLite persistence. Explicit SQL keeps the on-disk schema visible; rusqlite transactions coordinate database writes with deployment recovery.
- `src/desktop/`: Rune shell, views, shared components, and desktop background workers. `src/main.rs` dispatches commands to the same core.
