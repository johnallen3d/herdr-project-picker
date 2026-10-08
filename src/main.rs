mod config;
mod herdr;
mod picker;

use clap::{Parser, Subcommand};
use config::{config_path, Registry};
use eyre::{eyre, Result};
use herdr::Herdr;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    Open,
    Picker,
    AddCurrent,
    List,
}

fn run(action: Action, herdr: &Herdr) -> Result<()> {
    if let Action::Open = action {
        return herdr.open_picker_popup();
    }
    let mut registry = Registry::load(config_path()?, &herdr.session_name()?)?;
    match action {
        Action::Open => unreachable!(),
        Action::List => {
            for p in &registry.projects {
                println!("{}\t{}", p.name(), registry.path(p)?.display());
            }
        }
        Action::Picker => {
            let mut entries = picker::entries(&registry, &herdr.workspaces()?)?;
            let remote = herdr::discover_remote(herdr);
            picker::append_remote(&mut entries, &remote.machines);
            if let Some(index) = picker::select(&entries, &remote.warnings)? {
                picker::activate(herdr, &entries[index])?;
            }
        }
        Action::AddCurrent => {
            let id = std::env::var("HERDR_WORKSPACE_ID")
                .map_err(|_| eyre!("HERDR_WORKSPACE_ID is not set"))?;
            let workspace = herdr.current_workspace(&id)?;
            let cwd = workspace
                .cwd
                .ok_or_else(|| eyre!("Workspace {} has no pane cwd available", workspace.label))?;
            let path = cwd.canonicalize()?;
            let existing = registry
                .projects
                .iter()
                .find(|p| registry.path(p).ok().as_deref() == Some(path.as_path()))
                .map(|p| p.name());
            let message = if registry.add(&path)? {
                format!(
                    "Added project: {}",
                    path.file_name().unwrap().to_string_lossy()
                )
            } else {
                format!(
                    "Project already registered: {}",
                    existing.unwrap_or_else(|| path.display().to_string())
                )
            };
            herdr.notify("Project Picker", &message)?;
            println!("{message}");
        }
    }
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let herdr = Herdr::new();
    if let Err(error) = run(cli.command, &herdr) {
        eprintln!("{error:?}");
        if std::env::var_os("HERDR_PLUGIN_ID").is_some() {
            let _ = herdr.notify("Project Picker error", &format!("{error}"));
        }
        std::process::exit(1);
    }
}
