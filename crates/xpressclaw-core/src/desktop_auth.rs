//! Shared wire-cryptography for Desktop instance authentication.
//!
//! The server and Desktop client intentionally use one implementation of the
//! transcript, KDF, and associated data so the credential-channel protocol
//! cannot drift between crates.

use ring::hkdf::{Salt, HKDF_SHA256};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const IDENTITY_PROOF_DOMAIN: &[u8] = b"xpressclaw-desktop-identity-v1\0";
const CREDENTIAL_CHANNEL_PROOF_DOMAIN: &[u8] = b"xpressclaw-desktop-credential-channel-v1\0";
const CREDENTIAL_CHANNEL_KDF_DOMAIN: &[u8] = b"xpressclaw-desktop-credential-kdf-v1\0";
const CREDENTIAL_CHANNEL_AAD_DOMAIN: &[u8] = b"xpressclaw-desktop-credential-aad-v1\0";
pub const CREDENTIAL_REQUEST_DIRECTION: &[u8] = b"request";
pub const CREDENTIAL_RESPONSE_DIRECTION: &[u8] = b"response";
pub const CREDENTIAL_CHANNEL_NONCE: [u8; 12] = [0; 12];
pub const BROWSER_SESSION_COOKIE: &str = "xpressclaw_session";
pub const BROWSER_SESSION_LIFETIME_SECONDS: u64 = 12 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopCredentialPurpose {
    Validate,
    BrowserSession,
}

impl DesktopCredentialPurpose {
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Validate => b"validate",
            Self::BrowserSession => b"browser_session",
        }
    }
}

pub struct DesktopCredentialKeys {
    pub request: Zeroizing<[u8; 32]>,
    pub response: Zeroizing<[u8; 32]>,
}

pub fn identity_proof_message(instance_id: &str, challenge: &[u8]) -> Vec<u8> {
    let mut message =
        Vec::with_capacity(IDENTITY_PROOF_DOMAIN.len() + instance_id.len() + 1 + challenge.len());
    message.extend_from_slice(IDENTITY_PROOF_DOMAIN);
    message.extend_from_slice(instance_id.as_bytes());
    message.push(0);
    message.extend_from_slice(challenge);
    message
}

pub fn credential_proof_message(
    instance_id: &str,
    challenge: &[u8],
    exchange_id: &[u8; 32],
    client_public_key: &[u8; 32],
    server_public_key: &[u8; 32],
) -> Vec<u8> {
    let mut message = Vec::with_capacity(
        CREDENTIAL_CHANNEL_PROOF_DOMAIN.len()
            + instance_id.len()
            + 1
            + challenge.len()
            + exchange_id.len()
            + client_public_key.len()
            + server_public_key.len(),
    );
    message.extend_from_slice(CREDENTIAL_CHANNEL_PROOF_DOMAIN);
    message.extend_from_slice(instance_id.as_bytes());
    message.push(0);
    message.extend_from_slice(challenge);
    message.extend_from_slice(exchange_id);
    message.extend_from_slice(client_public_key);
    message.extend_from_slice(server_public_key);
    message
}

pub fn derive_credential_keys(
    shared_secret: &[u8],
    instance_id: &str,
    challenge: &[u8],
    exchange_id: &[u8; 32],
    client_public_key: &[u8; 32],
    server_public_key: &[u8; 32],
) -> Result<DesktopCredentialKeys, ring::error::Unspecified> {
    let transcript = credential_proof_message(
        instance_id,
        challenge,
        exchange_id,
        client_public_key,
        server_public_key,
    );
    let salt = Salt::new(HKDF_SHA256, exchange_id);
    let prk = salt.extract(shared_secret);
    let mut request = Zeroizing::new([0u8; 32]);
    prk.expand(
        &[
            CREDENTIAL_CHANNEL_KDF_DOMAIN,
            &transcript,
            CREDENTIAL_REQUEST_DIRECTION,
        ],
        HKDF_SHA256,
    )?
    .fill(request.as_mut())?;
    let mut response = Zeroizing::new([0u8; 32]);
    prk.expand(
        &[
            CREDENTIAL_CHANNEL_KDF_DOMAIN,
            &transcript,
            CREDENTIAL_RESPONSE_DIRECTION,
        ],
        HKDF_SHA256,
    )?
    .fill(response.as_mut())?;
    Ok(DesktopCredentialKeys { request, response })
}

pub fn credential_aad(
    instance_id: &str,
    exchange_id: &[u8; 32],
    direction: &[u8],
    purpose: DesktopCredentialPurpose,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(
        CREDENTIAL_CHANNEL_AAD_DOMAIN.len()
            + instance_id.len()
            + 1
            + exchange_id.len()
            + direction.len()
            + purpose.as_bytes().len(),
    );
    aad.extend_from_slice(CREDENTIAL_CHANNEL_AAD_DOMAIN);
    aad.extend_from_slice(instance_id.as_bytes());
    aad.push(0);
    aad.extend_from_slice(exchange_id);
    aad.extend_from_slice(direction);
    aad.extend_from_slice(purpose.as_bytes());
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_and_direction_keys_are_domain_separated() {
        let exchange = [1u8; 32];
        let client = [2u8; 32];
        let server = [3u8; 32];
        let first = derive_credential_keys(
            &[4u8; 32], "instance", &[5u8; 32], &exchange, &client, &server,
        )
        .unwrap();
        let second = derive_credential_keys(
            &[4u8; 32],
            "other-instance",
            &[5u8; 32],
            &exchange,
            &client,
            &server,
        )
        .unwrap();
        assert_ne!(*first.request, *first.response);
        assert_ne!(*first.request, *second.request);
        assert_ne!(
            credential_aad(
                "instance",
                &exchange,
                CREDENTIAL_REQUEST_DIRECTION,
                DesktopCredentialPurpose::Validate,
            ),
            credential_aad(
                "instance",
                &exchange,
                CREDENTIAL_RESPONSE_DIRECTION,
                DesktopCredentialPurpose::Validate,
            )
        );
        assert_ne!(
            credential_aad(
                "instance",
                &exchange,
                CREDENTIAL_REQUEST_DIRECTION,
                DesktopCredentialPurpose::Validate,
            ),
            credential_aad(
                "instance",
                &exchange,
                CREDENTIAL_REQUEST_DIRECTION,
                DesktopCredentialPurpose::BrowserSession,
            )
        );
    }
}
