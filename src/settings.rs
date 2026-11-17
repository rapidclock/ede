//! Machine-local configuration discovery, shared by general and syntax settings.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::OnceLock,
};

mod file;
mod keys;
mod service;
mod values;

pub(crate) use file::Base;
pub(crate) use keys::{Chord, Context};
pub(crate) use service::{SaveState, Service, Update};
pub(crate) use values::{Scalar, SyntaxLimits, Values};

/// An indivisible set of effective settings plus the explicit overrides used by the UI.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Revision {
    pub map: keys::Keymap,
    pub values: Values,
}

/// Keep the expected location available to the UI even when user I/O is disabled.
#[derive(Default)]
pub(crate) struct Location {
    directory: Option<PathBuf>,
    safe: bool,
}

impl Location {
    /// Capture the process's configuration directory before starting workers.
    pub(crate) fn discover(safe: bool) -> Self {
        static DIRECTORY: OnceLock<Option<PathBuf>> = OnceLock::new();
        let directory = DIRECTORY.get_or_init(|| {
            resolve(
                std::env::var_os("XDG_CONFIG_HOME"),
                std::env::var_os("HOME"),
            )
        });
        Self {
            directory: directory.clone(),
            safe,
        }
    }

    /// Whether user configuration is disabled for the entire session.
    pub(crate) fn safe(&self) -> bool {
        self.safe
    }

    /// Return a read root only when user configuration is enabled.
    pub(crate) fn user_directory(&self) -> Option<&Path> {
        self.directory.as_deref().filter(|_| !self.safe)
    }

    /// Display the expected config path, escaping controls and non-UTF-8 bytes.
    pub(crate) fn display_file(&self) -> Option<String> {
        let path = self.directory.as_ref()?.join("config.toml");
        Some(crate::text::safe(&format!("{path:?}")))
    }
}

/// Both environment bases must be absolute. Resolution does not touch disk.
fn resolve(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    xdg.map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        })
        .map(|path| path.join("ede"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_only_absolute_xdg_or_home_bases() {
        for (xdg, home, expected) in [
            (Some("/xdg"), Some("/home"), Some("/xdg/ede")),
            (Some("/xdg"), None, Some("/xdg/ede")),
            (Some(""), Some("/home"), Some("/home/.config/ede")),
            (Some("relative"), Some("/home"), Some("/home/.config/ede")),
            (None, Some("/home"), Some("/home/.config/ede")),
            (Some("relative"), Some(""), None),
            (None, Some("relative"), None),
            (None, None, None),
        ] {
            let directory = resolve(xdg.map(Into::into), home.map(Into::into));
            assert_eq!(directory.as_deref(), expected.map(Path::new));
        }
    }

    #[test]
    fn safe_location_retains_display_path_without_granting_a_read_root() {
        for safe in [false, true] {
            let location = Location {
                directory: Some("/xdg/ede".into()),
                safe,
            };
            assert_eq!(
                location.display_file().as_deref(),
                Some("\"/xdg/ede/config.toml\"")
            );
            assert_eq!(
                location.user_directory(),
                (!safe).then_some(Path::new("/xdg/ede"))
            );
        }
    }

    #[test]
    fn config_path_display_preserves_native_bytes_safely() {
        use std::os::unix::ffi::OsStringExt;

        let location = Location {
            directory: Some(OsString::from_vec(b"/config-\xff-\x1b/ede".to_vec()).into()),
            safe: true,
        };
        assert_eq!(
            location.display_file().as_deref(),
            Some(r#""/config-\xFF-\u{1b}/ede/config.toml""#)
        );
    }
}
