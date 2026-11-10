mod app;
mod diff;
mod editor;
mod git;
mod highlight;
mod operation;
mod ui;
mod watch;

use std::{
    env,
    io::{self, IsTerminal},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use crossterm::{
    event::{self, EnableBracketedPaste, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, enable_raw_mode},
};

use crate::{app::App, git::Repository, watch::Watcher};

fn main() -> Result<()> {
    let mut path = None;
    let mut base_branch = None;
    let mut positional = false;
    let mut args = env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if !positional && (arg == "--help" || arg == "-h") {
            println!(
                "diffy — live Git diffs with Tree-sitter highlighting\n\n\
                 Usage: diffy [--base BRANCH] [PATH]\n\n\
                 PATH defaults to the current directory; subdirectories are accepted.\n\
                 --base, -b REF Compare merge-base(REF, HEAD) to HEAD (committed only)\n\n\
                 Staged and unstaged changes appear together in the Changes view.\n\
                 Updates automatically when repository files or refs change.\n\
                 Keys: ←/→/Tab focus · ↑/↓/j/k move · s changes/branch view\n\
                       Space stage/unstage file or hunk · c commit · p pull · P push\n\
                       e edit in $EDITOR · b select base branch · [/] hunks · ? help · q quit\n\n\
                 Git output opens automatically: ↑/↓ or PgUp/PgDn scroll · Esc cancel running / close completed.\n\
                 Commit popup: Ctrl+S commit · Enter newline · Esc cancel.\n\
                 Pull is fast-forward only. Staging/unstaging never changes working-tree files."
            );
            return Ok(());
        } else if !positional && (arg == "--version" || arg == "-V") {
            println!("diffy {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
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
    let mut watcher = Watcher::new(&repo).context("Could not watch repository")?;
    let mut app = App::new(repo, base_branch)?;

    // Ratatui's panic hook and this guard both restore the terminal on exit.
    let _restore = RestoreTerminal;
    let mut terminal = ratatui::try_init().context("Could not initialize terminal")?;
    execute!(io::stdout(), EnableBracketedPaste)?;
    let mut dirty = true;
    loop {
        dirty |= app.poll_operation();
        if watcher.poll().context("Filesystem watcher failed")? {
            app.refresh();
            dirty = true;
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
                    if let Some(request) = app.edit_request.take() {
                        edit_file(request, &mut app, &mut terminal)?;
                    }
                    dirty = true;
                }
                Event::Paste(text) => {
                    app.handle_paste(&text);
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }
    }
    Ok(())
}

fn edit_file(
    request: editor::EditRequest,
    app: &mut App,
    terminal: &mut ratatui::DefaultTerminal,
) -> Result<()> {
    let mut command = match request.command(&app.repo.root) {
        Ok(command) => command,
        Err(error) => {
            app.error = Some(format!("Editor: {error:#}"));
            return Ok(());
        }
    };
    execute!(io::stdout(), event::DisableBracketedPaste)?;
    ratatui::try_restore().context("Could not suspend terminal")?;
    let result = command
        .status()
        .context("Could not start $EDITOR")
        .and_then(|status| {
            if !status.success() {
                bail!("$EDITOR exited with {status}");
            }
            Ok(())
        });
    // Resume even when launching the editor failed. Reuse the terminal and invalidate
    // its buffers so editor output and any resize are replaced by a complete redraw.
    enable_raw_mode().context("Could not resume terminal")?;
    execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
    terminal.clear()?;
    app.refresh();
    if let Err(error) = result {
        app.error = Some(format!("Editor: {error:#}"));
    }
    Ok(())
}

struct RestoreTerminal;

impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), event::DisableBracketedPaste);
        ratatui::restore();
        // Ensure an error printed by main starts on its own line.
        let _ = io::Write::flush(&mut io::stdout());
    }
}
