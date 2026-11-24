//! A wrapper for credential values so a stray `{:?}` or `{}` cannot leak them
//! (WI-4656). The value is reachable only through [`Secret::expose`].

use serde::{Deserialize, Deserializer, Serializer};
use zeroize::Zeroize;

/// A credential. `Debug` and `Display` print `[redacted]`.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret<T: Zeroize>(T);

/// The value is wiped from memory when the secret drops.
impl<T: Zeroize> Drop for Secret<T> {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl<T: Zeroize> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The wrapped value. Calling this is the explicit decision to use it.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T: Zeroize> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret<String> {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// The text of an optional secret string, for the few places that compare or
/// send it.
pub fn expose_opt(secret: &Option<Secret<String>>) -> Option<&str> {
    secret.as_ref().map(|s| s.expose().as_str())
}

impl<T: Zeroize> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<T: Zeroize> std::fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<'de, T: Deserialize<'de> + Zeroize> Deserialize<'de> for Secret<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Self)
    }
}

/// `#[serde(serialize_with)]` helper: emit the inner value, for a secret that
/// is meant to go over the wire.
pub fn serialize_exposed<T: serde::Serialize + Zeroize, S: Serializer>(
    secret: &Secret<T>,
    s: S,
) -> Result<S::Ok, S::Error> {
    secret.0.serialize(s)
}

/// As [`serialize_exposed`] for an optional secret.
pub fn serialize_exposed_opt<T: serde::Serialize + Zeroize, S: Serializer>(
    secret: &Option<Secret<T>>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match secret {
        Some(v) => s.serialize_some(&v.0),
        None => s.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_redact() {
        let s = Secret::new("hunter2".to_string());
        assert_eq!(format!("{s:?}"), "[redacted]");
        assert_eq!(format!("{s}"), "[redacted]");
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn zeroize_wipes_the_value() {
        let mut s = Secret::new("hunter2".to_string());
        s.0.zeroize();
        assert!(s.expose().is_empty());
    }
}
