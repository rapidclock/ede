//! Built-in syntax names, associations and per-tab language selection.

use std::{ffi::OsStr, path::Path};

/// One supported language and the final filename extensions it claims in Auto.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Language(&'static str, &'static [&'static [u8]]);

/// Built-in entries, also used as the picker order.
const LANGUAGES: &[Language] = &[
    Language("Markdown", &[b"md", b"markdown"]),
    Language("TOML", &[b"toml"]),
    Language("YAML", &[b"yaml", b"yml"]),
    Language("JSON", &[b"json"]),
    Language("JavaScript", &[b"js", b"mjs", b"cjs"]),
    Language("TypeScript", &[b"ts", b"mts", b"cts"]),
    Language("Python", &[b"py", b"pyi"]),
    Language("Rust", &[b"rs"]),
    Language("INI", &[b"ini"]),
    Language("XML", &[b"xml"]),
    Language("HTML", &[b"html", b"htm"]),
    Language("CSS", &[b"css"]),
];

/// Auto follows the committed filename. Manual choices last until the tab closes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Choice {
    #[default]
    Auto,
    Plain,
    Syntax(&'static Language),
}

impl Choice {
    /// Iterate over the picker options in display order.
    pub(crate) fn options() -> impl Iterator<Item = Self> {
        [Self::Auto, Self::Plain]
            .into_iter()
            .chain(LANGUAGES.iter().map(Self::Syntax))
    }

    /// Effective language label using the committed path when in Auto.
    pub(crate) fn name(self, path: Option<&Path>) -> &'static str {
        match self {
            Self::Auto => detect(path),
            Self::Plain => "Plain Text",
            Self::Syntax(language) => language.0,
        }
    }
}

/// Detect a language using only the final native extension.
fn detect(path: Option<&Path>) -> &'static str {
    let Some(extension) = path.and_then(Path::extension).map(OsStr::as_encoded_bytes) else {
        return "Plain Text";
    };
    LANGUAGES
        .iter()
        .find(|Language(_, extensions)| {
            extensions
                .iter()
                .any(|item| extension.eq_ignore_ascii_case(item))
        })
        .map_or("Plain Text", |Language(label, _)| *label)
}
