# Configuration guide

[Getting started](../README.md#getting-started) · [Usage guide](usage.md)

## Configuration file

Diffy reads `$XDG_CONFIG_HOME/diffy/config.toml`, falling back to
`~/.config/diffy/config.toml` when `XDG_CONFIG_HOME` is unset, empty, or relative.
A missing config file or omitted `keymap` uses the standard preset.

## Keymap

To use Vim-style navigation, create the config file with:

```toml
keymap = "vim"
```

Use `keymap = "standard"` to select the default explicitly. Restart diffy to apply
your settings.

Invalid TOML, unknown settings, and unknown preset names report an error with the
config path before the terminal UI starts. In-app help and popup hints show only
the active preset's shortcuts; the preset stays active when reviewing another
worktree.

See the [keyboard reference](usage.md#keyboard-shortcuts) for both presets.

## Editor

Set `EDITOR` before launching diffy, for example:

```sh
export EDITOR=nvim
```

For VS Code, use `export EDITOR='code --wait'`.

Arguments and quoted executable paths are supported; shell expansions and pipelines
are not evaluated. An unset or invalid `EDITOR` shows an error without leaving diffy.

Vim/Neovim, nano, Emacs, and other `+LINE` editors are supported, along with Helix,
VS Code/VSCodium, Sublime Text, and Zed. The latter GUI editors receive `--wait`
automatically; configure any other GUI editor to stay in the foreground.

See [opening files](usage.md#open-a-file-in-your-editor) for editor shortcuts and
line positioning.
