//! Circuitry's reflector prime (`core/primes.py::REFLECTOR_PRIME_V1`),
//! copied byte for byte by
//! `electricity/scripts/generate_reflector_prime.py --check` into
//! `reflector_prime.txt`. `ReflectorDefinition.prime_template`'s
//! default is this file read as a raw string, no normalization: lane
//! C's compiler uses this constant for a `reflector` effect that omits
//! `prime_template:`.

/// Circuitry's default reflector prime, byte for byte.
pub const REFLECTOR_PRIME: &str = include_str!("reflector_prime.txt");
