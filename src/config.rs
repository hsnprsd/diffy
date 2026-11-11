use std::{
    env,
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::keymap::Keymap;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub keymap: Keymap,
}

impl Config {
    pub fn load() -> Result<Self> {
        let Some(path) = config_path(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME")) else {
            return Ok(Self::default());
        };
        Self::load_from(&path)
    }

    fn load_from(path: &Path) -> Result<Self> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => {
                return Err(error).with_context(|| format!("Could not read {}", path.display()));
            }
        };
        toml::from_str(&contents).with_context(|| format!("Invalid config in {}", path.display()))
    }
}

fn config_path(config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = config_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|path| !path.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("diffy/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_location_respects_xdg_and_falls_back_to_home() {
        let dir = tempfile::tempdir().unwrap();
        let xdg = dir.path().join("xdg");
        let home = dir.path().join("home");
        for override_path in [None, Some(OsString::new()), Some("relative".into())] {
            assert_eq!(
                config_path(override_path, Some(home.clone().into_os_string())),
                Some(home.join(".config/diffy/config.toml"))
            );
        }
        assert_eq!(
            config_path(
                Some(xdg.clone().into_os_string()),
                Some(home.into_os_string())
            ),
            Some(xdg.join("diffy/config.toml"))
        );
        assert_eq!(config_path(None, None), None);
    }

    #[test]
    fn missing_or_empty_config_uses_standard_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert_eq!(Config::load_from(&path).unwrap().keymap, Keymap::Standard);
        fs::write(&path, "# Defaults\n").unwrap();
        assert_eq!(Config::load_from(&path).unwrap().keymap, Keymap::Standard);
    }

    #[test]
    fn config_selects_each_builtin_preset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for (value, keymap) in [("standard", Keymap::Standard), ("vim", Keymap::Vim)] {
            fs::write(&path, format!("keymap = \"{value}\"\n")).unwrap();
            assert_eq!(Config::load_from(&path).unwrap().keymap, keymap);
        }
    }

    #[test]
    fn malformed_unknown_and_unreadable_configs_report_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for contents in [
            "keymap =",
            "keymap = \"emacs\"",
            "keymap = 1",
            "keymaps = \"vim\"",
        ] {
            fs::write(&path, contents).unwrap();
            let error = format!("{:#}", Config::load_from(&path).unwrap_err());
            assert!(error.contains(&path.display().to_string()), "{error}");
        }
        let error = Config::load_from(dir.path()).unwrap_err().to_string();
        assert!(error.contains("Could not read"), "{error}");
        assert!(error.contains(&dir.path().display().to_string()), "{error}");
    }
}
