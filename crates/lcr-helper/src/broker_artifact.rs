use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;

pub(crate) const BROKER_SIGNED_MANIFEST_RESOURCE_ID: u16 = 102;

const BROKER_ARTIFACT_MAGIC: &[u8; 8] = b"LCRBSIG1";
const BROKER_ARTIFACT_FORMAT_VERSION: u16 = 1;
const BROKER_ARTIFACT_HEADER_LEN: usize = 52;
const ED25519_SIGNATURE_LEN: usize = 64;
const BROKER_ARTIFACT_MAX_VERSION_BYTES: usize = 128;

#[cfg(windows)]
pub(crate) const BROKER_SIGNED_MANIFEST_MAX_BYTES: usize =
    BROKER_ARTIFACT_HEADER_LEN + BROKER_ARTIFACT_MAX_VERSION_BYTES + ED25519_SIGNATURE_LEN;

const PINNED_BROKER_SIGNING_PUBLIC_KEY: &[u8; 32] =
    include_bytes!("../assets/broker-signing-public-key.bin");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrokerArtifactVerificationError {
    MalformedManifest,
    UnsupportedFormat,
    InvalidVersion,
    NotStrictlyNewer,
    NotCurrentVersion,
    PayloadLengthMismatch,
    InvalidPublicKey,
    InvalidSignature,
    PayloadHashMismatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedBrokerArtifact {
    target_version: String,
    payload_length: u64,
    payload_sha256: [u8; 32],
}

impl VerifiedBrokerArtifact {
    pub(crate) fn target_version(&self) -> &str {
        &self.target_version
    }

    pub(crate) fn payload_length(&self) -> u64 {
        self.payload_length
    }

    pub(crate) fn payload_sha256(&self) -> &[u8; 32] {
        &self.payload_sha256
    }
}

struct ParsedManifest<'a> {
    target_version: &'a str,
    payload_length: u64,
    payload_sha256: [u8; 32],
    signed_message: &'a [u8],
    signature: Signature,
}

#[derive(Clone, Copy)]
enum TargetVersionPolicy {
    StrictlyNewer,
    ExactCurrent,
}

fn canonical_version(value: &str) -> Option<Version> {
    if value.is_empty() || value.len() > BROKER_ARTIFACT_MAX_VERSION_BYTES {
        return None;
    }

    let parsed = Version::parse(value).ok()?;

    if parsed.to_string() != value {
        return None;
    }

    Some(parsed)
}

fn parse_manifest(manifest: &[u8]) -> Result<ParsedManifest<'_>, BrokerArtifactVerificationError> {
    let minimum_length = BROKER_ARTIFACT_HEADER_LEN
        .checked_add(1)
        .and_then(|value| value.checked_add(ED25519_SIGNATURE_LEN))
        .ok_or(BrokerArtifactVerificationError::MalformedManifest)?;

    if manifest.len() < minimum_length {
        return Err(BrokerArtifactVerificationError::MalformedManifest);
    }

    if manifest.get(0..8) != Some(BROKER_ARTIFACT_MAGIC.as_slice()) {
        return Err(BrokerArtifactVerificationError::UnsupportedFormat);
    }

    let format_version = u16::from_le_bytes(
        manifest
            .get(8..10)
            .ok_or(BrokerArtifactVerificationError::MalformedManifest)?
            .try_into()
            .map_err(|_| BrokerArtifactVerificationError::MalformedManifest)?,
    );

    if format_version != BROKER_ARTIFACT_FORMAT_VERSION {
        return Err(BrokerArtifactVerificationError::UnsupportedFormat);
    }

    let version_length = usize::from(u16::from_le_bytes(
        manifest
            .get(10..12)
            .ok_or(BrokerArtifactVerificationError::MalformedManifest)?
            .try_into()
            .map_err(|_| BrokerArtifactVerificationError::MalformedManifest)?,
    ));

    if version_length == 0 || version_length > BROKER_ARTIFACT_MAX_VERSION_BYTES {
        return Err(BrokerArtifactVerificationError::InvalidVersion);
    }

    let payload_length = u64::from_le_bytes(
        manifest
            .get(12..20)
            .ok_or(BrokerArtifactVerificationError::MalformedManifest)?
            .try_into()
            .map_err(|_| BrokerArtifactVerificationError::MalformedManifest)?,
    );

    if payload_length == 0 {
        return Err(BrokerArtifactVerificationError::PayloadLengthMismatch);
    }

    let mut payload_sha256 = [0u8; 32];

    payload_sha256.copy_from_slice(
        manifest
            .get(20..52)
            .ok_or(BrokerArtifactVerificationError::MalformedManifest)?,
    );

    let signature_offset = BROKER_ARTIFACT_HEADER_LEN
        .checked_add(version_length)
        .ok_or(BrokerArtifactVerificationError::MalformedManifest)?;

    let expected_length = signature_offset
        .checked_add(ED25519_SIGNATURE_LEN)
        .ok_or(BrokerArtifactVerificationError::MalformedManifest)?;

    if manifest.len() != expected_length {
        return Err(BrokerArtifactVerificationError::MalformedManifest);
    }

    let version_bytes = manifest
        .get(BROKER_ARTIFACT_HEADER_LEN..signature_offset)
        .ok_or(BrokerArtifactVerificationError::MalformedManifest)?;

    let target_version = std::str::from_utf8(version_bytes)
        .map_err(|_| BrokerArtifactVerificationError::InvalidVersion)?;

    canonical_version(target_version).ok_or(BrokerArtifactVerificationError::InvalidVersion)?;

    let signature_bytes: [u8; ED25519_SIGNATURE_LEN] = manifest
        .get(signature_offset..expected_length)
        .ok_or(BrokerArtifactVerificationError::MalformedManifest)?
        .try_into()
        .map_err(|_| BrokerArtifactVerificationError::MalformedManifest)?;

    Ok(ParsedManifest {
        target_version,
        payload_length,
        payload_sha256,
        signed_message: &manifest[..signature_offset],
        signature: Signature::from_bytes(&signature_bytes),
    })
}

fn verify_manifest_with_key_and_policy(
    current_version: &str,
    manifest: &[u8],
    public_key: &[u8; 32],
    policy: TargetVersionPolicy,
) -> Result<VerifiedBrokerArtifact, BrokerArtifactVerificationError> {
    let parsed = parse_manifest(manifest)?;

    let current = canonical_version(current_version)
        .ok_or(BrokerArtifactVerificationError::InvalidVersion)?;

    let target = canonical_version(parsed.target_version)
        .ok_or(BrokerArtifactVerificationError::InvalidVersion)?;

    match policy {
        TargetVersionPolicy::StrictlyNewer
            if target.cmp_precedence(&current) != Ordering::Greater =>
        {
            return Err(BrokerArtifactVerificationError::NotStrictlyNewer);
        }

        TargetVersionPolicy::ExactCurrent if parsed.target_version != current_version => {
            return Err(BrokerArtifactVerificationError::NotCurrentVersion);
        }

        _ => {}
    }

    let verifying_key = VerifyingKey::from_bytes(public_key)
        .map_err(|_| BrokerArtifactVerificationError::InvalidPublicKey)?;

    verifying_key
        .verify_strict(parsed.signed_message, &parsed.signature)
        .map_err(|_| BrokerArtifactVerificationError::InvalidSignature)?;

    Ok(VerifiedBrokerArtifact {
        target_version: parsed.target_version.to_owned(),
        payload_length: parsed.payload_length,
        payload_sha256: parsed.payload_sha256,
    })
}

fn verify_manifest_with_key(
    current_version: &str,
    manifest: &[u8],
    public_key: &[u8; 32],
) -> Result<VerifiedBrokerArtifact, BrokerArtifactVerificationError> {
    verify_manifest_with_key_and_policy(
        current_version,
        manifest,
        public_key,
        TargetVersionPolicy::StrictlyNewer,
    )
}

fn verify_payload_against_artifact(
    artifact: &VerifiedBrokerArtifact,
    payload: &[u8],
) -> Result<(), BrokerArtifactVerificationError> {
    let observed_payload_length = u64::try_from(payload.len())
        .map_err(|_| BrokerArtifactVerificationError::PayloadLengthMismatch)?;

    if observed_payload_length != artifact.payload_length {
        return Err(BrokerArtifactVerificationError::PayloadLengthMismatch);
    }

    let observed_payload_sha256: [u8; 32] = Sha256::digest(payload).into();

    if observed_payload_sha256 != artifact.payload_sha256 {
        return Err(BrokerArtifactVerificationError::PayloadHashMismatch);
    }

    Ok(())
}

fn verify_with_key(
    current_version: &str,
    manifest: &[u8],
    payload: &[u8],
    public_key: &[u8; 32],
) -> Result<VerifiedBrokerArtifact, BrokerArtifactVerificationError> {
    let artifact = verify_manifest_with_key(current_version, manifest, public_key)?;

    verify_payload_against_artifact(&artifact, payload)?;

    Ok(artifact)
}

fn verify_exact_current_with_key(
    current_version: &str,
    manifest: &[u8],
    payload: &[u8],
    public_key: &[u8; 32],
) -> Result<VerifiedBrokerArtifact, BrokerArtifactVerificationError> {
    let artifact = verify_manifest_with_key_and_policy(
        current_version,
        manifest,
        public_key,
        TargetVersionPolicy::ExactCurrent,
    )?;

    verify_payload_against_artifact(&artifact, payload)?;

    Ok(artifact)
}

/// Authenticates signed Broker metadata before any payload bytes are consumed.
///
/// The caller cannot supply a trust key, source version, filesystem path, Task
/// name, URL, or install authority. This gate verifies the pinned publisher
/// signature and strictly-newer canonical target only; it performs no mutation.
pub(crate) fn verify_current_broker_manifest(
    manifest: &[u8],
) -> Result<VerifiedBrokerArtifact, BrokerArtifactVerificationError> {
    verify_manifest_with_key(
        crate::distribution::CURRENT_VERSION,
        manifest,
        PINNED_BROKER_SIGNING_PUBLIC_KEY,
    )
}

/// Compatibility verifier for callers that already hold the complete payload.
///
/// A3B streaming migration must not use this API to buffer the PE. It exists so
/// existing artifact tests and non-streaming verification preserve their exact
/// length/hash contract while the maintenance receiver authenticates metadata
/// first with `verify_current_broker_manifest`.
pub(crate) fn verify_current_broker_upgrade(
    manifest: &[u8],
    payload: &[u8],
) -> Result<VerifiedBrokerArtifact, BrokerArtifactVerificationError> {
    let artifact = verify_current_broker_manifest(manifest)?;

    verify_payload_against_artifact(&artifact, payload)?;

    Ok(artifact)
}

/// Proves that the signed Broker bundle carried by this Helper is exactly for
/// this Helper's own canonical package version.
///
/// Equal-current is accepted only here. The existing upgrade verifier retains
/// its strictly-newer policy, while both policies share the same parser,
/// pinned publisher key, strict Ed25519 verification, length check, and hash
/// check.
#[cfg(windows)]
pub(crate) fn verify_current_broker_bundle(
    manifest: &[u8],
    payload: &[u8],
) -> Result<VerifiedBrokerArtifact, BrokerArtifactVerificationError> {
    verify_exact_current_with_key(
        crate::distribution::CURRENT_VERSION,
        manifest,
        payload,
        PINNED_BROKER_SIGNING_PUBLIC_KEY,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const TEST_PRIVATE_KEY: [u8; 32] = [
        0x42, 0x11, 0x92, 0xC4, 0x33, 0x51, 0xA8, 0x02, 0xED, 0x91, 0x03, 0xA4, 0x72, 0x37, 0xC0,
        0x5A, 0x2D, 0x77, 0x19, 0x60, 0x9F, 0xB1, 0x55, 0x8E, 0x13, 0x9A, 0x63, 0xD2, 0x48, 0x80,
        0xBC, 0x07,
    ];

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&TEST_PRIVATE_KEY)
    }

    fn manifest_with_metadata(
        signing_key: &SigningKey,
        version: &str,
        declared_length: u64,
        declared_hash: [u8; 32],
    ) -> Vec<u8> {
        let version_length = u16::try_from(version.len()).expect("test version length");

        let mut manifest =
            Vec::with_capacity(BROKER_ARTIFACT_HEADER_LEN + version.len() + ED25519_SIGNATURE_LEN);

        manifest.extend_from_slice(BROKER_ARTIFACT_MAGIC);
        manifest.extend_from_slice(&BROKER_ARTIFACT_FORMAT_VERSION.to_le_bytes());
        manifest.extend_from_slice(&version_length.to_le_bytes());
        manifest.extend_from_slice(&declared_length.to_le_bytes());
        manifest.extend_from_slice(&declared_hash);
        manifest.extend_from_slice(version.as_bytes());

        let signature = signing_key.sign(&manifest);
        manifest.extend_from_slice(&signature.to_bytes());

        manifest
    }

    fn valid_manifest(signing_key: &SigningKey, version: &str, payload: &[u8]) -> Vec<u8> {
        let hash: [u8; 32] = Sha256::digest(payload).into();

        manifest_with_metadata(signing_key, version, payload.len() as u64, hash)
    }

    #[test]
    fn valid_strictly_newer_signed_artifact_is_accepted_with_matching_key() {
        let key = signing_key();
        let public_key = key.verifying_key().to_bytes();
        let payload = b"valid broker artifact";
        let manifest = valid_manifest(&key, "0.0.5-beta", payload);

        let manifest_only = verify_manifest_with_key("0.0.4-beta", &manifest, &public_key).unwrap();

        assert_eq!(manifest_only.target_version(), "0.0.5-beta");
        assert_eq!(manifest_only.payload_length(), payload.len() as u64);

        let full = verify_with_key("0.0.4-beta", &manifest, payload, &public_key).unwrap();

        assert_eq!(manifest_only, full);
    }

    #[test]
    fn exact_current_signed_bundle_is_accepted_with_matching_test_key() {
        let key = signing_key();
        let public_key = key.verifying_key().to_bytes();
        let payload = b"current broker artifact";
        let manifest = valid_manifest(&key, "0.0.4-beta", payload);
        let payload_sha256: [u8; 32] = Sha256::digest(payload).into();

        let artifact =
            verify_exact_current_with_key("0.0.4-beta", &manifest, payload, &public_key).unwrap();

        assert_eq!(artifact.target_version(), "0.0.4-beta");
        assert_eq!(artifact.payload_length(), payload.len() as u64);
        assert_eq!(artifact.payload_sha256(), &payload_sha256);
    }

    #[test]
    fn exact_current_policy_rejects_higher_lower_and_textually_different_targets() {
        let key = signing_key();
        let public_key = key.verifying_key().to_bytes();
        let payload = b"MZ-current-version";

        for target in ["0.0.5-beta", "0.0.3-beta", "0.0.4-beta+build.7"] {
            let manifest = valid_manifest(&key, target, payload);

            assert_eq!(
                verify_exact_current_with_key("0.0.4-beta", &manifest, payload, &public_key),
                Err(BrokerArtifactVerificationError::NotCurrentVersion),
                "target={target}"
            );
        }

        let noncanonical = valid_manifest(&key, "00.0.4-beta", payload);

        assert_eq!(
            verify_exact_current_with_key("0.0.4-beta", &noncanonical, payload, &public_key,),
            Err(BrokerArtifactVerificationError::InvalidVersion)
        );
    }

    #[test]
    fn exact_current_policy_rejects_length_hash_and_signature_mismatches() {
        let key = signing_key();
        let public_key = key.verifying_key().to_bytes();
        let payload = b"MZ-current-payload";
        let hash: [u8; 32] = Sha256::digest(payload).into();

        let wrong_length =
            manifest_with_metadata(&key, "0.0.4-beta", payload.len() as u64 + 1, hash);

        assert_eq!(
            verify_exact_current_with_key("0.0.4-beta", &wrong_length, payload, &public_key,),
            Err(BrokerArtifactVerificationError::PayloadLengthMismatch)
        );

        let wrong_hash =
            manifest_with_metadata(&key, "0.0.4-beta", payload.len() as u64, [0xA5; 32]);

        assert_eq!(
            verify_exact_current_with_key("0.0.4-beta", &wrong_hash, payload, &public_key,),
            Err(BrokerArtifactVerificationError::PayloadHashMismatch)
        );

        let mut wrong_signature = valid_manifest(&key, "0.0.4-beta", payload);
        let last = wrong_signature.len() - 1;
        wrong_signature[last] ^= 1;

        assert_eq!(
            verify_exact_current_with_key("0.0.4-beta", &wrong_signature, payload, &public_key,),
            Err(BrokerArtifactVerificationError::InvalidSignature)
        );
    }

    #[test]
    fn production_entrypoint_does_not_accept_a_test_signing_identity() {
        let key = signing_key();
        let payload = b"test-signed broker artifact";
        let current = semver::Version::parse(crate::distribution::CURRENT_VERSION)
            .expect("canonical current version");
        let next_patch = current.patch.checked_add(1).expect("test next patch");
        let target = format!("{}.{}.{}", current.major, current.minor, next_patch);
        let manifest = valid_manifest(&key, &target, payload);

        assert_eq!(
            verify_current_broker_manifest(&manifest),
            Err(BrokerArtifactVerificationError::InvalidSignature)
        );

        assert_eq!(
            verify_current_broker_upgrade(&manifest, payload),
            Err(BrokerArtifactVerificationError::InvalidSignature)
        );
    }

    #[test]
    fn one_byte_payload_change_is_rejected() {
        let key = signing_key();
        let payload = b"MZ-original";
        let manifest = valid_manifest(&key, "0.0.5-beta", payload);

        let changed = b"MZ-originaL";

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                &manifest,
                changed,
                &key.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::PayloadHashMismatch)
        );
    }

    #[test]
    fn declared_payload_length_mismatch_is_rejected() {
        let key = signing_key();
        let payload = b"MZ-length";
        let hash: [u8; 32] = Sha256::digest(payload).into();

        let manifest = manifest_with_metadata(&key, "0.0.5-beta", payload.len() as u64 + 1, hash);

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                &manifest,
                payload,
                &key.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::PayloadLengthMismatch)
        );
    }

    #[test]
    fn wrong_signed_payload_hash_is_rejected() {
        let key = signing_key();
        let payload = b"MZ-hash";
        let wrong_hash = [0xA5; 32];

        let manifest = manifest_with_metadata(&key, "0.0.5-beta", payload.len() as u64, wrong_hash);

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                &manifest,
                payload,
                &key.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::PayloadHashMismatch)
        );
    }

    #[test]
    fn changed_signature_is_rejected() {
        let key = signing_key();
        let payload = b"MZ-signature";
        let mut manifest = valid_manifest(&key, "0.0.5-beta", payload);

        let last = manifest.len() - 1;
        manifest[last] ^= 0x01;

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                &manifest,
                payload,
                &key.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::InvalidSignature)
        );
    }

    #[test]
    fn different_public_key_is_rejected() {
        let signer = signing_key();
        let other = SigningKey::from_bytes(&[0x31; 32]);
        let payload = b"MZ-key-binding";
        let manifest = valid_manifest(&signer, "0.0.5-beta", payload);

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                &manifest,
                payload,
                &other.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::InvalidSignature)
        );
    }

    #[test]
    fn malformed_and_unknown_manifest_shapes_fail_closed() {
        let key = signing_key();
        let payload = b"MZ-format";

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                b"short",
                payload,
                &key.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::MalformedManifest)
        );

        let mut wrong_magic = valid_manifest(&key, "0.0.5-beta", payload);
        wrong_magic[0] ^= 0x01;

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                &wrong_magic,
                payload,
                &key.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::UnsupportedFormat)
        );

        let mut wrong_format = valid_manifest(&key, "0.0.5-beta", payload);
        wrong_format[8..10].copy_from_slice(&2u16.to_le_bytes());

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                &wrong_format,
                payload,
                &key.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::UnsupportedFormat)
        );
    }

    #[test]
    fn noncanonical_and_oversized_versions_are_rejected() {
        let key = signing_key();
        let payload = b"MZ-version";

        let noncanonical = valid_manifest(&key, "01.2.3", payload);

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                &noncanonical,
                payload,
                &key.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::InvalidVersion)
        );

        let oversized = "1".repeat(BROKER_ARTIFACT_MAX_VERSION_BYTES + 1);
        let manifest = valid_manifest(&key, &oversized, payload);

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                &manifest,
                payload,
                &key.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::InvalidVersion)
        );
    }

    #[test]
    fn equal_same_precedence_and_downgrade_targets_are_rejected() {
        let key = signing_key();
        let payload = b"MZ-order";

        for target in ["0.0.4-beta", "0.0.4-beta+build.7", "0.0.3"] {
            let manifest = valid_manifest(&key, target, payload);

            assert_eq!(
                verify_with_key(
                    "0.0.4-beta",
                    &manifest,
                    payload,
                    &key.verifying_key().to_bytes(),
                ),
                Err(BrokerArtifactVerificationError::NotStrictlyNewer),
                "target={target}"
            );
        }
    }

    #[test]
    fn trailing_or_missing_signature_bytes_are_rejected() {
        let key = signing_key();
        let payload = b"MZ-shape";
        let manifest = valid_manifest(&key, "0.0.5-beta", payload);

        let truncated = &manifest[..manifest.len() - 1];

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                truncated,
                payload,
                &key.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::MalformedManifest)
        );

        let mut trailing = manifest.clone();
        trailing.push(0);

        assert_eq!(
            verify_with_key(
                "0.0.4-beta",
                &trailing,
                payload,
                &key.verifying_key().to_bytes(),
            ),
            Err(BrokerArtifactVerificationError::MalformedManifest)
        );
    }

    #[test]
    fn pinned_public_key_is_exactly_the_committed_raw_key_shape() {
        assert_eq!(PINNED_BROKER_SIGNING_PUBLIC_KEY.len(), 32);
        assert!(PINNED_BROKER_SIGNING_PUBLIC_KEY
            .iter()
            .any(|byte| *byte != 0));
        assert_eq!(BROKER_SIGNED_MANIFEST_RESOURCE_ID, 102);
    }
}
