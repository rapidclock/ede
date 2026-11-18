//! Validated scalar values shared by persistence, editor runtime, and settings controls.

use std::time::Duration;

/// Individual configurable scalar fields, in display order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scalar {
    Checkpoint,
    Document,
    Line,
    Timeout,
}

impl Scalar {
    pub const ALL: [Self; 4] = [Self::Checkpoint, Self::Document, Self::Line, Self::Timeout];

    pub const fn table(self) -> &'static str {
        match self {
            Self::Checkpoint => "recovery",
            _ => "highlighting",
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint_interval_seconds",
            Self::Document => "max_document_bytes",
            Self::Line => "max_line_bytes",
            Self::Timeout => "parse_timeout_ms",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Checkpoint => "Checkpoint interval (s)",
            Self::Document => "Document limit (bytes)",
            Self::Line => "Line limit (bytes)",
            Self::Timeout => "Analysis deadline (ms)",
        }
    }

    pub const fn default(self) -> u64 {
        match self {
            Self::Checkpoint => 5,
            Self::Document => 20 * 1024 * 1024,
            Self::Line => 64 * 1024,
            Self::Timeout => 250,
        }
    }

    pub const fn maximum(self) -> u64 {
        match self {
            Self::Checkpoint => 3600,
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

/// Destinations for a document selection copied by ede.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ClipboardMode {
    Internal,
    System,
    #[default]
    Both,
}

impl ClipboardMode {
    /// Stable configuration value.
    pub fn id(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::System => "system",
            Self::Both => "both",
        }
    }

    /// Read a stable configuration value.
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Internal, Self::System, Self::Both]
            .into_iter()
            .find(|mode| mode.id() == value)
    }

    /// Name displayed in Settings.
    pub fn label(self) -> &'static str {
        match self {
            Self::Internal => "Internal",
            Self::System => "System",
            Self::Both => "Both",
        }
    }

    /// Advance the Settings row by one choice.
    pub fn next(self) -> Self {
        match self {
            Self::Internal => Self::System,
            Self::System => Self::Both,
            Self::Both => Self::Internal,
        }
    }
}

/// Explicit overrides are separate from their effective defaults.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Values {
    scalars: [Option<u64>; 4],
    pub(crate) clipboard_override: Option<ClipboardMode>,
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
