# Configuration guide

[Getting started](../README.md#getting-started) · [Usage guide](usage.md)

## Configuration file

Diffy reads `$XDG_CONFIG_HOME/diffy/config.toml`, falling back to
`~/.config/diffy/config.toml` when `XDG_CONFIG_HOME` is unset, empty, or relative.
A missing config file uses the defaults: the standard keymap and `$EDITOR`.

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

Set `editor` in the config file to choose an editor for diffy:

```toml
editor = "nvim"
```

For VS Code, use `editor = "code --wait"`. Restart diffy to apply your settings.

The `editor` setting takes precedence over the `EDITOR` environment variable. When
`editor` is omitted, diffy falls back to `EDITOR`, which you can set before launching:

```sh
export EDITOR=nvim
```

Arguments and quoted executable paths are supported; shell expansions and pipelines
are not evaluated. If neither setting is set, or the selected command is empty or
invalid, diffy shows an error without leaving the interface. An invalid `editor`
setting does not fall back to `EDITOR`.

Vim/Neovim, nano, Emacs, and other `+LINE` editors are supported, along with Helix,
VS Code/VSCodium, Sublime Text, and Zed. The latter GUI editors receive `--wait`
automatically; configure any other GUI editor to stay in the foreground.

See [opening files](usage.md#open-a-file-in-your-editor) for editor shortcuts and
line positioning.
