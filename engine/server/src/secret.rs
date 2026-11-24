//! A wrapper for credential values so a stray `{:?}` or `{}` cannot leak them
//! (WI-4656). The value is reachable only through [`Secret::expose`].

use serde::{Deserialize, Deserializer, Serializer};

/// A credential. `Debug` and `Display` print `[redacted]`.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The wrapped value. Calling this is the explicit decision to use it.
    pub fn expose(&self) -> &T {
        &self.0
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<T> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<T> std::fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Secret<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Self)
    }
}

/// `#[serde(serialize_with)]` helper: emit the inner value, for a secret that
/// is meant to go over the wire.
pub fn serialize_exposed<T: serde::Serialize, S: Serializer>(
    secret: &Secret<T>,
    s: S,
) -> Result<S::Ok, S::Error> {
    secret.0.serialize(s)
}

/// As [`serialize_exposed`] for an optional secret.
pub fn serialize_exposed_opt<T: serde::Serialize, S: Serializer>(
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
}
