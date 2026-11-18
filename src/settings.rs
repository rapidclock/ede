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
    /// Keep settings I/O isolated from process environment in integration-style unit tests.
    #[cfg(test)]
    pub(crate) fn isolated(directory: PathBuf) -> Self {
        Self {
            directory: Some(directory),
            safe: false,
        }
    }

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

    /// Applied settings retire work dispatched before disable, even after re-enable.
    #[test]
    fn clipboard_opt_in_is_revocable_across_applied_revisions() {
        use std::time::{Duration, Instant};

        let directory = tempfile::tempdir().unwrap();
        let mut app = crate::app::AppState::new();
        app.config_location = Location {
            directory: Some(directory.path().to_path_buf()),
            safe: false,
        };
        let apply = |app: &mut crate::app::AppState, enabled| {
            std::fs::write(
                directory.path().join("config.toml"),
                format!("[clipboard]\ncopy_to_system = {enabled}\n"),
            )
            .unwrap();
            let generation = app.settings_generation;
            app.settings.reload();
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.settings_generation == generation {
                app.poll_settings(None);
                assert!(
                    Instant::now() < deadline,
                    "clipboard revision did not apply"
                );
                std::thread::yield_now();
            }
        };
        let disabled_dispatch = app.system_clipboard_epoch();
        apply(&mut app, true);
        let enabled_dispatch = app.system_clipboard_epoch();
        assert!(enabled_dispatch.is_some());
        assert_ne!(disabled_dispatch, enabled_dispatch);
        apply(&mut app, false);
        assert_eq!(app.system_clipboard_epoch(), None);
        apply(&mut app, true);
        assert!(app.system_clipboard_epoch().is_some());
        assert_ne!(
            enabled_dispatch,
            app.system_clipboard_epoch(),
            "disabling left an old clipboard request authorized"
        );
        let before_suspend = app.system_clipboard_epoch();
        app.revoke_clipboard();
        assert_ne!(before_suspend, app.system_clipboard_epoch());
    }

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
