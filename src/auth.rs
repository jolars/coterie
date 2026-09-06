//! Session-scoped agent credentials.

use std::fmt;
use std::str::FromStr;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::Sha256;
use thiserror::Error;

use crate::id::{AgentId, RunId, SessionId};

const TOKEN_BYTES: usize = 32;
const TOKEN_PREFIX: &str = "cot1_";
const VERIFIER_DOMAIN: &[u8] = b"coterie.session-token.verifier.v1\0";

type HmacSha256 = Hmac<Sha256>;

/// A bearer credential supplied only to one provider session.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct AgentToken([u8; TOKEN_BYTES]);

impl AgentToken {
    /// Generates a credential from the operating system's random source.
    #[allow(
        dead_code,
        reason = "credential issuance is consumed by the next M2 provider-lifecycle item"
    )]
    pub(crate) fn generate() -> Result<Self, TokenGenerationError> {
        let mut bytes = [0_u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(TokenGenerationError)?;
        Ok(Self(bytes))
    }

    /// Returns the value used for `COTERIE_TOKEN` and RPC authentication.
    #[must_use]
    pub(crate) fn expose_secret(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(TOKEN_PREFIX.len() + 64);
        encoded.push_str(TOKEN_PREFIX);
        for byte in self.0 {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        encoded
    }

    /// Derives the durable verifier for exactly one run, agent, and session generation.
    #[must_use]
    #[allow(
        dead_code,
        reason = "credential issuance is consumed by the next M2 provider-lifecycle item"
    )]
    pub(crate) fn verifier(&self, scope: SessionScope) -> TokenVerifier {
        TokenVerifier(self.mac(scope).finalize().into_bytes().into())
    }

    fn mac(&self, scope: SessionScope) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(&self.0)
            .expect("HMAC accepts keys of every size");
        mac.update(VERIFIER_DOMAIN);
        mac.update(scope.run_id.to_string().as_bytes());
        mac.update(&[0]);
        mac.update(scope.agent_id.to_string().as_bytes());
        mac.update(&[0]);
        mac.update(scope.session_id.to_string().as_bytes());
        mac.update(&[0]);
        mac.update(&scope.generation.to_be_bytes());
        mac
    }
}

impl fmt::Debug for AgentToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AgentToken([REDACTED])")
    }
}

impl FromStr for AgentToken {
    type Err = ParseTokenError;

    fn from_str(encoded: &str) -> Result<Self, Self::Err> {
        let payload =
            encoded.strip_prefix(TOKEN_PREFIX).ok_or(ParseTokenError)?;
        if payload.len() != TOKEN_BYTES * 2 {
            return Err(ParseTokenError);
        }

        let mut bytes = [0_u8; TOKEN_BYTES];
        let (pairs, remainder) = payload.as_bytes().as_chunks::<2>();
        debug_assert!(remainder.is_empty());
        for (index, pair) in pairs.iter().enumerate() {
            bytes[index] =
                (decode_nibble(pair[0])? << 4) | decode_nibble(pair[1])?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for AgentToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.expose_secret())
    }
}

impl<'de> Deserialize<'de> for AgentToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(<D::Error as serde::de::Error>::custom)
    }
}

fn decode_nibble(encoded: u8) -> Result<u8, ParseTokenError> {
    match encoded {
        b'0'..=b'9' => Ok(encoded - b'0'),
        b'a'..=b'f' => Ok(encoded - b'a' + 10),
        _ => Err(ParseTokenError),
    }
}

/// The full scope that fences a session credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SessionScope {
    pub(crate) run_id: RunId,
    pub(crate) agent_id: AgentId,
    pub(crate) session_id: SessionId,
    pub(crate) generation: i64,
}

/// A non-secret digest safe to store in durable state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TokenVerifier([u8; TOKEN_BYTES]);

impl TokenVerifier {
    #[must_use]
    pub(crate) fn from_bytes(bytes: [u8; TOKEN_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub(crate) fn as_bytes(&self) -> &[u8; TOKEN_BYTES] {
        &self.0
    }

    /// Verifies the supplied token with a constant-time MAC comparison.
    #[must_use]
    pub(crate) fn verify(
        &self,
        token: &AgentToken,
        scope: SessionScope,
    ) -> bool {
        token.mac(scope).verify_slice(&self.0).is_ok()
    }
}

/// Failure to obtain cryptographic randomness for a new session token.
#[derive(Debug, Error)]
#[error("could not obtain randomness for an agent token: {0}")]
#[allow(
    dead_code,
    reason = "credential issuance is consumed by the next M2 provider-lifecycle item"
)]
pub(crate) struct TokenGenerationError(#[source] getrandom::Error);

/// A malformed opaque session token.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("invalid agent token encoding")]
pub(crate) struct ParseTokenError;

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{AgentToken, SessionScope};
    use crate::id::{AgentId, RunId, SessionId};

    const TOKEN: &str =
        "cot1_000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const AGENT_ID: &str = "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const SESSION_ID: &str = "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY";

    #[test]
    fn generated_tokens_are_random_and_have_a_canonical_encoding() {
        let first =
            AgentToken::generate().expect("randomness should be available");
        let second =
            AgentToken::generate().expect("randomness should be available");

        assert_ne!(first, second);
        assert_eq!(first.expose_secret().len(), TOKEN.len());
        assert_eq!(
            AgentToken::from_str(&first.expose_secret())
                .expect("a generated token should parse"),
            first
        );
    }

    #[test]
    fn tokens_are_redacted_from_debug_output_and_parse_errors() {
        let token = AgentToken::from_str(TOKEN)
            .expect("the fixture token should parse");

        assert_eq!(format!("{token:?}"), "AgentToken([REDACTED])");
        let error = AgentToken::from_str("not-a-token")
            .expect_err("an invalid token should fail");
        assert!(!error.to_string().contains("not-a-token"));
    }

    #[test]
    fn verifiers_bind_tokens_to_the_complete_session_scope() {
        let token = AgentToken::from_str(TOKEN)
            .expect("the fixture token should parse");
        let scope = scope();
        let verifier = token.verifier(scope);

        assert!(verifier.verify(&token, scope));
        let other_scopes = [
            SessionScope {
                run_id: RunId::generate(),
                ..scope
            },
            SessionScope {
                agent_id: AgentId::generate(),
                ..scope
            },
            SessionScope {
                session_id: SessionId::generate(),
                ..scope
            },
            SessionScope {
                generation: scope.generation + 1,
                ..scope
            },
        ];
        assert!(
            other_scopes
                .into_iter()
                .all(|other_scope| !verifier.verify(&token, other_scope))
        );
        assert!(!verifier.verify(
            &AgentToken::generate().expect("randomness should be available"),
            scope
        ));
    }

    fn scope() -> SessionScope {
        SessionScope {
            run_id: RUN_ID.parse::<RunId>().expect("valid run ID"),
            agent_id: AGENT_ID.parse::<AgentId>().expect("valid agent ID"),
            session_id: SESSION_ID
                .parse::<SessionId>()
                .expect("valid session ID"),
            generation: 2,
        }
    }
}
