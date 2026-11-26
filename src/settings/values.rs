//! Validated scalar values shared by persistence, editor runtime, and settings controls.

use crate::document::Newline;
use std::time::Duration;

/// Individual configurable scalar fields, in display order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scalar {
    Checkpoint,
    Document,
    Line,
    Timeout,
    FindMatchRows,
}

impl Scalar {
    pub const ALL: [Self; 5] = [
        Self::Checkpoint,
        Self::Document,
        Self::Line,
        Self::Timeout,
        Self::FindMatchRows,
    ];

    pub const fn table(self) -> &'static str {
        match self {
            Self::Checkpoint => "recovery",
            Self::FindMatchRows => "editor",
            _ => "highlighting",
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint_interval_seconds",
            Self::Document => "max_document_bytes",
            Self::Line => "max_line_bytes",
            Self::Timeout => "parse_timeout_ms",
            Self::FindMatchRows => "find_match_rows",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Checkpoint => "Checkpoint interval (s)",
            Self::Document => "Document limit (B)",
            Self::Line => "Line limit (bytes)",
            Self::Timeout => "Parse timeout (ms)",
            Self::FindMatchRows => "Find match rows",
        }
    }

    pub const fn default(self) -> u64 {
        match self {
            Self::Checkpoint => 5,
            Self::Document => 20 * 1024 * 1024,
            Self::Line => 64 * 1024,
            Self::Timeout => 250,
            Self::FindMatchRows => 3,
        }
    }

    pub const fn maximum(self) -> u64 {
        match self {
            Self::Checkpoint => 3600,
            Self::FindMatchRows => 10,
            _ => self.default(),
        }
    }

    /// Report a qualified diagnostic and reuse the same range for UI and TOML.
    pub fn validate(self, value: Option<u64>) -> Result<u64, String> {
        value
            .filter(|value| (1..=self.maximum()).contains(value))
            .ok_or_else(|| {
                format!(
                    "{}.{}: expected an integer from 1 to {}",
                    self.table(),
                    self.id(),
                    self.maximum()
                )
            })
    }
}

/// An explicit policy for access to external clipboards.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ClipboardMode {
    Internal,
    #[default]
    Automatic,
}

impl ClipboardMode {
    /// Stable configuration value.
    pub fn id(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Automatic => "both",
        }
    }

    /// Read a stable configuration value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "internal" => Some(Self::Internal),
            "system" | "both" => Some(Self::Automatic),
            _ => None,
        }
    }
}

/// Explicit overrides are separate from their effective defaults.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Values {
    scalars: [Option<u64>; Scalar::ALL.len()],
    pub(crate) clipboard_override: Option<ClipboardMode>,
    /// Fallback for documents with no LF or CRLF yet.
    pub(crate) default_line_ending: Option<Newline>,
}

impl Values {
    pub fn set(&mut self, setting: Scalar, value: Option<u64>) {
        self.scalars[setting as usize] = value;
    }

    pub fn override_value(self, setting: Scalar) -> Option<u64> {
        self.scalars[setting as usize]
    }

    pub fn get(self, setting: Scalar) -> u64 {
        self.override_value(setting)
            .unwrap_or_else(|| setting.default())
    }

    pub fn checkpoint(self) -> Duration {
        Duration::from_secs(self.get(Scalar::Checkpoint))
    }

    /// Effective normal-session destination.
    pub fn clipboard_mode(self) -> ClipboardMode {
        self.clipboard_override.unwrap_or_default()
    }

    /// Effective line ending for documents with no detected style.
    pub fn default_line_ending(self) -> Newline {
        self.default_line_ending.unwrap_or_default()
    }

    pub fn syntax(self) -> SyntaxLimits {
        SyntaxLimits {
            document: self.get(Scalar::Document),
            line: self.get(Scalar::Line) as usize,
            timeout: Duration::from_millis(self.get(Scalar::Timeout)),
        }
    }
}

/// Analysis limits can be tightened inside the fixed syntax worker resource envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SyntaxLimits {
    pub document: u64,
    pub line: usize,
    pub timeout: Duration,
}

impl Default for SyntaxLimits {
    fn default() -> Self {
        Values::default().syntax()
    }
}
