use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

#[derive(Debug, PartialEq, Eq)]
pub struct EditRequest {
    pub path: PathBuf,
    pub line: Option<usize>,
}

impl EditRequest {
    pub fn command(&self, root: &Path) -> Result<Command> {
        let editor = env::var("EDITOR").context("Set $EDITOR to the editor you want to use")?;
        let path = root.join(&self.path);
        if !path
            .metadata()
            .with_context(|| format!("Cannot open {}", path.display()))?
            .is_file()
        {
            bail!("{} is not a regular file", path.display());
        }
        let mut command = editor_command(&editor, &path, self.line)?;
        command.current_dir(root);
        Ok(command)
    }
}

fn editor_command(editor: &str, path: &Path, line: Option<usize>) -> Result<Command> {
    // Parse quoted arguments without running a shell; filenames are always separate arguments.
    let words = shlex::split(editor).context("Invalid quoting in $EDITOR")?;
    let (program, args) = words.split_first().context("$EDITOR is empty")?;
    if program.is_empty() {
        bail!("$EDITOR is empty");
    }
    let mut command = Command::new(program);
    command.args(args);
    let name = Path::new(program)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    match name {
        "code" | "code-insiders" | "codium" => {
            command.arg("--wait");
            if line.is_some() {
                command.arg("--goto");
            }
            command.arg(path_with_line(path, line));
        }
        "subl" | "sublime_text" | "zed" => {
            command.arg("--wait").arg(path_with_line(path, line));
        }
        "hx" | "helix" => {
            command.arg(path_with_line(path, line));
        }
        _ => {
            // vi, Vim, Neovim, nano, Emacs, and many other terminal editors use +LINE.
            if let Some(line) = line {
                command.arg(format!("+{line}"));
            }
            command.arg(path);
        }
    }
    Ok(command)
}

fn path_with_line(path: &Path, line: Option<usize>) -> std::ffi::OsString {
    let mut path = path.as_os_str().to_owned();
    if let Some(line) = line {
        path.push(format!(":{line}"));
    }
    path
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn args(editor: &str, line: Option<usize>) -> Vec<OsString> {
        editor_command(editor, Path::new("/repo/a file.rs"), line)
            .unwrap()
            .get_args()
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn terminal_editors_receive_the_line_and_a_single_path_argument() {
        for editor in ["vi", "vim", "nvim", "nano", "emacs"] {
            assert_eq!(args(editor, Some(42)), ["+42", "/repo/a file.rs"]);
            assert_eq!(args(editor, None), ["/repo/a file.rs"]);
        }
    }

    #[test]
    fn preserves_editor_arguments_and_quoted_executable_paths() {
        let command = editor_command(
            "'/opt/my editor/nvim' -f --cmd 'set number'",
            Path::new("/repo/a file.rs"),
            Some(7),
        )
        .unwrap();
        assert_eq!(command.get_program(), "/opt/my editor/nvim");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-f", "--cmd", "set number", "+7", "/repo/a file.rs"]
        );
    }

    #[test]
    fn gui_editors_wait_for_the_file_to_close() {
        for editor in ["code", "code-insiders", "codium"] {
            assert_eq!(
                args(editor, Some(12)),
                ["--wait", "--goto", "/repo/a file.rs:12"]
            );
            assert_eq!(args(editor, None), ["--wait", "/repo/a file.rs"]);
        }
        for editor in ["subl", "sublime_text", "zed"] {
            assert_eq!(args(editor, Some(12)), ["--wait", "/repo/a file.rs:12"]);
        }
        for editor in ["hx", "helix"] {
            assert_eq!(args(editor, Some(12)), ["/repo/a file.rs:12"]);
        }
    }

    #[test]
    fn rejects_empty_or_invalid_editor_settings() {
        for editor in ["", "  ", "''", "vim '"] {
            assert!(editor_command(editor, Path::new("/repo/file"), None).is_err());
        }
    }

    #[test]
    fn does_not_interpret_shell_metacharacters() {
        let path = Path::new("/repo/$(touch bad); 'file'\n.rs");
        let command = editor_command("nvim", path, Some(3)).unwrap();
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [std::ffi::OsStr::new("+3"), path.as_os_str()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(OsString::from_vec(b"/repo/\xff.rs".to_vec()));
        for editor in ["nvim", "code", "hx"] {
            let command = editor_command(editor, &path, Some(9)).unwrap();
            let mut expected = path.as_os_str().to_owned();
            if editor != "nvim" {
                expected.push(":9");
            }
            assert_eq!(command.get_args().last().unwrap(), expected);
        }
    }
}
