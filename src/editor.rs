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
    pub fn command(&self, root: &Path, configured_editor: Option<&str>) -> Result<Command> {
        let editor = match configured_editor {
            Some(editor) => editor.to_owned(),
            None => env::var("EDITOR")
                .context("Set editor in config or $EDITOR to the editor you want to use")?,
        };
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
    let words = shlex::split(editor).context("Invalid quoting in editor command")?;
    let (program, args) = words.split_first().context("Editor command is empty")?;
    if program.is_empty() {
        bail!("Editor command is empty");
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
    fn configured_editor_takes_precedence_and_otherwise_uses_editor_environment() {
        // Isolate environment changes in subprocesses so parallel tests stay safe.
        const CHILD: &str = "DIFFY_TEST_EDITOR_CHILD";
        if env::var_os(CHILD).is_none() {
            for editor in [None, Some("nano -w"), Some(""), Some("nano '")] {
                let mut child = Command::new(env::current_exe().unwrap());
                child
                    .args([
                        "--exact",
                        "editor::tests::configured_editor_takes_precedence_and_otherwise_uses_editor_environment",
                        "--nocapture",
                    ])
                    .env(CHILD, "1");
                match editor {
                    Some(editor) => child.env("EDITOR", editor),
                    None => child.env_remove("EDITOR"),
                };
                let output = child.output().unwrap();
                assert!(output.status.success(), "{editor:?}: {output:?}");
            }
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a file.rs"), "contents\n").unwrap();
        let request = EditRequest {
            path: "a file.rs".into(),
            line: Some(42),
        };
        let command = request.command(dir.path(), Some("nvim -f")).unwrap();
        assert_eq!(command.get_program(), "nvim");
        assert_eq!(command.get_current_dir(), Some(dir.path()));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                std::ffi::OsStr::new("-f"),
                std::ffi::OsStr::new("+42"),
                dir.path().join("a file.rs").as_os_str(),
            ]
        );

        let fallback = request.command(dir.path(), None);
        match env::var("EDITOR").ok().as_deref() {
            Some("nano -w") => {
                let command = fallback.unwrap();
                assert_eq!(command.get_program(), "nano");
                assert_eq!(
                    command.get_args().collect::<Vec<_>>(),
                    [
                        std::ffi::OsStr::new("-w"),
                        std::ffi::OsStr::new("+42"),
                        dir.path().join("a file.rs").as_os_str(),
                    ]
                );
            }
            editor => {
                let expected = match editor {
                    None => "Set editor in config or $EDITOR to the editor you want to use",
                    Some("") => "Editor command is empty",
                    Some("nano '") => "Invalid quoting in editor command",
                    _ => unreachable!(),
                };
                assert_eq!(fallback.unwrap_err().to_string(), expected);
            }
        }
        for (editor, expected) in [
            ("", "Editor command is empty"),
            ("nvim '", "Invalid quoting in editor command"),
        ] {
            assert_eq!(
                request
                    .command(dir.path(), Some(editor))
                    .unwrap_err()
                    .to_string(),
                expected
            );
        }
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
        for (editor, expected) in [
            ("", "Editor command is empty"),
            ("  ", "Editor command is empty"),
            ("''", "Editor command is empty"),
            ("vim '", "Invalid quoting in editor command"),
        ] {
            assert_eq!(
                editor_command(editor, Path::new("/repo/file"), None)
                    .unwrap_err()
                    .to_string(),
                expected
            );
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
