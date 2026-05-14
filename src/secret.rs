//! `Secret<T>` — the only home-grown alternative to the `secrecy` crate (§15).
//!
//! Holds a value that must never appear in logs or `Debug` output. Exposes the
//! inner value only through an explicit `.expose()` call so that secret leaks
//! show up at code-review time as `.expose()` call sites.

// `expose` is unused in M1 (no handler reads the cookie key yet). Removed once
// M2 wires sessions.
#![allow(dead_code)]

/// A wrapper that redacts itself in `Debug` and forbids accidental display.
///
/// `Clone` is allowed (the inner type is typically `String` and config gets
/// cloned during startup). `PartialEq` is intentionally not derived — comparing
/// secrets is something we want to write explicitly, with constant-time
/// semantics where it matters.
#[derive(Clone)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Yield the wrapped value. Every call site is a leak-audit point.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_leak() {
        let s = Secret::new("hunter2".to_string());
        let rendered = format!("{s:?}");
        assert!(!rendered.contains("hunter2"));
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn expose_returns_inner() {
        let s = Secret::new(42_u32);
        assert_eq!(*s.expose(), 42);
    }
}
