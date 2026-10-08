//! Lane C: compiles `prompt`, `tool`, `use`, `yield` and `reflector`
//! into their [`electricity_bytecode::effects`] option structs — every
//! effect type's own required-field checks, templates, params, `expect`
//! and `outputs`, and Python's coercions (`int(...)`, `float(...)`,
//! truthiness, an invalid `on_error` becoming `"fail"`).
