//! Wrapper for secrets entered by the user.

use std::fmt;

/// A verification code, 2FA password or master password.
///
/// Does not implement `Clone` or reveal its contents through `Debug`, so the secret
/// cannot accidentally enter a log or be duplicated in memory.
///
/// Memory zeroing on drop will be added with the `zeroize` dependency
/// at stage 2 — we do not implement it manually.
pub struct Secret(String);

impl Secret {
    /// Wraps a value, taking ownership of it.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Access to the contents. Each call is a place to examine during review.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_leak_the_value() {
        let secret = Secret::new("private input".to_owned());

        assert!(!format!("{secret:?}").contains("private input"));
    }

    #[test]
    fn expose_returns_the_original_value() {
        let secret = Secret::new("private input".to_owned());

        assert_eq!(secret.expose(), "private input");
    }
}
