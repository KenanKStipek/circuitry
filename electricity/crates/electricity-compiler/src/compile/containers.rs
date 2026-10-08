//! Lane C: compiles `dynamic`, `if`/`conditional` and `loop` into
//! [`electricity_bytecode::Region`] — names and scopes, duplicate-name
//! checks per scope, `finally:` sharing the body's scope, and the
//! `if`/`while` mode checks.
