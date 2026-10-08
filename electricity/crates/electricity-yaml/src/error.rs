//! The error type [`YamlError`] every composer function in this crate
//! returns, and the position it carries.

use std::fmt;

/// A 0-indexed `(line, column)` position, in PyYAML's own `Mark`
/// convention (`mark.line`/`mark.column`, both 0-indexed; PyYAML only adds
/// 1 when it *displays* a mark). Converting a `saphyr_parser::Marker`
/// (1-indexed line, 0-indexed column) to this type is `(line() - 1,
/// col())`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub line: usize,
    pub column: usize,
}

impl From<saphyr_parser::Marker> for Mark {
    fn from(marker: saphyr_parser::Marker) -> Self {
        Mark {
            line: marker.line().saturating_sub(1),
            column: marker.col(),
        }
    }
}

/// Why loading a YAML document failed.
///
/// [`YamlError::DuplicateKey`] is Circuitry's own check and carries the
/// exact message `core/yaml_load.py` produces. Every other variant
/// reproduces a PyYAML/third-party failure: same place (a [`Mark`]), a
/// non-empty message, not necessarily the same words (DESIGN.md §1, §12).
#[derive(Debug, Clone, PartialEq)]
pub enum YamlError {
    /// A scan/parse-level failure from `saphyr-parser` itself (bad
    /// syntax, an unknown anchor, ...).
    Scan { message: String, mark: Mark },
    /// More than one YAML document in the stream — `safe_load` only
    /// accepts one.
    MultipleDocuments { mark: Mark },
    /// The same mapping defines one key twice (`core/yaml_load.py`'s
    /// `DuplicateKeyError`, word for word). `<<:` is exempt.
    DuplicateKey { message: String },
    /// A resolved tag (explicit or the `=`/`<<` scalar text standing
    /// alone as a value) has no constructor — matches `SafeLoader` having
    /// none either, including `!!set`/`!!omap`/`!!pairs` and any unknown
    /// tag, none of which `Value` can represent (DESIGN.md §3.2: "anything
    /// PyYAML accepts that it cannot represent becomes a documented
    /// known-divergence case").
    UnresolvableTag { tag: String, mark: Mark },
    /// An explicit `!!int`/`!!float`/`!!bool`/`!!timestamp`/`!!binary` tag
    /// whose scalar text doesn't parse under that type's constructor.
    InvalidScalar { type_name: &'static str, mark: Mark },
    /// A mapping key that YAML/Python can't hash (a list or a mapping
    /// used as a key) — PyYAML's `SafeConstructor.construct_mapping`
    /// raises `found unhashable key`.
    UnhashableKey { mark: Mark },
    /// `<<:`'s value is not a mapping, nor a sequence of mappings.
    InvalidMerge { mark: Mark },
}

impl YamlError {
    /// The position this error occurred at, when it has one meaningful to
    /// compare against PyYAML's own (`DuplicateKey` carries its two marks
    /// inside the message itself instead).
    pub fn mark(&self) -> Option<Mark> {
        match self {
            YamlError::Scan { mark, .. }
            | YamlError::MultipleDocuments { mark }
            | YamlError::UnresolvableTag { mark, .. }
            | YamlError::InvalidScalar { mark, .. }
            | YamlError::UnhashableKey { mark }
            | YamlError::InvalidMerge { mark } => Some(*mark),
            YamlError::DuplicateKey { .. } => None,
        }
    }
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            YamlError::Scan { message, mark } => {
                write!(f, "{message} at line {}, column {}", mark.line, mark.column)
            }
            YamlError::MultipleDocuments { mark } => write!(
                f,
                "expected a single document in the stream at line {}, column {}",
                mark.line, mark.column
            ),
            YamlError::DuplicateKey { message } => write!(f, "{message}"),
            YamlError::UnresolvableTag { tag, mark } => write!(
                f,
                "could not determine a constructor for the tag {tag:?} at line {}, column {}",
                mark.line, mark.column
            ),
            YamlError::InvalidScalar { type_name, mark } => write!(
                f,
                "invalid {type_name} scalar at line {}, column {}",
                mark.line, mark.column
            ),
            YamlError::UnhashableKey { mark } => write!(
                f,
                "found unhashable key at line {}, column {}",
                mark.line, mark.column
            ),
            YamlError::InvalidMerge { mark } => write!(
                f,
                "expected a mapping or list of mappings for merging at line {}, column {}",
                mark.line, mark.column
            ),
        }
    }
}

impl std::error::Error for YamlError {}
