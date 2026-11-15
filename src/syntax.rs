//! Language registration and syntax metadata shared by the editor and analysis worker.

mod config;
mod engine;
mod limits;
mod projection;
mod wire;
mod worker;

pub(crate) use config::{Config, Foreground, Palette};
pub(crate) use limits::DOCUMENT as DOCUMENT_LIMIT;
pub(crate) use projection::{Projection, Styled, ViewKey};
pub use wire::serve;
pub(crate) use worker::{
    ProcessControl, Session, WORK_CHARGE, Work, WorkerResult, read_config, validate_config,
};

use std::path::Path;

/// Native suffixes, stable configuration identifiers, and display names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Language {
    pub(crate) id: &'static str,
    label: &'static str,
    extensions: &'static [&'static str],
}

impl Language {
    /// Build one registration. Built-in suffixes also serve as fence aliases.
    const fn new(
        id: &'static str,
        label: &'static str,
        extensions: &'static [&'static str],
    ) -> Self {
        Self {
            id,
            label,
            extensions,
        }
    }

    /// Native extension lookup preserves non-UTF-8 path stems.
    fn extension(extension: &[u8]) -> Option<&'static Self> {
        LANGUAGES.iter().find(|entry| {
            entry
                .extensions
                .iter()
                .any(|suffix| extension.eq_ignore_ascii_case(suffix.as_bytes()))
        })
    }

    /// Explicit identifiers in config are canonical lower-case names.
    fn by_id(id: &str) -> Option<&'static Self> {
        LANGUAGES.iter().find(|entry| entry.id == id)
    }
}

/// Registrations are shared by detection, the picker and worker protocol.
const LANGUAGES: &[Language] = &[
    Language::new("markdown", "Markdown", &["md", "markdown"]),
    Language::new("toml", "TOML", &["toml"]),
    Language::new("yaml", "YAML", &["yaml", "yml"]),
    Language::new("json", "JSON", &["json"]),
    Language::new("javascript", "JavaScript", &["js", "mjs", "cjs"]),
    Language::new("typescript", "TypeScript", &["ts", "mts", "cts"]),
    Language::new("python", "Python", &["py", "pyi"]),
    Language::new("rust", "Rust", &["rs"]),
    Language::new("ini", "INI", &["ini"]),
    Language::new("xml", "XML", &["xml"]),
    Language::new("html", "HTML", &["html", "htm"]),
    Language::new("css", "CSS", &["css"]),
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

    /// Resolve the actual parser from this choice and the active associations.
    pub(crate) fn effective(
        self,
        path: Option<&Path>,
        config: &Config,
    ) -> Option<&'static Language> {
        match self {
            Self::Auto => path.and_then(|path| config.detect(path)),
            Self::Plain => None,
            Self::Syntax(language) => Some(language),
        }
    }

    /// Effective language label for the picker and Tab Details.
    pub(crate) fn name(self, path: Option<&Path>, config: &Config) -> &'static str {
        self.effective(path, config)
            .map_or("Plain Text", |language| language.label)
    }
}

/// Semantic foreground categories accepted in themes and custom queries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum Category {
    Comment,
    String,
    Number,
    Keyword,
    Type,
    Property,
    Markup,
    Function,
    Constant,
    Operator,
    Punctuation,
    Variable,
    Tag,
    Attribute,
    Code,
}

impl Category {
    /// Canonical theme names and wire order.
    const ALL: &[(Self, &str)] = &[
        (Self::Comment, "comment"),
        (Self::String, "string"),
        (Self::Number, "number"),
        (Self::Keyword, "keyword"),
        (Self::Type, "type"),
        (Self::Property, "property"),
        (Self::Markup, "markup"),
        (Self::Function, "function"),
        (Self::Constant, "constant"),
        (Self::Operator, "operator"),
        (Self::Punctuation, "punctuation"),
        (Self::Variable, "variable"),
        (Self::Tag, "tag"),
        (Self::Attribute, "attribute"),
        (Self::Code, "code"),
    ];

    /// Read a canonical name from user configuration.
    fn named(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, category)| *category == name)
            .map(|(category, _)| *category)
    }

    /// Decode a checked helper reply.
    fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL
            .get(usize::from(byte))
            .map(|(category, _)| *category)
    }
}

/// Short reasons retained independently of transient editor status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fallback {
    Size,
    Line,
    Timeout,
    Read,
    Worker,
    Startup,
    Transfer,
    Memory,
    Output,
    Cancelled,
    Busy,
}

impl Fallback {
    /// Only size and line fallback are reconsidered on ordinary edits.
    pub(crate) fn revision_specific(self) -> bool {
        matches!(self, Self::Size | Self::Line)
    }

    /// Text displayed alongside search, save and refresh state.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Size => "document exceeds 20 MiB",
            Self::Line => "a line exceeds 64 KiB",
            Self::Timeout => "analysis exceeded 250 ms",
            Self::Read => "snapshot read failed",
            Self::Worker => "analysis worker failed",
            Self::Startup => "analysis worker startup timed out",
            Self::Transfer => "analysis input transfer timed out",
            Self::Memory => "analysis memory limit reached",
            Self::Output => "invalid or over-budget analysis output",
            Self::Cancelled => "analysis cancelled",
            Self::Busy => "waiting for shared storage capacity",
        }
    }
}

/// Syntax preparation status for the active tab.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Status {
    #[default]
    Plain,
    Preparing,
    Ready,
    Fallback(Fallback),
}

impl Status {
    /// Describe the active analysis without replacing other editor notices.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Plain => "Plain Text",
            Self::Preparing => "preparing",
            Self::Ready => "highlighted",
            Self::Fallback(reason) => reason.message(),
        }
    }
}
