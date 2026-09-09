//! Credential filtering at Coterie's storage and diagnostic boundaries.

use std::os::unix::ffi::OsStrExt;

const REPLACEMENT: &[u8] = b"[REDACTED]";
const TOKEN_PREFIX: &[u8] = b"cot1_";
const TOKEN_LENGTH: usize = 69;

/// Retains only an ambiguous suffix between chunks, so a split secret is never written.
#[derive(Default)]
pub(crate) struct Redactor {
    secrets: Vec<Vec<u8>>,
    pending: Vec<u8>,
}

impl Redactor {
    pub(crate) fn from_environment() -> Self {
        let mut redactor = Self::default();
        for name in ["OPENAI_API_KEY", "COTERIE_TOKEN"] {
            if let Some(value) = std::env::var_os(name) {
                redactor.add(value.as_os_str().as_bytes());
            }
        }
        redactor
    }

    pub(crate) fn add(&mut self, secret: &[u8]) {
        if !secret.is_empty() && !self.secrets.iter().any(|s| s == secret) {
            self.secrets.push(secret.to_vec());
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        self.drain(false)
    }

    /// Conceals an unfinished credential prefix if a stream ends or is interrupted.
    pub(crate) fn finish(&mut self) -> Vec<u8> {
        self.drain(true)
    }

    fn drain(&mut self, end: bool) -> Vec<u8> {
        let mut output = Vec::new();
        let mut offset = 0;
        while offset < self.pending.len() {
            let remaining = &self.pending[offset..];
            let token_prefix = remaining.len() < TOKEN_LENGTH
                && (TOKEN_PREFIX.starts_with(remaining)
                    || (remaining.starts_with(TOKEN_PREFIX)
                        && remaining[TOKEN_PREFIX.len()..]
                            .iter()
                            .all(u8::is_ascii_hexdigit)));
            let partial = token_prefix
                || self.secrets.iter().any(|secret| {
                    remaining.len() < secret.len()
                        && secret.starts_with(remaining)
                });
            if partial {
                if end {
                    output.extend_from_slice(REPLACEMENT);
                    offset = self.pending.len();
                }
                break;
            }
            let exact = self
                .secrets
                .iter()
                .filter(|s| remaining.starts_with(s))
                .map(Vec::len)
                .max();
            let token = (remaining.len() >= TOKEN_LENGTH
                && remaining.starts_with(TOKEN_PREFIX)
                && remaining[TOKEN_PREFIX.len()..TOKEN_LENGTH]
                    .iter()
                    .all(u8::is_ascii_hexdigit))
            .then_some(TOKEN_LENGTH);
            if let Some(length) = exact.into_iter().chain(token).max() {
                output.extend_from_slice(REPLACEMENT);
                offset += length;
            } else {
                output.push(remaining[0]);
                offset += 1;
            }
        }
        self.pending.drain(..offset);
        output
    }
}

/// Redacts complete values without treating ordinary text suffixes as stream tails.
pub(crate) fn text(value: &str) -> String {
    let mut redactor = Redactor::from_environment();
    // A delimiter resolves partial prefixes without changing the caller's text.
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    let mut output = redactor.push(&bytes);
    output.extend(redactor.finish());
    output.pop();
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_split_of_multiple_credentials_is_redacted() {
        let token = format!("cot1_{}", "ab".repeat(32));
        let input = format!("before {token} api-secret after\n");
        for split in 0..=input.len() {
            let mut redactor = Redactor::default();
            redactor.add(b"api-secret");
            let mut output = redactor.push(&input.as_bytes()[..split]);
            output.extend(redactor.push(&input.as_bytes()[split..]));
            output.extend(redactor.finish());
            assert_eq!(
                output, b"before [REDACTED] [REDACTED] after\n",
                "split {split}"
            );
        }
    }

    #[test]
    fn unfinished_credentials_never_reach_storage() {
        let mut redactor = Redactor::default();
        redactor.add(b"api-secret");
        assert_eq!(redactor.push(b"output api-sec"), b"output ");
        assert_eq!(redactor.finish(), REPLACEMENT);
        assert_eq!(text("ordinary text c"), "ordinary text c");
    }
}
