# DMM CLI

Project includes CLI support, mainly used for local testing.

Slop text:

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
