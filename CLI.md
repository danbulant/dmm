# DMM CLI

Project includes CLI support, mainly used for local testing.

Slop text:

Use `dmm --help` for commands. `dmm gui` (or `dmm` without a command) opens the desktop app in a build with `rune-ui`. Pass `--state <directory>` to use a separate library.

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

Cyberpunk and FF7 Remake install files into the game. `--disabled` imports without deploying those files. ZZMI and EFMI manage folders in the selected importer's `Mods` directory and use `DISABLED_` renames. Use `--group <character>` to prevent a second mod in the same group from being enabled unless explicitly allowed.

```sh
dmm add-game zzz zzmi '/path/to/ZZMI/Mods'
dmm install zzz ellen ./Ellen.7z --group ellen
dmm launch zzz --plan --xxmi '/path/to/XXMI Launcher.exe' --wine-prefix '/path/to/prefix'
dmm add-game endfield efmi '/path/to/EFMI/Mods'
dmm detect ff7_remake
dmm add-game ff7 ff7_remake '/path/to/FINAL FANTASY VII REMAKE'
dmm install ff7 outfit ./Outfit_P.pak
dmm launch cp --plan
dmm diagnose cp
```

FF7 Remake accepts loose `.pak` files and PAKs inside archives or folders. DMM installs them into `End/Content/Paks/~mods/`. If PAKs are split across multiple folders, extract and import the variant you want. FFVIIHook and INI mods are not supported by the PAK importer.

Drop `--plan` to launch. Cyberpunk and FF7 Remake use Steam's URL handler. On Linux, `zzmi` and `efmi` launch via XXMI and Wine. Use `--wine` to choose a Wine binary. To use another launcher, pass it and its arguments after `--` in `dmm launch <instance> -- <program> <arguments...>`.

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

Nexus search and mod details work without `NEXUS_API_KEY`. Without a key, `install-remote` opens the Nexus file page (or prints it with `--no-browser`); you can download manually and install the local file. For authenticated downloads, set `NEXUS_API_KEY` in the environment. DMM does not store the key in SQLite. If Nexus requires a browser handoff, click **Mod Manager Download** for each selected file and pass each signed URI to `dmm accept-nxm 'nxm://…'` using the same state directory. DMM waits for all selected files before installing. On Linux, the desktop app can register a Nexus link handler from Game setup; CLI-only builds require pasting the URI manually.

This text was slopped
