use std::fmt;

/// A wrapper that redacts its inner value when displayed.
///
/// Useful for logging secrets without leaking them:
/// ```
/// use bccf_core::Redacted;
/// let token = Redacted::new("sk-ant-secret-key");
/// assert_eq!(format!("{token}"), "[REDACTED]");
/// ```
#[derive(Clone)]
pub struct Redacted<T> {
    inner: T,
}

impl<T> Redacted<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> T {
        self.inner
    }

    pub fn inner(&self) -> &T {
        &self.inner
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_redacted() {
        let secret = Redacted::new("sk-ant-api03-secret-key");
        assert_eq!(format!("{secret}"), "[REDACTED]");
    }

    #[test]
    fn debug_is_redacted() {
        let secret = Redacted::new("my-secret");
        assert_eq!(format!("{secret:?}"), "[REDACTED]");
    }

    #[test]
    fn inner_returns_value() {
        let secret = Redacted::new("real-value");
        assert_eq!(*secret.inner(), "real-value");
    }

    #[test]
    fn into_inner_returns_value() {
        let secret = Redacted::new(String::from("real-value"));
        assert_eq!(secret.into_inner(), "real-value");
    }
}
