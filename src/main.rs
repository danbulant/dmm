#[cfg(feature = "rune-ui")]
mod desktop;
mod remote;

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use dmm::games::{GameKind, cp, detect, launch_profile};
use dmm::library::{InstallOptions, Library};
use dmm::sources::{RemoteMod, Source};
use serde::Serialize;
use serde_json::json;

use remote::{Browse, Identity, RemoteInstall};

#[derive(Parser)]
#[command(
    version,
    about = "Folder-based mod manager prototype",
    subcommand_required = !cfg!(feature = "rune-ui"),
    arg_required_else_help = !cfg!(feature = "rune-ui")
)]
struct Cli {
    #[arg(long, global = true, default_value = ".dmm")]
    state: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the native Rune mod manager (requires the rune-ui feature).
    #[cfg(feature = "rune-ui")]
    Gui,
    /// Find installed Cyberpunk or ZZMI game roots without changing them.
    Detect {
        kind: GameKind,
    },
    /// Register a game root. Select the Mods directory itself for ZZMI.
    AddGame {
        name: String,
        kind: GameKind,
        root: PathBuf,
        #[arg(long)]
        profile: Option<PathBuf>,
    },
    Games,
    /// Preview mappings and collisions without deploying anything.
    Inspect {
        instance: String,
        slug: String,
        input: PathBuf,
        #[arg(long)]
        into: Option<PathBuf>,
    },
    Install {
        instance: String,
        slug: String,
        input: PathBuf,
        #[command(flatten)]
        flags: InstallFlags,
    },
    Installed {
        instance: String,
    },
    Enable {
        instance: String,
        slug: String,
        #[arg(long)]
        replace_unmanaged: bool,
        #[arg(long)]
        allow_multiple: bool,
    },
    Disable {
        instance: String,
        slug: String,
    },
    Remove {
        instance: String,
        slug: String,
    },
    Rollback {
        instance: String,
        slug: String,
        version: usize,
        #[arg(long)]
        replace_unmanaged: bool,
    },
    /// Launch the registered game; custom command and args go after `--`.
    Launch {
        instance: String,
        #[arg(long)]
        xxmi: Option<PathBuf>,
        #[arg(long)]
        wine: Option<PathBuf>,
        #[arg(long)]
        wine_prefix: Option<PathBuf>,
        #[arg(long)]
        plan: bool,
        #[arg(last = true)]
        custom: Vec<OsString>,
    },
    Diagnose {
        instance: String,
    },
    Search(Browse),
    Categories {
        source: Source,
        game: String,
    },
    Details(Identity),
    InstallRemote(RemoteInstall),
    /// Finish an authorized Nexus handoff from a copied or OS-delivered nxm:// URL.
    AcceptNxm {
        uri: String,
        /// Deliver an OS-opened link to the live desktop queue; never mutate its library separately.
        #[arg(long)]
        from_handler: bool,
    },
    /// List source updates, or install unambiguous updates with --apply.
    Updates {
        instance: String,
        #[arg(long)]
        apply: bool,
    },
    Save(Identity),
    Unsave(Identity),
    Saved,
    SaveSearch {
        name: String,
        #[command(flatten)]
        search: Browse,
    },
    RunSearch {
        name: String,
    },
}

#[derive(Args, Default)]
struct InstallFlags {
    /// Explicit relative destination directory for ambiguous layouts.
    #[arg(long)]
    into: Option<PathBuf>,
    /// Import into the library without enabling.
    #[arg(long)]
    disabled: bool,
    /// Back up and replace conflicting unmanaged files.
    #[arg(long)]
    replace_unmanaged: bool,
    /// Assign an exclusive group, such as a character name.
    #[arg(long)]
    group: Option<String>,
    #[arg(long)]
    allow_multiple: bool,
}

impl InstallFlags {
    fn options(&self) -> InstallOptions<'_> {
        InstallOptions {
            destination: self.into.as_deref(),
            keep_disabled: self.disabled,
            replace_unmanaged: self.replace_unmanaged,
            group: self.group.as_deref(),
            allow_multiple: self.allow_multiple,
        }
    }
}

fn main() -> Result<()> {
    let Cli { state, command } = Cli::parse();
    match command {
        #[cfg(feature = "rune-ui")]
        None | Some(Command::Gui) => {
            desktop::run(state);
            Ok(())
        }
        Some(Command::Detect { kind }) => print(&detect(kind)?),
        Some(Command::Categories { source, game }) => {
            print(&remote::client()?.categories(source, &game)?)
        }
        Some(Command::Details(identity)) => print(&remote::client()?.details(&identity.into())?),
        Some(Command::AcceptNxm {
            uri,
            from_handler: true,
        }) => {
            #[cfg(all(feature = "rune-ui", target_os = "linux"))]
            {
                desktop::forward_nexus_handler(&state, &uri)
            }
            #[cfg(not(all(feature = "rune-ui", target_os = "linux")))]
            {
                let _ = uri;
                anyhow::bail!(
                    "Nexus desktop-link handling is supported only by the Linux desktop build"
                )
            }
        }
        Some(command) => with_library(&state, command),
        #[cfg(not(feature = "rune-ui"))]
        None => unreachable!("Clap requires a subcommand in CLI-only builds"),
    }
}

fn with_library(state: &Path, command: Command) -> Result<()> {
    let mut library = Library::open(state)?;
    match command {
        Command::AddGame {
            name,
            kind,
            root,
            profile,
        } => print(&library.register(&name, kind, &root, profile.as_deref())?),
        Command::Games => print(&library.instances()?),
        Command::Inspect {
            instance,
            slug,
            input,
            into,
        } => print(&library.inspect(&instance, &slug, &input, into.as_deref())?),
        Command::Install {
            instance,
            slug,
            input,
            flags,
        } => library.install(&instance, &slug, &input, None, flags.options()),
        Command::Installed { instance } => print(&library.mods(&instance)?),
        Command::Enable {
            instance,
            slug,
            replace_unmanaged,
            allow_multiple,
        } => library.toggle(&instance, &slug, true, replace_unmanaged, allow_multiple),
        Command::Disable { instance, slug } => library.toggle(&instance, &slug, false, false, true),
        Command::Remove { instance, slug } => library.remove(&instance, &slug),
        Command::Rollback {
            instance,
            slug,
            version,
            replace_unmanaged,
        } => library.rollback(&instance, &slug, version, replace_unmanaged),
        Command::Launch {
            instance,
            xxmi,
            wine,
            wine_prefix,
            plan,
            custom,
        } => {
            let instance = library.instance(&instance)?;
            let command = launch_profile(
                &instance.profile()?,
                &instance.root,
                xxmi.as_deref(),
                wine.as_deref(),
                wine_prefix.as_deref(),
                &custom,
            )?;
            if plan {
                println!("{command:?}");
                Ok(())
            } else {
                command.spawn()
            }
        }
        Command::Diagnose { instance } => {
            let instance = library.instance(&instance)?;
            print(&json!({
                "root": instance.root,
                "exists": instance.root.is_dir(),
                "cyberpunk_proton": (instance.kind == GameKind::Cyberpunk).then(|| cp::proton_diagnostics(&instance.root)).flatten(),
            }))
        }
        Command::Search(search) => {
            let instance = search.instance.clone();
            remote::search(&library, search.into(), instance.as_deref())
        }
        Command::InstallRemote(request) => remote::install(&mut library, request),
        Command::AcceptNxm { uri, from_handler } => {
            if from_handler {
                anyhow::bail!("OS-delivered Nexus links must be routed to the running desktop");
            }
            remote::accept_nxm(&mut library, &uri)
        }
        Command::Updates { instance, apply } => remote::updates(&mut library, &instance, apply),
        Command::Save(identity) => {
            let id: RemoteMod = identity.into();
            let details = remote::client()?.details(&id)?;
            let image = details
                .images
                .first()
                .map(|image| image.thumbnail.as_deref().unwrap_or(&image.url));
            library.save_favorite(&id, &details.name, image)
        }
        Command::Unsave(identity) => library.unsave(&identity.into()),
        Command::Saved => print(&library.saved_favorites()?),
        Command::SaveSearch { name, search } => library.save_search(&name, &search.into()),
        Command::RunSearch { name } => remote::search(&library, library.saved_search(&name)?, None),
        #[cfg(feature = "rune-ui")]
        Command::Gui => unreachable!("desktop command"),
        Command::Detect { .. } | Command::Categories { .. } | Command::Details(_) => {
            unreachable!("stateless command")
        }
    }
}

fn print<T: Serialize>(value: &T) -> Result<()> {
    let output = io::stdout().lock();
    let mut json = serde_json::Serializer::pretty(output);
    value.serialize(&mut json).context("writing JSON output")?;
    writeln!(json.into_inner())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "rune-ui")]
    #[test]
    fn the_desktop_accepts_no_subcommand_and_an_optional_state_directory() {
        assert!(Cli::try_parse_from(["dmm"]).unwrap().command.is_none());
        let app = Cli::try_parse_from(["dmm", "--state", "other-library"]).unwrap();
        assert!(app.command.is_none());
        assert_eq!(app.state, PathBuf::from("other-library"));
        assert!(matches!(
            Cli::try_parse_from(["dmm", "gui"]).unwrap().command,
            Some(Command::Gui)
        ));
    }

    #[cfg(not(feature = "rune-ui"))]
    #[test]
    fn the_cli_only_build_requires_a_command_and_displays_help() {
        let Err(error) = Cli::try_parse_from(["dmm"]) else {
            panic!("a CLI-only build must not accept an absent command");
        };
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        assert!(matches!(
            Cli::try_parse_from(["dmm", "games"]).unwrap().command,
            Some(Command::Games)
        ));
    }
}
