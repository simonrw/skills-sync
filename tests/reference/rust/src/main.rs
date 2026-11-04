mod config;
mod generation;
mod lock;
mod patch;
mod source;
mod sync;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Relative manifest paths are resolved from the manifest directory.
    #[arg(short, long, default_value = "skills.toml", global = true)]
    manifest: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Write an example manifest without overwriting an existing file.
    Init,
    /// Reconcile skills, preserving already locked Git commits.
    Sync {
        /// Require unchanged manifest, patches, local content, and lockfile.
        #[arg(long)]
        locked: bool,
        /// Never fetch Git objects from upstream.
        #[arg(long)]
        offline: bool,
        /// Validate a plan without changing links or lockfile (may populate cache).
        #[arg(long)]
        dry_run: bool,
    },
    /// Advance commits and sync. Optional arguments limit the source IDs updated.
    Update {
        sources: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Generate a verified scoped Markdown patch from the locked effective source.
    Patch {
        #[command(subcommand)]
        command: PatchCommands,
    },
    /// Show the last successfully installed skills and their upstream origins.
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum PatchCommands {
    Generate {
        source: String,
        file: PathBuf,
        edited: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Commands::Init) {
        util::create_new(&cli.manifest, include_bytes!("../examples/skills.toml"))?;
        println!("Created {}", cli.manifest.display());
        return Ok(());
    }
    let config = config::Loaded::read(&cli.manifest)?;
    match cli.command {
        Commands::Sync {
            locked,
            offline,
            dry_run,
        } => sync::run(
            &config,
            sync::Options {
                locked,
                offline,
                dry_run,
                update: None,
            },
        ),
        Commands::Update { sources, dry_run } => sync::run(
            &config,
            sync::Options {
                locked: false,
                offline: false,
                dry_run,
                update: Some(sources),
            },
        ),
        Commands::Patch {
            command:
                PatchCommands::Generate {
                    source,
                    file,
                    edited,
                    output,
                },
        } => generation::run(&config, &source, &file, &edited, &output),
        Commands::List { json } => sync::list(&config, json),
        Commands::Init => unreachable!(),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
