use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Keymap {
    #[default]
    Standard,
    Vim,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Down,
    Up,
    PageDown,
    PageUp,
    First,
    Last,
    Left,
    Right,
    Stage,
    Commit,
    Pull,
    Push,
    Edit,
    Help,
    Quit,
    Worktrees,
    Close,
    Select,
    DeleteWorktree,
    RefreshWorktrees,
    ConfirmRemoval,
    NextHunk,
    PreviousHunk,
    ToggleView,
    NextTheme,
}

const ACTIONS: [Action; 25] = [
    Action::Down,
    Action::Up,
    Action::PageDown,
    Action::PageUp,
    Action::First,
    Action::Last,
    Action::Left,
    Action::Right,
    Action::Stage,
    Action::Commit,
    Action::Pull,
    Action::Push,
    Action::Edit,
    Action::Help,
    Action::Quit,
    Action::Worktrees,
    Action::Close,
    Action::Select,
    Action::DeleteWorktree,
    Action::RefreshWorktrees,
    Action::ConfirmRemoval,
    Action::NextHunk,
    Action::PreviousHunk,
    Action::ToggleView,
    Action::NextTheme,
];

// Some terminals report Shift for uppercase letters and punctuation, others only
// report the resulting character. These are the same shortcut, not extra aliases.
pub fn plain_code(key: KeyEvent) -> Option<KeyCode> {
    if key.modifiers.is_empty()
        || (key.modifiers == KeyModifiers::SHIFT
            && matches!(key.code, KeyCode::Char(ch) if ch.is_uppercase() || ch.is_ascii_punctuation()))
    {
        Some(key.code)
    } else {
        None
    }
}

impl Keymap {
    pub fn action(self, key: KeyEvent) -> Option<Action> {
        let code = plain_code(key)?;
        ACTIONS
            .into_iter()
            .find(|&action| self.binding(action).0 == code)
    }

    pub fn label(self, action: Action) -> &'static str {
        self.binding(action).1
    }

    fn binding(self, action: Action) -> (KeyCode, &'static str) {
        use Action::*;
        use KeyCode::{
            Char, Delete, Down as ArrowDown, End, Enter, Esc, Home, Left as ArrowLeft,
            PageDown as PgDown, PageUp as PgUp, Right as ArrowRight, Up as ArrowUp,
        };
        match (self, action) {
            (Self::Standard, Down) => (ArrowDown, "↓"),
            (Self::Standard, Up) => (ArrowUp, "↑"),
            (Self::Standard, PageDown) => (PgDown, "PgDn"),
            (Self::Standard, PageUp) => (PgUp, "PgUp"),
            (Self::Standard, First) => (Home, "Home"),
            (Self::Standard, Last) => (End, "End"),
            (Self::Standard, Left) => (ArrowLeft, "←"),
            (Self::Standard, Right) => (ArrowRight, "→"),
            (Self::Standard, DeleteWorktree) => (Delete, "Delete"),
            (Self::Vim, Down) => (Char('j'), "j"),
            (Self::Vim, Up) => (Char('k'), "k"),
            (Self::Vim, PageDown) => (Char('d'), "d"),
            (Self::Vim, PageUp) => (Char('u'), "u"),
            (Self::Vim, First) => (Char('g'), "g"),
            (Self::Vim, Last) => (Char('G'), "G"),
            (Self::Vim, Left) => (Char('h'), "h"),
            (Self::Vim, Right) => (Char('l'), "l"),
            (Self::Vim, DeleteWorktree) => (Char('D'), "D"),
            (_, Stage) => (Char(' '), "Space"),
            (_, Commit) => (Char('c'), "c"),
            (_, Pull) => (Char('p'), "p"),
            (_, Push) => (Char('P'), "P"),
            (_, Edit) => (Char('e'), "e"),
            (_, Help) => (Char('?'), "?"),
            (_, Quit) => (Char('q'), "q"),
            (_, Worktrees) => (Char('w'), "w"),
            (_, Close) => (Esc, "Esc"),
            (_, Select) => (Enter, "Enter"),
            (_, RefreshWorktrees) => (Char('r'), "r"),
            (_, ConfirmRemoval) => (Char('y'), "y"),
            (_, NextHunk) => (Char(']'), "]"),
            (_, PreviousHunk) => (Char('['), "["),
            (_, ToggleView) => (Char('v'), "v"),
            (_, NextTheme) => (Char('t'), "t"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_exactly_one_unique_binding_in_each_preset() {
        for keymap in [Keymap::Standard, Keymap::Vim] {
            for (index, action) in ACTIONS.into_iter().enumerate() {
                let code = keymap.binding(action).0;
                assert_eq!(
                    keymap.action(KeyEvent::new(code, KeyModifiers::NONE)),
                    Some(action)
                );
                assert!(!keymap.label(action).is_empty());
                for other in &ACTIONS[..index] {
                    assert_ne!(
                        code,
                        keymap.binding(*other).0,
                        "{keymap:?}: {action:?} and {other:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn navigation_and_delete_use_only_the_selected_preset() {
        for (action, standard, vim) in [
            (Action::Down, KeyCode::Down, KeyCode::Char('j')),
            (Action::Up, KeyCode::Up, KeyCode::Char('k')),
            (Action::PageDown, KeyCode::PageDown, KeyCode::Char('d')),
            (Action::PageUp, KeyCode::PageUp, KeyCode::Char('u')),
            (Action::First, KeyCode::Home, KeyCode::Char('g')),
            (Action::Last, KeyCode::End, KeyCode::Char('G')),
            (Action::Left, KeyCode::Left, KeyCode::Char('h')),
            (Action::Right, KeyCode::Right, KeyCode::Char('l')),
            (Action::DeleteWorktree, KeyCode::Delete, KeyCode::Char('D')),
        ] {
            for (keymap, active, inactive) in [
                (Keymap::Standard, standard, vim),
                (Keymap::Vim, vim, standard),
            ] {
                assert_eq!(
                    keymap.action(KeyEvent::new(active, KeyModifiers::NONE)),
                    Some(action)
                );
                assert_eq!(
                    keymap.action(KeyEvent::new(inactive, KeyModifiers::NONE)),
                    None
                );
            }
        }
    }

    #[test]
    fn modifiers_and_removed_aliases_do_not_create_extra_shortcuts() {
        for keymap in [Keymap::Standard, Keymap::Vim] {
            for code in [
                KeyCode::Tab,
                KeyCode::BackTab,
                KeyCode::F(1),
                KeyCode::Char('n'),
            ] {
                assert_eq!(keymap.action(KeyEvent::new(code, KeyModifiers::NONE)), None);
            }
            for action in ACTIONS {
                let code = keymap.binding(action).0;
                for modifiers in [
                    KeyModifiers::CONTROL,
                    KeyModifiers::ALT,
                    KeyModifiers::SUPER,
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ] {
                    assert_eq!(keymap.action(KeyEvent::new(code, modifiers)), None);
                }
            }
            assert_eq!(
                keymap.action(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)),
                None
            );
            assert_eq!(
                keymap.action(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT)),
                Some(Action::Help)
            );
            assert_eq!(
                keymap.action(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT)),
                Some(Action::Push)
            );
        }
    }
}
