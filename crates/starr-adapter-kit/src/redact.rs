//! Central redaction of secrets in log lines and error details.

use secrecy::{ExposeSecret, SecretString};

const MASK: &str = "[redacted]";

/// Replaces every occurrence of known secrets in text taken from foreign
/// responses or transport errors before it is logged or stored.
#[derive(Debug)]
pub struct Redactor {
    secrets: Vec<SecretString>,
}

impl Redactor {
    /// Empty secrets are ignored (they would match everywhere).
    #[must_use]
    pub fn new(secrets: Vec<SecretString>) -> Self {
        Self {
            secrets: secrets
                .into_iter()
                .filter(|s| !s.expose_secret().is_empty())
                .collect(),
        }
    }

    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for secret in &self.secrets {
            let value = secret.expose_secret();
            if out.contains(value) {
                out = out.replace(value, MASK);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_secrets_are_masked_everywhere() {
        let r = Redactor::new(vec![
            SecretString::from("abc123".to_owned()),
            SecretString::from(String::new()),
        ]);
        assert_eq!(
            r.redact("key abc123 in url ?apikey=abc123"),
            "key [redacted] in url ?apikey=[redacted]"
        );
        assert_eq!(r.redact("nothing here"), "nothing here");
    }
}
