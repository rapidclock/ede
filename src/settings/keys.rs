//! Stable command contexts and exact, normalized single-chord bindings.

use crate::command::{BINDINGS, Command, Movement};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::{collections::HashMap, fmt, str::FromStr};

/// The top input surface determines which rows are active.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Context {
    Global,
    Editor,
    Controls,
    Search,
    Picker,
    Dialog,
    Settings,
}

impl Context {
    pub const ALL: [Self; 7] = [
        Self::Global,
        Self::Editor,
        Self::Controls,
        Self::Search,
        Self::Picker,
        Self::Dialog,
        Self::Settings,
    ];

    /// Stable keybindings table name.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Editor => "editor",
            Self::Controls => "controls",
            Self::Search => "search",
            Self::Picker => "picker",
            Self::Dialog => "dialog",
            Self::Settings => "settings",
        }
    }

    /// Modals never dispatch to the covered workspace.
    pub const fn has_global(self) -> bool {
        matches!(self, Self::Editor | Self::Controls | Self::Search)
    }
}

/// Exact modifiers and an ASCII letter/digit or named key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Chord(KeyCode, KeyModifiers);

use KeyCode as K;
use KeyModifiers as M;

const MODIFIERS: [(M, &str); 4] = [
    (M::CONTROL, "Ctrl"),
    (M::ALT, "Alt"),
    (M::SHIFT, "Shift"),
    (M::SUPER, "Cmd"),
];

const NAMED: [(K, &str); 14] = [
    (K::Esc, "Escape"),
    (K::Enter, "Enter"),
    (K::Tab, "Tab"),
    (K::Backspace, "Backspace"),
    (K::Delete, "Delete"),
    (K::Left, "Left"),
    (K::Right, "Right"),
    (K::Up, "Up"),
    (K::Down, "Down"),
    (K::Home, "Home"),
    (K::End, "End"),
    (K::PageUp, "PageUp"),
    (K::PageDown, "PageDown"),
    (K::Char(' '), "Space"),
];

impl Chord {
    /// Normalize a delivered press, repeat or release without dropping any modifier bits.
    pub fn delivered(event: KeyEvent) -> Option<Self> {
        let mut modifiers = event.modifiers;
        let code = match event.code {
            K::BackTab => {
                modifiers |= M::SHIFT;
                K::Tab
            }
            K::Char(ch) => K::Char(ch.to_ascii_lowercase()),
            other => other,
        };
        Self::checked(code, modifiers)
    }

    fn checked(code: K, modifiers: M) -> Option<Self> {
        if !(modifiers - (M::CONTROL | M::ALT | M::SHIFT | M::SUPER)).is_empty() {
            return None;
        }
        match code {
            K::Char(ch)
                if (ch.is_ascii_alphanumeric() || ch == ' ')
                    && !(modifiers - M::SHIFT).is_empty() => {}
            K::F(1..=12) => {}
            other if !matches!(other, K::Char(_)) && NAMED.iter().any(|(key, _)| *key == other) => {
            }
            _ => return None,
        }
        Some(Self(code, modifiers))
    }

    /// Chords the legacy protocol can distinguish for keyboard recovery.
    pub(crate) fn portable(self) -> bool {
        match self {
            Self(K::Char(ch), M::CONTROL) => {
                ch.is_ascii_lowercase() && !matches!(ch, 'h' | 'i' | 'j' | 'm')
            }
            Self(K::Tab, M::SHIFT) => true,
            Self(code, M::NONE) => !matches!(code, K::Char(_)),
            _ => false,
        }
    }
}

impl FromStr for Chord {
    type Err = &'static str;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut words = text.rsplit('+');
        let key = words.next().unwrap_or("").to_ascii_lowercase();
        let mut modifiers = M::NONE;
        for word in words {
            let flag = MODIFIERS
                .iter()
                .find_map(|&(flag, name)| name.eq_ignore_ascii_case(word).then_some(flag))
                .or_else(|| word.eq_ignore_ascii_case("super").then_some(M::SUPER))
                .ok_or("unknown modifier")?;
            if modifiers.contains(flag) {
                return Err("repeated modifier");
            }
            modifiers |= flag;
        }
        let code = match key.as_bytes() {
            [letter] if letter.is_ascii_alphanumeric() => K::Char(char::from(*letter)),
            _ => NAMED
                .iter()
                .find_map(|&(code, name)| name.eq_ignore_ascii_case(&key).then_some(code))
                .or_else(|| {
                    let number = key.strip_prefix('f')?.parse::<u8>().ok()?;
                    (key == format!("f{number}") && (1..=12).contains(&number))
                        .then_some(K::F(number))
                })
                .ok_or("unknown key")?,
        };
        Self::checked(code, modifiers)
            .ok_or("plain and Shift-only text keys are reserved for typing")
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (flag, name) in MODIFIERS {
            if self.1.contains(flag) {
                write!(f, "{name}+")?;
            }
        }
        if let Some((_, name)) = NAMED.iter().find(|(code, _)| *code == self.0) {
            return f.write_str(name);
        }
        match self.0 {
            K::Char(ch) => write!(f, "{}", ch.to_ascii_uppercase()),
            K::F(number) => write!(f, "F{number}"),
            _ => unreachable!("validated chord"),
        }
    }
}

/// One catalog entry with a default or explicit override, including an empty list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Row {
    pub context: Context,
    pub command: Command,
    pub defaults: Vec<Chord>,
    pub override_keys: Option<Vec<Chord>>,
}

impl Row {
    /// The complete effective list in presentation order.
    pub fn keys(&self) -> &[Chord] {
        self.override_keys.as_deref().unwrap_or(&self.defaults)
    }

    /// Fully qualified keybindings identifier for diagnostics.
    pub fn identifier(&self) -> String {
        format!("{}.{}", self.context.id(), self.command.id())
    }
}

/// A draft preserves explicit overrides even when they match defaults.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Keymap {
    pub rows: Vec<Row>,
    dispatch: HashMap<(Context, Chord), Command>,
}

use Command as C;
use Context as X;

impl Default for Keymap {
    fn default() -> Self {
        let mut map = Self {
            rows: Vec::new(),
            dispatch: HashMap::new(),
        };
        // The legacy table remains the source of shipped workspace aliases.
        for binding in BINDINGS {
            let command = binding.command;
            let context = match command {
                C::Copy
                | C::Cut
                | C::Paste
                | C::Undo
                | C::Redo
                | C::SelectAll
                | C::Newline
                | C::Tab
                | C::Backspace
                | C::Delete => X::Editor,
                C::FocusNext | C::FocusPrevious | C::PreviousControl | C::Cancel => continue,
                _ => X::Global,
            };
            map.add(
                context,
                command,
                Chord::delivered(KeyEvent::new(binding.code, binding.modifiers)),
            );
        }
        for command in [
            C::ReplaceNext,
            C::ReplaceAll,
            C::CancelSearch,
            C::FollowFile,
            C::ToggleFollow,
            C::RetryFinish,
            C::ExportRetained,
            C::Tabs,
            C::TabDetails,
            C::About,
            C::Language,
            C::SyntaxTheme,
            C::ReloadHighlighting,
            C::RetryHighlighting,
            C::Recovery,
            C::RetryRefresh,
            C::Suspend,
            C::MouseCapture,
            C::Settings,
            C::ReloadSettings,
        ] {
            map.add(X::Global, command, None);
        }
        for context in X::ALL.into_iter().filter(|context| *context != X::Global) {
            map.add(context, C::Cancel, Some(Chord(K::Esc, M::NONE)));
            if context == X::Editor {
                map.add(context, C::FocusSearch, Some(Chord(K::F(6), M::NONE)));
                map.add(context, C::FocusNext, None);
            } else {
                map.add(context, C::Activate, Some(Chord(K::Enter, M::NONE)));
                map.add(context, C::FocusNext, Some(Chord(K::Tab, M::NONE)));
                if context != X::Search {
                    map.add(context, C::FocusNext, Some(Chord(K::F(6), M::NONE)));
                }
            }
            map.add(context, C::FocusPrevious, Some(Chord(K::Tab, M::SHIFT)));
            if context != X::Search {
                map.add(context, C::FocusPrevious, Some(Chord(K::F(6), M::SHIFT)));
            }
        }
        for modifier in [M::NONE, M::SHIFT] {
            map.add(X::Search, C::FocusEditor, Some(Chord(K::F(6), modifier)));
        }
        for (movement, code, modifiers) in [
            (Movement::Left, K::Left, M::NONE),
            (Movement::Right, K::Right, M::NONE),
            (Movement::Up, K::Up, M::NONE),
            (Movement::Down, K::Down, M::NONE),
            (Movement::Home, K::Home, M::NONE),
            (Movement::End, K::End, M::NONE),
            (Movement::PageUp, K::PageUp, M::NONE),
            (Movement::PageDown, K::PageDown, M::NONE),
            (Movement::Start, K::Home, M::CONTROL),
            (Movement::Finish, K::End, M::CONTROL),
        ] {
            map.add(
                X::Editor,
                C::Move(movement, false),
                Some(Chord(code, modifiers)),
            );
            map.add(
                X::Editor,
                C::Move(movement, true),
                Some(Chord(code, modifiers | M::SHIFT)),
            );
            for context in [X::Dialog, X::Picker, X::Settings] {
                map.add(
                    context,
                    C::Move(movement, false),
                    Some(Chord(code, modifiers)),
                );
                map.add(
                    context,
                    C::Move(movement, false),
                    Some(Chord(code, modifiers | M::SHIFT)),
                );
            }
        }
        for context in [X::Search, X::Picker, X::Dialog, X::Settings] {
            map.add(
                context,
                C::FieldBackspace,
                Some(Chord(K::Backspace, M::NONE)),
            );
        }
        for command in [
            C::ToggleMatchCase,
            C::ToggleWholeWord,
            C::ToggleSearchScope,
            C::CloseSearch,
            C::SearchDetails,
            C::InsertQueryTab,
            C::InsertQueryLf,
            C::InsertQueryCrlf,
        ] {
            map.add(X::Search, command, None);
        }
        for command in [
            C::PickerParent,
            C::PickerPrevious,
            C::PickerNext,
            C::SelectRow,
        ] {
            map.add(X::Picker, command, None);
        }
        map.add(X::Dialog, C::SaveAs, Some(Chord(K::F(4), M::NONE)));
        map.add(
            X::Dialog,
            C::SaveAs,
            Some(Chord(K::Char('s'), M::SHIFT | M::SUPER)),
        );
        for command in [
            C::DialogSave,
            C::DialogDiscard,
            C::DialogRetry,
            C::SelectRow,
            C::CloseSelectedTab,
            C::RetryRefresh,
            C::ConfirmReplaceAll,
            C::RecoveryPrevious,
            C::RecoveryNext,
            C::RecoveryOpen,
            C::RecoveryDiscard,
            C::RecoveryRetry,
        ] {
            map.add(X::Dialog, command, None);
        }
        for command in [
            C::ReloadSettings,
            C::SaveSettings,
            C::CancelSave,
            C::RestoreDefaults,
            C::UseDefaults,
            C::AddShortcut,
            C::ReplaceShortcut,
            C::RemoveShortcut,
            C::EditSetting,
            C::CaptureShortcut,
            C::AcceptSetting,
            C::DiscardCapture,
            C::KeepEditing,
            C::DiscardDraft,
            C::SettingsOptions,
            C::SettingsInfo,
            C::SelectRow,
        ] {
            map.add(X::Settings, command, None);
        }
        map.rows
            .sort_by_key(|row| (row.context.id(), row.command.id()));
        map.revalidate().expect("bundled bindings must be usable");
        map
    }
}

impl Keymap {
    fn add(&mut self, context: X, command: C, chord: Option<Chord>) {
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| row.context == context && row.command == command)
        {
            row.defaults.extend(chord);
        } else {
            self.rows.push(Row {
                context,
                command,
                defaults: chord.into_iter().collect(),
                override_keys: None,
            });
        }
    }

    /// Replace an entire override and defer cross-row validation until the candidate is complete.
    pub fn set_override(&mut self, context: X, id: &str, keys: Vec<Chord>) -> Result<(), String> {
        let row = self
            .rows
            .iter_mut()
            .find(|row| row.context == context && row.command.id() == id)
            .ok_or_else(|| format!("{}.{}: unknown command", context.id(), id))?;
        row.override_keys = Some(keys);
        Ok(())
    }

    /// Rebuild the dispatch index only after validating collisions and recovery routes.
    pub fn revalidate(&mut self) -> Result<(), String> {
        let f5 = Chord(K::F(5), M::NONE);
        let competing_f5 = self.rows.iter().any(|row| {
            (row.context.has_global() || row.context == X::Global && row.command != C::Settings)
                && row
                    .override_keys
                    .as_ref()
                    .is_some_and(|keys| keys.contains(&f5))
        });
        let settings = self
            .rows
            .iter_mut()
            .find(|row| row.context == X::Global && row.command == C::Settings)
            .expect("global Settings command");
        settings.defaults.clear();
        if !competing_f5 {
            settings.defaults.push(f5);
        }
        let mut dispatch = HashMap::new();
        for row in &self.rows {
            if row.keys().len() > 8 {
                return Err(format!("{}: at most eight shortcuts", row.identifier()));
            }
            for &chord in row.keys() {
                if let Some(previous) = dispatch.insert((row.context, chord), row.command) {
                    return Err(format!(
                        "{}: {chord} conflicts with {}",
                        row.identifier(),
                        previous.id()
                    ));
                }
            }
        }
        for row in self.rows.iter().filter(|row| row.context.has_global()) {
            for &chord in row.keys() {
                if let Some(global) = dispatch.get(&(X::Global, chord)) {
                    return Err(format!(
                        "{}: {chord} conflicts with global.{}",
                        row.identifier(),
                        global.id()
                    ));
                }
            }
        }
        let portable = |context, commands: &[C]| {
            dispatch.iter().any(|(&(owner, chord), command)| {
                owner == context && chord.portable() && commands.contains(command)
            })
        };
        for (context, step, commands) in [
            (X::Global, "Actions", &[C::Menu][..]),
            (X::Global, "Quit", &[C::Quit][..]),
            (
                X::Editor,
                "focus controls",
                &[C::FocusSearch, C::FocusNext, C::FocusPrevious][..],
            ),
            (X::Search, "focus editor", &[C::FocusEditor][..]),
        ] {
            if !portable(context, commands) {
                return Err(format!(
                    "{}: {step} requires a portable shortcut",
                    context.id()
                ));
            }
        }
        for context in X::ALL.into_iter().filter(|context| *context != X::Global) {
            for (step, commands) in [
                ("Cancel", &[C::Cancel][..]),
                ("focus", &[C::FocusNext, C::FocusPrevious][..]),
                ("Activate", &[C::Activate][..]),
            ] {
                if context == X::Editor && step != "Cancel" {
                    continue;
                }
                if !portable(context, commands) {
                    return Err(format!(
                        "{}: {step} requires a portable shortcut",
                        context.id()
                    ));
                }
            }
        }
        self.dispatch = dispatch;
        Ok(())
    }

    /// Resolve the current local context first and consume unavailable or repeated bindings.
    pub fn command(&self, context: X, key: KeyEvent) -> Option<C> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let chord = Chord::delivered(key)?;
        self.dispatch
            .get(&(context, chord))
            .or_else(|| {
                context
                    .has_global()
                    .then(|| self.dispatch.get(&(X::Global, chord)))
                    .flatten()
            })
            .copied()
            .filter(|command| key.kind == KeyEventKind::Press || command.repeatable())
    }

    /// Display the effective shortcuts for a specific command and context.
    pub fn shortcuts(&self, context: X, command: C) -> String {
        self.rows
            .iter()
            .find(|row| row.context == context && row.command == command)
            .map(|row| {
                row.keys()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default()
    }

    /// Prefer a legacy-decodable route in space-constrained help and recovery text.
    pub fn portable_hint(&self, context: X, command: C) -> String {
        self.rows
            .iter()
            .find(|row| row.context == context && row.command == command)
            .and_then(|row| {
                row.keys()
                    .iter()
                    .copied()
                    .find(|key| key.portable())
                    .or_else(|| row.keys().first().copied())
            })
            .map_or_else(String::new, |key| key.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_chords_normalize_only_supported_aliases() {
        let canonical = "Ctrl+Alt+Shift+Cmd+Space";
        assert_eq!(
            "super+SHIFT+alt+cTrL+space"
                .parse::<Chord>()
                .unwrap()
                .to_string(),
            canonical
        );
        assert_eq!(
            Chord::delivered(KeyEvent::new(K::BackTab, M::CONTROL)),
            "Ctrl+Shift+Tab".parse().ok(),
        );
        for invalid in [
            "Space",
            "Shift+Z",
            "Ctrl+-",
            "Ctrl+Ctrl+A",
            "Cmd+Super+A",
            "Ctrl+F13",
            "Ctrl+A Ctrl+B",
            "Ctrl++A",
        ] {
            assert!(invalid.parse::<Chord>().is_err(), "{invalid}");
        }
        let map = Keymap::default();
        for kind in [
            KeyEventKind::Press,
            KeyEventKind::Repeat,
            KeyEventKind::Release,
        ] {
            assert_eq!(
                map.command(
                    X::Editor,
                    KeyEvent::new_with_kind(K::Char('S'), M::SUPER | M::SHIFT, kind)
                ),
                (kind == KeyEventKind::Press).then_some(C::SaveAs)
            );
            assert_eq!(
                map.command(X::Editor, KeyEvent::new_with_kind(K::Up, M::SHIFT, kind)),
                (kind != KeyEventKind::Release).then_some(C::Move(Movement::Up, true))
            );
        }
        assert_eq!(
            map.command(X::Dialog, KeyEvent::new(K::Char('q'), M::CONTROL)),
            None
        );
        assert_eq!(
            crate::input::text(KeyEvent::new(K::Char('界'), M::SHIFT)),
            Some('界')
        );
    }

    #[test]
    fn documented_default_catalog_matches_dispatch() {
        use std::fmt::Write as _;
        let map = Keymap::default();
        let mut table =
            String::from("\n| Context | Command ID | Defaults |\n| --- | --- | --- |\n");
        for context in X::ALL {
            for row in map.rows.iter().filter(|row| row.context == context) {
                let keys = map.shortcuts(context, row.command);
                writeln!(
                    table,
                    "| `{}` | `{}` | {} |",
                    context.id(),
                    row.command.id(),
                    if keys.is_empty() { "—" } else { &keys }
                )
                .unwrap();
            }
        }
        let docs = include_str!("../../README.md");
        let (_, docs) = docs.split_once("<!-- keymap-defaults:start -->").unwrap();
        let (docs, _) = docs.split_once("<!-- keymap-defaults:end -->").unwrap();
        if docs != table {
            eprintln!("<!-- keymap-defaults:start -->{table}<!-- keymap-defaults:end -->");
        }
        assert_eq!(
            docs, table,
            "README command catalog must reflect default dispatch"
        );
    }
}
