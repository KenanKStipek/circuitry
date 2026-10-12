//! Python `re` pattern-syntax translation (#448 lane D2, issue #479),
//! shared by `electricity-tools::regex` (the `regex` tool plugin) and
//! `electricity-schema::engine` (JSON Schema `pattern`/
//! `patternProperties`/`propertyNames.pattern`, and the
//! `additionalProperties`+`patternProperties` filter) -- the one place
//! either crate needs to turn Python `re` syntax into `fancy-regex`
//! syntax, moved out from under `electricity-tools` (PR #502's round-4
//! fix, finding F2) so `electricity-schema` can depend on it without
//! also pulling in every other tool plugin.
//!
//! See [`translate`]'s own module docs for what this does and does not
//! translate, and `electricity/DESIGN.md` §7.3/§16 for the accepted,
//! documented parity gaps.

mod regex_unicode;
pub mod translate;
