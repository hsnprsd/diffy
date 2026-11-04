mod app;
mod diff;
mod git;
mod highlight;
mod ui;
mod watch;

use std::{
    env,
    io::{self, IsTerminal},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyEventKind};

use crate::{app::App, git::Repository, watch::Watcher};

fn main() -> Result<()> {
    let mut path = None;
    let mut no_watch = false;
    let mut base_branch = None;
    let mut positional = false;
    let mut args = env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if !positional && (arg == "--help" || arg == "-h") {
            println!(
                "diffy — live Git diffs with Tree-sitter highlighting\n\n\
                 Usage: diffy [--no-watch] [--base BRANCH] [PATH]\n\n\
                 PATH defaults to the current directory; subdirectories are accepted.\n\
                 --no-watch     Disable filesystem watching (use r to refresh)\n\
                 --base, -b REF Compare merge-base(REF, HEAD) to HEAD (committed only)\n\n\
                 Keys: Tab focus · j/k scroll · n/p files · s cycle views\n\
                       b select base branch · [/] hunks · / filter · ? help · q quit\n\n\
                 Read-only: never stages, discards, or changes your files."
            );
            return Ok(());
        } else if !positional && (arg == "--version" || arg == "-V") {
            println!("diffy {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        } else if !positional && arg == "--no-watch" {
            no_watch = true;
        } else if !positional
            && (arg == "--base"
                || arg == "-b"
                || arg
                    .to_str()
                    .is_some_and(|value| value.starts_with("--base=")))
        {
            let value = match arg.to_str().and_then(|value| value.strip_prefix("--base=")) {
                Some(value) => value.to_owned(),
                None => args
                    .next()
                    .context("Expected a branch after --base. See diffy --help")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("Base branch must be UTF-8. See diffy --help"))?,
            };
            if value.trim().is_empty() || value.starts_with('-') {
                bail!("Expected a branch after --base. See diffy --help");
            }
            if base_branch.replace(value).is_some() {
                bail!("Expected at most one --base branch. See diffy --help");
            }
        } else if !positional && arg == "--" {
            positional = true;
        } else if !positional && arg.to_string_lossy().starts_with('-') {
            bail!("Unknown option {}. See diffy --help", arg.to_string_lossy());
        } else if path.replace(PathBuf::from(arg)).is_some() {
            bail!("Expected at most one repository path. See diffy --help");
        }
    }
    let repo = Repository::discover(&path.unwrap_or(env::current_dir()?))?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("An interactive terminal is required. See diffy --help");
    }
    let mut watcher = if no_watch {
        None
    } else {
        Some(Watcher::new(&repo).context("Could not watch repository; try --no-watch")?)
    };
    let mut app = App::new(repo, base_branch)?;

    // Ratatui's panic hook and this guard both restore the terminal on exit.
    let _restore = RestoreTerminal;
    let mut terminal = ratatui::try_init().context("Could not initialize terminal")?;
    let mut dirty = true;
    loop {
        let update = watcher.as_mut().map(Watcher::poll).transpose();
        match update {
            Ok(Some(true)) => {
                app.refresh();
                dirty = true;
            }
            Err(error) => {
                app.error = Some(format!("Watcher: {error}; r refreshes manually"));
                watcher = None;
                dirty = true;
            }
            _ => {}
        }
        if dirty {
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if app.handle_key(key) {
                        break;
                    }
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }
    }
    Ok(())
}

struct RestoreTerminal;

impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        ratatui::restore();
        // Ensure an error printed by main starts on its own line.
        let _ = io::Write::flush(&mut io::stdout());
    }
}
