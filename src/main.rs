mod app;
mod config;
mod diff;
mod editor;
mod git;
mod highlight;
mod keymap;
mod operation;
mod ui;
mod watch;

use std::{
    env,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
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
    let mut positional = false;
    for arg in env::args_os().skip(1) {
        if !positional && (arg == "--help" || arg == "-h") {
            println!(
                "diffy — live Git diffs with Tree-sitter highlighting\n\n\
                 Usage: diffy [PATH]\n\n\
                 PATH defaults to the current directory; subdirectories are accepted.\n\
                 Staged and unstaged changes appear together in the Changes view.\n\
                 Updates automatically when repository files or refs change.\n\
                 Standard keys: Tab focus · ↑/↓ move\n\
                       Space stage/unstage file or hunk · c commit · p pull · P push\n\
                       w worktrees · e open editor · [/] hunks · ? help · q quit\n\
                       v toggle unified / side-by-side diff\n\n\
                 Git output opens automatically: ↑/↓ or PgUp/PgDn scroll · Esc cancel running / close completed.\n\
                 Commit popup: Ctrl+S commit · Enter newline · Esc cancel.\n\
                 Worktree picker: Enter switch · Delete delete · r refresh · Esc close.\n\
                 Config: $XDG_CONFIG_HOME/diffy/config.toml (default ~/.config/diffy/config.toml).\n\
                 Set keymap = \"vim\" for Vim-style keys; the default is \"standard\".\n\
                 Set editor = \"nvim\" to override $EDITOR.\n\
                 Pull is fast-forward only. Staging/unstaging never changes working-tree files."
            );
            return Ok(());
        } else if !positional && (arg == "--version" || arg == "-V") {
            println!("diffy {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        } else if !positional && arg == "--" {
            positional = true;
        } else if !positional && arg.to_string_lossy().starts_with('-') {
            bail!("Unknown option {}. See diffy --help", arg.to_string_lossy());
        } else if path.replace(PathBuf::from(arg)).is_some() {
            bail!("Expected at most one repository path. See diffy --help");
        }
    }
    let config = config::Config::load()?;
    let repo = Repository::discover(&path.unwrap_or(env::current_dir()?))?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("An interactive terminal is required. See diffy --help");
    }
    let mut watcher = Watcher::new(&repo).context("Could not watch repository")?;
    let mut app = App::new(repo)?;
    app.keymap = config.keymap;

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
                    if let Some(path) = app.worktree_request.take()
                        && let Err(error) = switch_worktree(&path, &mut app, &mut watcher)
                    {
                        app.worktree_popup.as_mut().unwrap().error = Some(format!("{error:#}"));
                    }
                    if let Some(request) = app.edit_request.take() {
                        edit_file(request, config.editor.as_deref(), &mut app, &mut terminal)?;
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

fn switch_worktree(path: &Path, app: &mut App, watcher: &mut Watcher) -> Result<()> {
    let repo = Repository::discover(path)?;
    anyhow::ensure!(
        repo.root == path.canonicalize()? && repo.common_dir == app.repo.common_dir,
        "Selected worktree is no longer part of this repository"
    );
    // Prepare both before replacing either: failures leave the old view and watcher usable.
    let next_watcher = Watcher::new(&repo).context("Could not watch worktree")?;
    let mut next_app = App::new(repo)?;
    next_app.focus = app.focus;
    next_app.set_diff_mode(app.diff_mode);
    next_app.keymap = app.keymap;
    next_app.resize_diff(app.viewport_width, app.viewport_height);
    *watcher = next_watcher;
    *app = next_app;
    Ok(())
}

fn edit_file(
    request: editor::EditRequest,
    configured_editor: Option<&str>,
    app: &mut App,
    terminal: &mut ratatui::DefaultTerminal,
) -> Result<()> {
    let mut command = match request.command(&app.repo.root, configured_editor) {
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
        .context("Could not start editor")
        .and_then(|status| {
            if !status.success() {
                bail!("Editor exited with {status}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::Focus,
        diff::DiffMode,
        git::tests::{git, linked_repo, repo, text},
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::{fs, time::Instant};

    fn key(app: &mut App, code: KeyCode) {
        assert!(!app.handle_key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn refresh_on_change(app: &mut App, watcher: &mut Watcher) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !watcher.poll().unwrap() {
            assert!(
                Instant::now() < deadline,
                "Selected worktree was not watched"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        app.refresh();
    }

    #[test]
    fn switching_retargets_the_view_watcher_staging_editor_and_commits() {
        let (_dir, _linked, repo, linked_repo) = linked_repo();
        fs::write(repo.root.join("same.txt"), "main changes\n").unwrap();
        fs::write(linked_repo.root.join("same.txt"), "staged\n").unwrap();
        git(&linked_repo.root, &["add", "."]);
        fs::write(linked_repo.root.join("same.txt"), "unstaged\n").unwrap();
        let main_index = fs::read(repo.git_dir.join("index")).unwrap();
        let linked_index = fs::read(linked_repo.git_dir.join("index")).unwrap();
        let main_head = fs::read(repo.git_dir.join("refs/heads/main")).unwrap();
        let mut watcher = Watcher::new(&repo).unwrap();
        let mut app = App::new(repo.clone()).unwrap();
        app.focus = Focus::Diff;
        app.keymap = keymap::Keymap::Vim;
        app.set_diff_mode(DiffMode::SideBySide);
        app.resize_diff(100, 8);
        app.scroll = 5;
        key(&mut app, KeyCode::Char('w'));
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Enter);
        let path = app.worktree_request.take().unwrap();
        switch_worktree(&path, &mut app, &mut watcher).unwrap();
        assert_eq!(app.repo.root, linked_repo.root);
        assert_eq!(app.repo.git_dir, linked_repo.git_dir);
        assert_eq!(app.branch, "linked");
        assert_eq!(app.keymap, keymap::Keymap::Vim);
        assert_eq!(app.diff_mode, DiffMode::SideBySide);
        assert!(!app.view.split_rows.is_empty());
        assert!(app.focus == Focus::Diff);
        assert_eq!(
            (app.viewport_width, app.viewport_height, app.scroll),
            (100, 8, 0)
        );
        assert_eq!(
            app.files
                .iter()
                .map(|entry| entry.staged)
                .collect::<Vec<_>>(),
            vec![false, true]
        );
        assert_eq!(
            app.view.source,
            Some(("staged\n".into(), "unstaged\n".into()))
        );
        assert!(app.worktree_popup.is_none());
        assert_eq!(fs::read(repo.git_dir.join("index")).unwrap(), main_index);
        assert_eq!(
            fs::read(linked_repo.git_dir.join("index")).unwrap(),
            linked_index
        );

        fs::write(linked_repo.root.join("same.txt"), "live changes\n").unwrap();
        refresh_on_change(&mut app, &mut watcher);
        assert_eq!(
            app.view.source,
            Some(("staged\n".into(), "live changes\n".into()))
        );
        key(&mut app, KeyCode::Char('e'));
        assert_eq!(
            app.edit_request.take().unwrap().path,
            PathBuf::from("same.txt")
        );
        app.focus = Focus::Files;
        key(&mut app, KeyCode::Char(' '));
        let staged = linked_repo.staged_files().unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(
            text(linked_repo.contents(&staged[0]).unwrap().1),
            "live changes\n"
        );
        key(&mut app, KeyCode::Char('c'));
        app.handle_paste("Commit in selected worktree");
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.operation_label().is_some() {
            app.poll_operation();
            assert!(Instant::now() < deadline, "Commit did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .message()
                .starts_with("Committed:")
        );
        assert!(linked_repo.files().unwrap().is_empty());
        assert_eq!(fs::read(repo.git_dir.join("index")).unwrap(), main_index);
        assert_eq!(
            fs::read(repo.git_dir.join("refs/heads/main")).unwrap(),
            main_head
        );
        assert_eq!(
            fs::read_to_string(repo.root.join("same.txt")).unwrap(),
            "main changes\n"
        );

        key(&mut app, KeyCode::Esc); // Close the completed operation's output.
        app.set_diff_mode(DiffMode::Unified);
        key(&mut app, KeyCode::Char('w'));
        key(&mut app, KeyCode::Char('g'));
        key(&mut app, KeyCode::Enter);
        let path = app.worktree_request.take().unwrap();
        switch_worktree(&path, &mut app, &mut watcher).unwrap();
        assert_eq!(app.repo.root, repo.root);
        assert_eq!(app.branch, "main");
        assert_eq!(app.diff_mode, DiffMode::Unified);
        assert!(app.view.split_rows.is_empty());
        assert_eq!(
            app.view.source,
            Some((String::new(), "main changes\n".into()))
        );
        assert!(app.status.is_none());
        assert!(app.operation_details.is_none());
        fs::write(repo.root.join("same.txt"), "main live changes\n").unwrap();
        refresh_on_change(&mut app, &mut watcher);
        assert_eq!(
            app.view.source,
            Some((String::new(), "main live changes\n".into()))
        );
    }

    #[test]
    fn failed_switches_preserve_the_previous_snapshot_and_watcher() {
        let (_dir, _linked, main_repo, linked_repo) = linked_repo();
        let (_unrelated, unrelated_repo) = repo();
        fs::write(main_repo.root.join("main.txt"), "previous snapshot\n").unwrap();
        let mut watcher = Watcher::new(&main_repo).unwrap();
        let mut app = App::new(main_repo.clone()).unwrap();
        fs::write(linked_repo.git_dir.join("index"), "invalid index").unwrap();
        for path in [&linked_repo.root, &unrelated_repo.root] {
            assert!(switch_worktree(path, &mut app, &mut watcher).is_err());
            assert_eq!(app.repo.root, main_repo.root);
            assert_eq!(
                app.view.source,
                Some((String::new(), "previous snapshot\n".into()))
            );
        }
        fs::remove_dir_all(&linked_repo.root).unwrap();
        assert!(switch_worktree(&linked_repo.root, &mut app, &mut watcher).is_err());
        assert_eq!(app.branch, "main");
        fs::write(main_repo.root.join("main.txt"), "still watching\n").unwrap();
        refresh_on_change(&mut app, &mut watcher);
        assert_eq!(
            app.view.source,
            Some((String::new(), "still watching\n".into()))
        );
    }
}
