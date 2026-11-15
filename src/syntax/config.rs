//! Syntax palettes and atomically loaded file, fence and query customizations.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};
use toml::{Table, Value};

use super::{Category, LANGUAGES, Language};

pub(super) const SOURCE_LIMIT: usize = 256 * 1024;

/// Values map onto the terminal's ANSI palette or explicit truecolor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Foreground {
    Ansi(u8),
    Rgb(u8, u8, u8),
}

/// Foregrounds indexed by stable semantic category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Palette([Foreground; Category::ALL.len()]);

impl Default for Palette {
    fn default() -> Self {
        Self::built_in(false)
    }
}

impl Palette {
    /// Built-in colors with equivalent contrast intent on dark and light terminals.
    fn built_in(light: bool) -> Self {
        use Category::*;
        let colors: [(Category, u8, u8); Category::ALL.len()] = [
            // Category, Dark palette, Light palette.
            (Comment, 8, 8),
            (String, 10, 2),
            (Number, 13, 5),
            (Keyword, 14, 4),
            (Type, 11, 6),
            (Property, 12, 4),
            (Markup, 13, 5),
            (Function, 12, 4),
            (Constant, 13, 5),
            (Operator, 15, 0),
            (Punctuation, 7, 8),
            (Variable, 12, 4),
            (Tag, 14, 5),
            (Attribute, 11, 6),
            (Code, 10, 2),
            (Heading1, 13, 5),
            (Heading2, 14, 6),
            (Heading3, 12, 4),
            (Heading4, 10, 2),
            (Heading5, 11, 1),
            (Heading6, 9, 8),
            (ListMarker, 11, 4),
            (OrderedListMarker, 14, 6),
            (QuoteMarker, 12, 4),
            (CodeDelimiter, 12, 4),
            (FenceLanguage, 11, 5),
            (Emphasis, 13, 5),
            (StrongEmphasis, 11, 1),
            (Strikethrough, 8, 8),
            (Link, 14, 4),
            (LinkTarget, 12, 6),
            (TaskChecked, 10, 2),
            (TaskUnchecked, 11, 5),
            (TableHeader, 13, 5),
            (Separator, 8, 8),
        ];
        let mut palette = [Foreground::Ansi(0); Category::ALL.len()];
        for (category, dark_color, light_color) in colors {
            palette[category as usize] =
                Foreground::Ansi(if light { light_color } else { dark_color });
        }
        Self(palette)
    }

    /// Resolve a category without allocating or reading configuration.
    pub(crate) fn color(&self, category: Category) -> Foreground {
        self.0[category as usize]
    }
}

/// Sources whose revision changes semantic analysis, independently of palette changes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Rules {
    pub(super) fences: BTreeMap<String, Option<&'static Language>>,
    pub(super) queries: BTreeMap<&'static str, String>,
}

impl Rules {
    /// Automatic fence aliases are ASCII case-insensitive. Plain aliases are reserved.
    pub(super) fn fence(&self, word: &str) -> Option<&'static Language> {
        if word.eq_ignore_ascii_case("text") || word.eq_ignore_ascii_case("plain") {
            return None;
        }
        let folded = word.to_ascii_lowercase();
        self.fences.get(&folded).copied().unwrap_or_else(|| {
            Language::by_id(&folded).or_else(|| Language::extension(folded.as_bytes()))
        })
    }
}

/// An immutable validated revision. An absent revision is equivalent to defaults.
#[derive(Debug, Default)]
pub(crate) struct Config {
    names: BTreeMap<String, Option<&'static Language>>,
    extensions: BTreeMap<String, Option<&'static Language>>,
    themes: BTreeMap<String, Palette>,
    pub(crate) default_theme: Option<String>,
    pub(super) rules: Arc<Rules>,
}

impl Config {
    /// Resolve exact basename, custom final extension, then bundled final extension.
    pub(super) fn detect(&self, path: &Path) -> Option<&'static Language> {
        if let Some(target) = path
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| self.names.get(name))
        {
            return *target;
        }
        let extension = path.extension()?;
        if !self.extensions.is_empty()
            && let Some(extension) = extension.to_str()
            && let Some(target) = self.extensions.get(&extension.to_ascii_lowercase())
        {
            return *target;
        }
        Language::extension(extension.as_encoded_bytes())
    }

    /// Display built-in themes and valid configuration names in deterministic order.
    pub(crate) fn themes(&self) -> impl Iterator<Item = &str> {
        ["Dark", "Light"]
            .into_iter()
            .chain(self.themes.keys().map(String::as_str))
    }

    /// Theme lookup for initial selection, reload and explicit session choice.
    pub(crate) fn palette(&self, name: &str) -> Option<Palette> {
        match name {
            "Dark" => Some(Palette::built_in(false)),
            "Light" => Some(Palette::built_in(true)),
            _ => self.themes.get(name).copied(),
        }
    }

    /// Test whether analysis results can survive the configuration update.
    pub(crate) fn same_rules(&self, other: &Self) -> bool {
        self.rules == other.rules
    }

    /// Read all candidate sources under one byte cap. Missing paths use defaults.
    pub(super) fn load(root: Option<&Path>) -> io::Result<Self> {
        let Some(root) = root else {
            return Ok(Self::default());
        };
        let mut remaining = SOURCE_LIMIT;
        let path = root.join("highlighting.toml");
        let mut config = match read(&path, &mut remaining)? {
            Some(text) => Self::parse(&text).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {error}", path.display()),
                )
            })?,
            None => Self::default(),
        };
        for id in LANGUAGES
            .iter()
            .map(|language| language.id)
            .chain([super::engine::INLINE])
        {
            let path = root.join(query_path(id));
            if let Some(text) = read(&path, &mut remaining)? {
                Arc::make_mut(&mut config.rules).queries.insert(id, text);
            }
        }
        Ok(config)
    }

    /// Parse a strict TOML schema and apply inheritance before publication.
    fn parse(text: &str) -> Result<Self, String> {
        let mut root = text.parse::<Table>().map_err(|error| {
            let line = error.span().map_or(1, |span| {
                memchr::memchr_iter(b'\n', &text.as_bytes()[..span.start.min(text.len())]).count()
                    + 1
            });
            format!("line {line}: {}", error.message())
        })?;
        let mut files = take_table(&mut root, "files")?;
        let names = associations(take_table(&mut files, "names")?, false, false)?;
        let extensions = associations(take_table(&mut files, "extensions")?, true, false)?;
        empty(&files, "files")?;
        let mut fences = take_table(&mut root, "fences")?;
        let aliases = associations(take_table(&mut fences, "aliases")?, true, true)?;
        empty(&fences, "fences")?;
        let mut themes = BTreeMap::new();
        for (name, value) in take_table(&mut root, "themes")? {
            if name.is_empty()
                || name.len() > 80
                || name.chars().any(char::is_control)
                || name.eq_ignore_ascii_case("dark")
                || name.eq_ignore_ascii_case("light")
            {
                return Err(format!("invalid or reserved theme name {name:?}"));
            }
            let Value::Table(mut theme) = value else {
                return Err(format!("{name}: expected table"));
            };
            let mut palette = match theme.remove("base").as_ref().and_then(Value::as_str) {
                Some("dark") => Palette::built_in(false),
                Some("light") => Palette::built_in(true),
                _ => return Err(format!("{name}: base must be dark or light")),
            };
            let mut overrides = [None; Category::ALL.len()];
            for (key, value) in take_table(&mut theme, "colors")? {
                let category = Category::named(&key)
                    .ok_or_else(|| format!("{name}: unknown category {key:?}"))?;
                let color = value.as_str().and_then(color).ok_or_else(|| {
                    format!("{name}.{key}: expected an ANSI color name or #RRGGBB")
                })?;
                overrides[category as usize] = Some(color);
            }
            for &(category, _) in Category::ALL {
                if let Some(color) =
                    std::iter::successors(Some(category), |entry| entry.theme_parent())
                        .find_map(|entry| overrides[entry as usize])
                {
                    palette.0[category as usize] = color;
                }
            }
            empty(&theme, &name)?;
            themes.insert(name, palette);
        }
        let default_theme = match root.remove("default_theme") {
            Some(Value::String(name))
                if name == "Dark" || name == "Light" || themes.contains_key(&name) =>
            {
                Some(name)
            }
            Some(_) => return Err("default_theme must name Dark, Light or a defined theme".into()),
            None => None,
        };
        empty(&root, "highlighting")?;
        Ok(Self {
            names,
            extensions,
            themes,
            default_theme,
            rules: Arc::new(Rules {
                fences: aliases,
                queries: BTreeMap::new(),
            }),
        })
    }
}

/// Compute the config directory once from absolute XDG_CONFIG_HOME or HOME.
pub(super) fn directory() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".config"))
                .filter(|path| path.is_absolute())
        })
        .map(|path| path.join("ede"))
}

/// Load only regular bounded files so a FIFO or device cannot block reload indefinitely.
fn read(path: &Path, remaining: &mut usize) -> io::Result<Option<String>> {
    use std::os::unix::fs::OpenOptionsExt;
    let result = (|| {
        let mut file = match File::options()
            .read(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected a regular file",
            ));
        }
        let mut bytes = String::new();
        (&mut file)
            .take(*remaining as u64 + 1)
            .read_to_string(&mut bytes)?;
        *remaining = remaining.checked_sub(bytes.len()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "combined syntax configuration exceeds 256 KiB",
            )
        })?;
        Ok(Some(bytes))
    })();
    result.map_err(|error: io::Error| {
        io::Error::new(error.kind(), format!("{}: {error}", path.display()))
    })
}

/// Load and report the same path for the block and inline Markdown grammars.
pub(super) fn query_path(id: &str) -> PathBuf {
    if id == super::engine::INLINE {
        PathBuf::from("queries/markdown/inline-highlights.scm")
    } else {
        Path::new("queries").join(id).join("highlights.scm")
    }
}

/// Pull a named optional table out of a strict schema.
fn take_table(parent: &mut Table, key: &str) -> Result<Table, String> {
    match parent.remove(key) {
        None => Ok(Table::new()),
        Some(Value::Table(table)) => Ok(table),
        _ => Err(format!("{key}: expected a table")),
    }
}

/// Reject misspelled or unsupported keys early.
fn empty(table: &Table, parent: &str) -> Result<(), String> {
    if let Some((key, _)) = table.iter().next() {
        return Err(format!("{parent}: unknown key {key:?}"));
    }
    Ok(())
}

/// Normalize ASCII-folded maps while checking duplicates and reserved aliases.
fn associations(
    source: Table,
    fold: bool,
    fence: bool,
) -> Result<BTreeMap<String, Option<&'static Language>>, String> {
    let mut output = BTreeMap::new();
    for (key, value) in source {
        let target = match value.as_str() {
            Some("plain") => None,
            Some(id) => Some(
                Language::by_id(id)
                    .ok_or_else(|| format!("unknown language {id:?} for {key:?}"))?,
            ),
            None => return Err(format!("{key:?}: expected a language ID")),
        };
        let key = if fold { key.to_ascii_lowercase() } else { key };
        if key.is_empty()
            || key.contains('/')
            || key.as_bytes().contains(&0)
            || (fence && key.chars().any(char::is_whitespace))
            || (fold && !fence && key.contains('.'))
            || (fence && matches!(key.as_str(), "text" | "plain"))
        {
            return Err(format!("invalid filename, extension or fence key {key:?}"));
        }
        if output.insert(key.clone(), target).is_some() {
            return Err(format!("duplicate association {key:?}"));
        }
    }
    Ok(output)
}

/// Parse the sixteen portable ANSI names plus explicit six-digit RGB.
fn color(value: &str) -> Option<Foreground> {
    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() != 6 || !hex.as_bytes().iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let rgb = u32::from_str_radix(hex, 16).ok()?;
        return Some(Foreground::Rgb(
            (rgb >> 16) as u8,
            (rgb >> 8) as u8,
            rgb as u8,
        ));
    }
    let names = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "white",
        "bright-black",
        "bright-red",
        "bright-green",
        "bright-yellow",
        "bright-blue",
        "bright-magenta",
        "bright-cyan",
        "bright-white",
    ];
    names
        .iter()
        .position(|name| *name == value)
        .map(|index| Foreground::Ansi(index as u8))
}
