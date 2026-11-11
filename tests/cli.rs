use std::process::Command;

fn diffy() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_diffy"));
    command.env_remove("XDG_CONFIG_HOME").env_remove("HOME");
    command
}

#[test]
fn help_and_version_do_not_need_a_repository_or_terminal() {
    let dir = tempfile::tempdir().unwrap();
    for (arg, expected) in [
        ("--help", "Usage: diffy [OPTIONS]"),
        ("--version", "diffy 0.1.0"),
    ] {
        let output = diffy().arg(arg).current_dir(dir.path()).output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn config_is_loaded_from_xdg_or_home_before_starting_the_ui() {
    let dir = tempfile::tempdir().unwrap();
    for use_xdg in [false, true] {
        let base = if use_xdg {
            dir.path().join("xdg")
        } else {
            dir.path().join(".config")
        };
        let config_dir = base.join("diffy");
        std::fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join("config.toml");
        std::fs::write(&path, "keymap = \"unknown\"\n").unwrap();
        let mut command = diffy();
        command.env("HOME", dir.path()).current_dir(dir.path());
        if use_xdg {
            command.env("XDG_CONFIG_HOME", &base);
        }
        let output = command.output().unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(
            error.contains(&format!("Invalid config in {}", path.display())),
            "{error}"
        );
        assert!(error.contains("unknown variant"), "{error}");

        for option in ["--help", "--version"] {
            let output = diffy()
                .env("XDG_CONFIG_HOME", &base)
                .arg(option)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        for preset in ["standard", "vim"] {
            std::fs::write(
                &path,
                format!("keymap = \"{preset}\"\neditor = \"nvim -f\"\n"),
            )
            .unwrap();
            let output = command.output().unwrap();
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("Not inside a Git working tree")
            );
        }
    }
}

#[test]
fn unknown_theme_reports_the_config_path_and_builtin_names_before_startup() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("diffy");
    std::fs::create_dir_all(&config_dir).unwrap();
    let path = config_dir.join("config.toml");
    std::fs::write(&path, "theme = \"custom\"\n").unwrap();
    let output = diffy()
        .env("XDG_CONFIG_HOME", dir.path())
        .current_dir(dir.path())
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    for expected in [
        format!("Invalid config in {}", path.display()),
        "unknown variant `custom`".into(),
        "`default`".into(),
        "`github-light`".into(),
    ] {
        assert!(error.contains(&expected), "{error}");
    }
}

#[test]
fn invalid_arguments_fail_before_starting_the_ui() {
    for args in [vec!["--invalid"], vec!["first", "second"]] {
        let output = diffy().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("See diffy --help"));
    }
}

#[test]
fn missing_repository_has_an_actionable_error() {
    let dir = tempfile::tempdir().unwrap();
    let output = diffy().current_dir(dir.path()).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Not inside a Git working tree"));
}

#[test]
fn redirected_input_is_rejected_without_terminal_escape_sequences() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .status()
            .unwrap()
            .success()
    );
    let subdir = dir.path().join("nested");
    std::fs::create_dir(&subdir).unwrap();
    for cwd in [dir.path(), subdir.as_path()] {
        let output = diffy().current_dir(cwd).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "Error: An interactive terminal is required. See diffy --help"
        );
    }
}
