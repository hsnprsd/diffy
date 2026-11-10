use std::process::Command;

fn diffy() -> Command {
    Command::new(env!("CARGO_BIN_EXE_diffy"))
}

#[test]
fn help_and_version_do_not_need_a_repository_or_terminal() {
    let dir = tempfile::tempdir().unwrap();
    for (arg, expected) in [("--help", "Usage: diffy"), ("--version", "diffy 0.1.0")] {
        let output = diffy().arg(arg).current_dir(dir.path()).output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn help_describes_automatic_updates_and_pane_navigation() {
    let output = diffy().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("Staged and unstaged changes appear together"));
    assert!(help.contains("Usage: diffy [PATH]"));
    assert!(help.contains("Updates automatically"));
    assert!(help.contains("←/→/Tab focus"));
    assert!(help.contains("↑/↓/j/k move"));
    assert!(help.contains("e edit in $EDITOR"));
    assert!(help.contains("w worktrees"));
    assert!(help.contains("Worktree picker: Enter switch · D/Del delete · r refresh · Esc close"));
    assert!(help.contains("Space stage/unstage file or hunk"));
    assert!(help.contains("c commit · p pull · P push"));
    assert!(help.contains("Ctrl+S commit"));
    assert!(help.contains("Pull is fast-forward only"));
    assert!(help.contains("Git output opens automatically"));
    assert!(help.contains("PgUp/PgDn scroll · Esc cancel running / close completed"));
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
    let output = diffy().arg(dir.path()).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Not inside a Git working tree"));
}

#[test]
fn removed_base_options_report_an_unknown_option() {
    for (args, option) in [
        (vec!["--base", "main", "--help"], "--base"),
        (vec!["-b", "origin/main", "--help"], "-b"),
        (vec!["--base=main", "--help"], "--base=main"),
    ] {
        let output = diffy().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            format!("Error: Unknown option {option}. See diffy --help")
        );
    }
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
    let output = diffy().arg(dir.path()).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
}
