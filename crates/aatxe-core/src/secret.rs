//! Newtype wrapper for credentials.
//!
//! `String`-typed API keys and tokens leak through any accidental `Debug`
//! print — error messages, panic backtraces, third-party logging
//! middleware. Wrapping them in a [`Secret`] ensures the credential is
//! never serialised to a log: both `Debug` and `Display` print a fixed
//! redaction marker, and there is no `Deref` to `str`, so accessing the
//! underlying value requires an explicit [`Secret::reveal`] call that a
//! reader can grep for.
//!
//! `Secret` deliberately does **not** implement `Serialize`. The credential
//! is set in memory and used in HTTP headers; persisting it to disk is a
//! distinct decision the caller should have to make explicitly.

use std::fmt;

/// Opaque credential. Constructed from any string-like value; the only
/// way to read the contents back is [`Secret::reveal`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a string as a credential.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the underlying credential as a `&str`. Use only where the
    /// value must be sent (HTTP header, command arg). Never log the result.
    pub fn reveal(&self) -> &str {
        &self.0
    }

    /// True when the credential is the empty string. Useful for "config
    /// missing" checks without needing to call `reveal`.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_value() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{:?}", s), "Secret(***)");
        assert!(!format!("{:?}", s).contains("hunter2"));
    }

    #[test]
    fn display_redacts_value() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{}", s), "***");
    }

    #[test]
    fn reveal_returns_underlying_value() {
        let s = Secret::new("hunter2");
        assert_eq!(s.reveal(), "hunter2");
    }

    #[test]
    fn redaction_survives_nested_debug() {
        // A struct that derives Debug should still print the redacted form.
        #[derive(Debug)]
        #[allow(dead_code)] // fields are read via the derived Debug impl.
        struct Wrap {
            token: Secret,
            other: u32,
        }
        let w = Wrap {
            token: Secret::new("hunter2"),
            other: 42,
        };
        let printed = format!("{:?}", w);
        assert!(!printed.contains("hunter2"));
        assert!(printed.contains("Secret(***)"));
        assert!(printed.contains("42"));
    }
}
