//! Lane B: converts a loaded [`electricity_value::Value`] into the
//! schema-instance shape `electricity-schema` validates, keeping
//! Python's verdicts for dates, bytes, and non-string keys such as
//! `yes:` or `1:` (`structural.rs`'s JSON Schema step).
