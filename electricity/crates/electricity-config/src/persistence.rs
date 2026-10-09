//! `core/store/persistence.py::build_persistence_backend`'s own
//! *validation* -- the backend-name alias lookup and each backend's
//! `from_config` field checks (`core/store/{jsonl_file,mongodb,postgres,
//! sqlite}.py`, `core/store/_identifiers.py::validate_table_name`).
//! M0-H never actually opens a persistence backend (a document/config
//! that configures one is refused with the preview marker right after
//! this validation runs -- issue #431's "Out of scope" section), so
//! nothing here connects to anything; it only reproduces the exceptions
//! Circuitry's own `from_config` raises for a malformed block, which a
//! run must still fail on before reaching the refusal.

use crate::util::get;
use electricity_value::{Dict, Value};
use regex::Regex;
use std::sync::LazyLock;

static TABLE_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,62}$").unwrap());

/// `core/store/_identifiers.py::validate_table_name`.
fn validate_table_name(name: &str) -> Result<(), String> {
    if TABLE_NAME_RE.is_match(name) {
        Ok(())
    } else {
        Err("runtime.persistence.table must match [A-Za-z_][A-Za-z0-9_]{0,62}".to_string())
    }
}

/// `str(config.get(key) or "")` -- Circuitry's own idiom throughout the
/// four `from_config` methods: a falsy value (absent, `None`, `""`,
/// `0`, `False`) is treated the same as "not given".
fn str_or_empty(config: &Dict, key: &str) -> String {
    match get(config, key) {
        Some(Value::Str(s)) if !s.is_empty() => s.clone(),
        Some(Value::Bool(b)) if *b => "True".to_string(),
        Some(Value::Int(i)) if !i.is_zero() => i.to_string(),
        Some(Value::Float(f)) if *f != 0.0 => f.to_string(),
        _ => String::new(),
    }
}

fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::None) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(i)) => !i.is_zero(),
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::List(items)) => !items.is_empty(),
        Some(Value::Dict(d)) => !d.is_empty(),
        Some(Value::Bytes(b)) => !b.is_empty(),
        Some(Value::Date(_)) | Some(Value::DateTime(..)) => true,
    }
}

fn validate_jsonl_file(config: &Dict) -> Result<(), String> {
    let path = {
        let p = str_or_empty(config, "path");
        if p.is_empty() {
            str_or_empty(config, "db_path")
        } else {
            p
        }
    };
    if path.trim().is_empty() {
        return Err(
            "Persistence backend 'jsonl-file' requires runtime.persistence.path".to_string(),
        );
    }
    Ok(())
}

fn validate_mongodb(config: &Dict) -> Result<(), String> {
    let uri = {
        let u = str_or_empty(config, "uri");
        if u.is_empty() {
            str_or_empty(config, "dsn")
        } else {
            u
        }
    };
    if uri.trim().is_empty() {
        return Err("Persistence backend 'mongodb' requires runtime.persistence.uri".to_string());
    }
    let database = str_or_empty(config, "database");
    let database = if database.trim().is_empty() {
        "circuitry".to_string()
    } else {
        database
    };
    if database.trim().is_empty() {
        return Err("runtime.persistence.database must be a non-empty string".to_string());
    }
    let collection = str_or_empty(config, "collection");
    let collection = if collection.trim().is_empty() {
        "circuitry_runs".to_string()
    } else {
        collection
    };
    if collection.trim().is_empty() {
        return Err("runtime.persistence.collection must be a non-empty string".to_string());
    }
    Ok(())
}

fn validate_postgres(config: &Dict) -> Result<(), String> {
    let dsn = str_or_empty(config, "dsn");
    if dsn.trim().is_empty() {
        return Err("Persistence backend 'postgres' requires runtime.persistence.dsn".to_string());
    }
    let table = str_or_empty(config, "table");
    let table = if table.trim().is_empty() {
        "circuitry_runs".to_string()
    } else {
        table
    };
    if table.trim().is_empty() {
        return Err("runtime.persistence.table must be a non-empty string".to_string());
    }
    validate_table_name(table.trim())?;

    let sslmode = {
        let s = str_or_empty(config, "sslmode");
        if s.is_empty() {
            "require".to_string()
        } else {
            s
        }
    }
    .trim()
    .to_lowercase();
    let allow_insecure = is_truthy(get(config, "allow_insecure"));
    if matches!(sslmode.as_str(), "disable" | "allow" | "prefer") && !allow_insecure {
        return Err(
            "Insecure postgres sslmode requested. Set runtime.persistence.sslmode to \
             'require'/'verify-ca'/'verify-full', or explicitly set \
             runtime.persistence.allow_insecure=true for local development."
                .to_string(),
        );
    }
    Ok(())
}

fn validate_sqlite(config: &Dict) -> Result<(), String> {
    let db_path = {
        let p = str_or_empty(config, "db_path");
        if p.is_empty() {
            str_or_empty(config, "path")
        } else {
            p
        }
    };
    if db_path.trim().is_empty() {
        return Err(
            "Persistence backend 'sqlite' requires runtime.persistence.db_path".to_string(),
        );
    }
    let table = str_or_empty(config, "table");
    let table = if table.trim().is_empty() {
        "circuitry_runs".to_string()
    } else {
        table
    };
    if table.trim().is_empty() {
        return Err("runtime.persistence.table must be a non-empty string".to_string());
    }
    validate_table_name(table.trim())?;
    Ok(())
}

/// `_BACKEND_ALIASES`' canonical name for *requested* (already
/// `.strip().lower()`ed), or `None` for an unrecognized spelling.
fn canonical_backend(requested: &str) -> Option<&'static str> {
    match requested {
        "jsonl-file" | "jsonl_file" | "jsonl" | "file" => Some("jsonl-file"),
        "mongodb" | "mongo" => Some("mongodb"),
        "postgres" | "postgresql" => Some("postgres"),
        "sqlite" | "sqlite3" => Some("sqlite"),
        _ => None,
    }
}

/// `build_persistence_backend(runtime)`'s own validation half: *runtime*
/// is the already-merged, effective `runtime:` mapping. `Ok(())` when
/// there is nothing to validate (`runtime.persistence` absent, not an
/// object, or `enabled` is not `true`) -- Circuitry's own function
/// returns `None` for exactly these, never raising.
pub fn validate_persistence_block(runtime: Option<&Value>) -> Result<(), String> {
    let Some(persistence_cfg) = runtime
        .and_then(Value::as_dict)
        .and_then(|r| get(r, "persistence"))
        .and_then(Value::as_dict)
    else {
        return Ok(());
    };
    if !is_truthy(get(persistence_cfg, "enabled")) {
        return Ok(());
    }

    let requested = {
        let raw = str_or_empty(persistence_cfg, "backend");
        if raw.is_empty() {
            "postgres".to_string()
        } else {
            raw
        }
    }
    .trim()
    .to_lowercase();

    match canonical_backend(&requested) {
        Some("postgres") => validate_postgres(persistence_cfg),
        Some("sqlite") => validate_sqlite(persistence_cfg),
        Some("jsonl-file") => validate_jsonl_file(persistence_cfg),
        Some("mongodb") => validate_mongodb(persistence_cfg),
        _ => Err(format!(
            "Unsupported persistence backend: {}. Supported backends: jsonl-file, mongodb, \
             postgres, sqlite.",
            Value::Str(requested).py_repr()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_with_persistence(persistence: Dict) -> Value {
        let mut runtime = Dict::new();
        runtime.insert(
            Value::Str("persistence".to_string()),
            Value::Dict(persistence),
        );
        Value::Dict(runtime)
    }

    #[test]
    fn disabled_persistence_is_never_validated() {
        let mut persistence = Dict::new();
        persistence.insert(Value::Str("backend".to_string()), Value::from("nonsense"));
        let runtime = runtime_with_persistence(persistence);
        assert!(validate_persistence_block(Some(&runtime)).is_ok());
    }

    #[test]
    fn an_unsupported_backend_name_is_rejected() {
        let mut persistence = Dict::new();
        persistence.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        persistence.insert(Value::Str("backend".to_string()), Value::from("dynamodb"));
        let runtime = runtime_with_persistence(persistence);
        let err = validate_persistence_block(Some(&runtime)).unwrap_err();
        assert_eq!(
            err,
            "Unsupported persistence backend: 'dynamodb'. Supported backends: jsonl-file, \
             mongodb, postgres, sqlite."
        );
    }

    #[test]
    fn postgres_requires_a_dsn() {
        let mut persistence = Dict::new();
        persistence.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        persistence.insert(Value::Str("backend".to_string()), Value::from("postgres"));
        let runtime = runtime_with_persistence(persistence);
        let err = validate_persistence_block(Some(&runtime)).unwrap_err();
        assert_eq!(
            err,
            "Persistence backend 'postgres' requires runtime.persistence.dsn"
        );
    }

    #[test]
    fn postgres_rejects_an_insecure_sslmode_without_the_opt_out() {
        let mut persistence = Dict::new();
        persistence.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        persistence.insert(Value::Str("backend".to_string()), Value::from("postgres"));
        persistence.insert(
            Value::Str("dsn".to_string()),
            Value::from("postgres://localhost/db"),
        );
        persistence.insert(Value::Str("sslmode".to_string()), Value::from("disable"));
        let runtime = runtime_with_persistence(persistence);
        let err = validate_persistence_block(Some(&runtime)).unwrap_err();
        assert!(err.starts_with("Insecure postgres sslmode requested."));
    }

    #[test]
    fn a_malformed_table_name_is_rejected() {
        let mut persistence = Dict::new();
        persistence.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        persistence.insert(Value::Str("backend".to_string()), Value::from("sqlite"));
        persistence.insert(Value::Str("db_path".to_string()), Value::from("a.db"));
        persistence.insert(Value::Str("table".to_string()), Value::from("bad table!"));
        let runtime = runtime_with_persistence(persistence);
        let err = validate_persistence_block(Some(&runtime)).unwrap_err();
        assert_eq!(
            err,
            "runtime.persistence.table must match [A-Za-z_][A-Za-z0-9_]{0,62}"
        );
    }

    #[test]
    fn a_valid_sqlite_block_passes() {
        let mut persistence = Dict::new();
        persistence.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        persistence.insert(Value::Str("backend".to_string()), Value::from("sqlite"));
        persistence.insert(Value::Str("db_path".to_string()), Value::from("a.db"));
        let runtime = runtime_with_persistence(persistence);
        assert!(validate_persistence_block(Some(&runtime)).is_ok());
    }
}
