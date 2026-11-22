//! The in-process Sigstore verification backend.
//!
//! `sigstore-rs` verifies the Fulcio chain, SCT, DSSE signature, subject digest,
//! tlog body consistency and certificate validity at integration time. This
//! module adds the checks it leaves as TODOs (SET + Merkle inclusion), plus
//! tuishou's strict GitHub identity and provenance policy.

use super::{AttestationReport, AttestationRequest};
use crate::error::{Error, Result};
use base64::Engine as _;
use const_oid::ObjectIdentifier;
use serde_json::Value;
use sigstore::bundle::verify::policy::{
    AllOf, Identity, OIDCIssuer, SingleX509ExtPolicy, VerificationPolicy,
};
use sigstore::bundle::verify::{blocking, ArtifactDigest};
use sigstore::crypto::{CosignVerificationKey, Signature, SigningScheme};
use sigstore::rekor::models::checkpoint::SignedCheckpoint;
use sigstore::rekor::models::inclusion_proof::InclusionProof as RekorInclusionProof;
use sigstore::trust::sigstore::SigstoreTrustRoot;
use sigstore::trust::TrustRoot as _;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::bundle::Content as BundleContent;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::verification_material::Content as MaterialContent;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::Bundle;
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::TransparencyLogEntry;
use x509_cert::der::Decode as _;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::Certificate;

const OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";
const SUBJECT_PREFIX: &str = "https://github.com/";
const IN_TOTO_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";
const PREDICATE_V1: &str = "https://slsa.dev/provenance/v1";
const PREDICATE_V0_2: &str = "https://slsa.dev/provenance/v0.2";

const OID_ISSUER: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.1");
// Fulcio certificate extensions (sigstore/fulcio docs/oid-info.md). The 1.12 to 1.14 group names
// the source repository, its digest and its ref; 1.5, 1.3 and 1.6 are the older workflow-level
// extensions. 1.18 to 1.20 are build config and trigger, not the source, and must not be used here.
const OID_SOURCE_REPO_NEW: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.12");
const OID_SOURCE_DIGEST_NEW: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.13");
const OID_SOURCE_REF_NEW: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.14");
const OID_SOURCE_REPO_LEGACY: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.5");
const OID_SOURCE_DIGEST_LEGACY: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.3");
const OID_SOURCE_REF_LEGACY: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.6");

#[derive(Debug)]
struct BundleMaterials {
    certificate: Certificate,
    tlog: TransparencyLogEntry,
    predicate_type: String,
}

/// Verify one bundle against the artifact and return the identity report.
pub fn verify_bundle(
    request: &AttestationRequest,
    trust_root: SigstoreTrustRoot,
    bundle_bytes: &[u8],
    artifact_digest: [u8; 32],
) -> Result<AttestationReport> {
    let bundle: Bundle = serde_json::from_slice(bundle_bytes).map_err(|e| {
        Error::AttestationRefused(format!("attestation bundle is not valid JSON: {e}"))
    })?;
    let materials = parse_materials(&bundle)?;
    let (identity, ref_value) = resolve_identity(&materials, request)?;

    check_certificate_policy(&materials, &identity, &ref_value, request)?;
    check_tlog(&materials.tlog, &trust_root)?;
    check_signing_time(&materials)?;

    let issuer = extension_text(&materials.certificate, OID_ISSUER).unwrap_or_default();
    let verifier = blocking::Verifier::new(Default::default(), trust_root)
        .map_err(|e| Error::Attestation(format!("cannot construct Sigstore verifier: {e}")))?;
    let issuer_policy = OIDCIssuer::new(OIDC_ISSUER);
    let identity_policy = Identity::new(&identity, OIDC_ISSUER);
    let policies: Vec<&dyn VerificationPolicy> = vec![&issuer_policy, &identity_policy];
    let policy = AllOf::new(policies)
        .ok_or_else(|| Error::Attestation("failed to construct verification policy".to_string()))?;
    verifier
        .verify_digest_with(ArtifactDigest::from(artifact_digest), bundle, &policy, true)
        .map_err(|e| {
            Error::AttestationRefused(format!("Sigstore bundle verification failed: {e}"))
        })?;

    Ok(AttestationReport {
        digest: super::hex_encode(&artifact_digest),
        issuer,
        identity,
        workflow: request.signer_workflow.clone(),
        ref_value,
        log_index: materials.tlog.log_index,
        predicate_type: materials.predicate_type,
    })
}

fn resolve_identity(
    materials: &BundleMaterials,
    request: &AttestationRequest,
) -> Result<(String, String)> {
    let prefix = format!(
        "{SUBJECT_PREFIX}{}/{}{}",
        request.repo, request.signer_workflow, "@"
    );
    if let Some(tag) = &request.tag {
        super::validate_tag(tag, "expected release tag")?;
        let ref_value = format!("refs/tags/{tag}");
        return Ok((format!("{prefix}{ref_value}"), ref_value));
    }
    let sans = subject_alt_names(&materials.certificate)?;
    let mut candidates: Vec<String> = sans
        .into_iter()
        .filter_map(|name| {
            let suffix = name.strip_prefix(&prefix)?.to_string();
            Some((name, suffix))
        })
        .filter(|(_, suffix)| suffix.starts_with("refs/tags/") && suffix.len() > "refs/tags/".len())
        .map(|(name, _)| name)
        .collect();
    candidates.sort();
    candidates.dedup();
    match candidates.as_slice() {
        [identity] => {
            let ref_value = identity
                .strip_prefix(&prefix)
                .expect("candidate was filtered by prefix")
                .to_string();
            Ok((identity.clone(), ref_value))
        }
        [] => Err(Error::AttestationRefused(format!(
            "no SAN workflow URI for {}/{} with a tag ref",
            request.repo, request.signer_workflow
        ))),
        _ => Err(Error::AttestationRefused(format!(
            "ambiguous SAN workflow URIs for {}/{}",
            request.repo, request.signer_workflow
        ))),
    }
}

fn parse_materials(bundle: &Bundle) -> Result<BundleMaterials> {
    let material = bundle
        .verification_material
        .as_ref()
        .ok_or_else(|| Error::AttestationRefused("bundle has no verification material".into()))?;
    let cert_der = match material.content.as_ref() {
        Some(MaterialContent::Certificate(cert)) => cert.raw_bytes.clone(),
        Some(MaterialContent::X509CertificateChain(chain)) => chain
            .certificates
            .first()
            .map(|c| c.raw_bytes.clone())
            .ok_or_else(|| Error::AttestationRefused("bundle certificate chain is empty".into()))?,
        _ => {
            return Err(Error::AttestationRefused(
                "bundle does not carry an X.509 signing certificate".into(),
            ))
        }
    };
    let certificate = Certificate::from_der(&cert_der).map_err(|e| {
        Error::AttestationRefused(format!("bundle signing certificate is malformed: {e}"))
    })?;
    if material.tlog_entries.len() != 1 {
        return Err(Error::AttestationRefused(format!(
            "bundle must contain exactly one transparency log entry, found {}",
            material.tlog_entries.len()
        )));
    }
    let tlog = material.tlog_entries[0].clone();
    let envelope = match bundle.content.as_ref() {
        Some(BundleContent::DsseEnvelope(envelope)) => envelope,
        _ => {
            return Err(Error::AttestationRefused(
                "bundle does not carry a DSSE envelope".into(),
            ))
        }
    };
    if envelope.payload_type != IN_TOTO_PAYLOAD_TYPE {
        return Err(Error::AttestationRefused(format!(
            "unsupported DSSE payload type '{}'",
            envelope.payload_type
        )));
    }
    // Protobuf JSON already base64-decodes byte fields into Vec<u8>.
    let payload = &envelope.payload;
    let statement: Value = serde_json::from_slice(payload).map_err(|e| {
        Error::AttestationRefused(format!("DSSE payload is not valid in-toto JSON: {e}"))
    })?;
    let predicate_type = statement
        .get("predicateType")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::AttestationRefused("in-toto statement has no predicateType".into()))?
        .to_string();
    if predicate_type != PREDICATE_V1 && predicate_type != PREDICATE_V0_2 {
        return Err(Error::AttestationRefused(format!(
            "unsupported provenance predicate type '{predicate_type}'"
        )));
    }
    Ok(BundleMaterials {
        certificate,
        tlog,
        predicate_type,
    })
}

fn check_certificate_policy(
    materials: &BundleMaterials,
    identity: &str,
    ref_value: &str,
    request: &AttestationRequest,
) -> Result<()> {
    let issuer = extension_text(&materials.certificate, OID_ISSUER).ok_or_else(|| {
        Error::AttestationRefused("certificate has no OIDC issuer extension".into())
    })?;
    if issuer != OIDC_ISSUER {
        return Err(Error::AttestationRefused(format!(
            "OIDC issuer mismatch: expected {OIDC_ISSUER}, got {issuer}"
        )));
    }
    let sans = subject_alt_names(&materials.certificate)?;
    if !sans.iter().any(|name| name == identity) {
        return Err(Error::AttestationRefused(format!(
            "workflow identity mismatch: expected {identity}, got {}",
            sans.join(", ")
        )));
    }

    let actual_ref = first_extension(
        &materials.certificate,
        &[OID_SOURCE_REF_NEW, OID_SOURCE_REF_LEGACY],
    )
    .ok_or_else(|| Error::AttestationRefused("certificate has no source ref extension".into()))?;
    let expected_source_ref = if ref_value.is_empty() {
        actual_ref.clone()
    } else {
        ref_value.to_string()
    };
    if actual_ref != expected_source_ref {
        return Err(Error::AttestationRefused(format!(
            "source ref mismatch: expected {expected_source_ref}, got {actual_ref}"
        )));
    }
    if !ref_value.is_empty() && !actual_ref.starts_with("refs/tags/") {
        return Err(Error::AttestationRefused(format!(
            "source ref is not a tag: {actual_ref}"
        )));
    }

    let repo_value = first_extension(
        &materials.certificate,
        &[OID_SOURCE_REPO_NEW, OID_SOURCE_REPO_LEGACY],
    )
    .ok_or_else(|| {
        Error::AttestationRefused("certificate has no source repository extension".into())
    })?;
    if !source_repo_matches(&repo_value, &request.repo) {
        return Err(Error::AttestationRefused(format!(
            "source repository mismatch: expected {}, got {repo_value}",
            request.repo
        )));
    }

    let digest = first_extension(
        &materials.certificate,
        &[OID_SOURCE_DIGEST_NEW, OID_SOURCE_DIGEST_LEGACY],
    )
    .ok_or_else(|| {
        Error::AttestationRefused("certificate has no source digest extension".into())
    })?;
    if !(digest.len() == 40 || digest.len() == 64) || !digest.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::AttestationRefused(format!(
            "source digest extension is not a hex digest: {digest}"
        )));
    }
    if let Some(expected) = &request.source_digest {
        if !digest.eq_ignore_ascii_case(expected) {
            return Err(Error::AttestationRefused(format!(
                "source digest mismatch: expected {expected}, got {digest}"
            )));
        }
    }
    Ok(())
}

fn source_repo_matches(actual: &str, expected: &str) -> bool {
    matches!(
        actual,
        a if a == expected
            || a == format!("https://github.com/{expected}")
            || a == format!("git+https://github.com/{expected}")
    )
}

fn check_signing_time(materials: &BundleMaterials) -> Result<()> {
    let integrated = materials.tlog.integrated_time;
    if integrated <= 0 {
        return Err(Error::AttestationRefused(
            "transparency log integration time is missing".into(),
        ));
    }
    let validity = materials.certificate.tbs_certificate().validity();
    let not_before = validity.not_before.to_unix_duration().as_secs() as i64;
    let not_after = validity.not_after.to_unix_duration().as_secs() as i64;
    if integrated < not_before || integrated > not_after {
        return Err(Error::AttestationRefused(
            "signing certificate was not valid at the transparency log integration time".into(),
        ));
    }
    Ok(())
}

fn check_tlog(tlog: &TransparencyLogEntry, trust_root: &SigstoreTrustRoot) -> Result<()> {
    let log_id = tlog
        .log_id
        .as_ref()
        .ok_or_else(|| Error::AttestationRefused("transparency log entry has no log id".into()))?;
    let key_id = crate::verify::hex_encode(&log_id.key_id);
    let keys = trust_root
        .rekor_keys()
        .map_err(|e| Error::Attestation(format!("cannot read Rekor keys from trust root: {e}")))?;
    let raw_key = keys.get(&key_id).ok_or_else(|| {
        Error::AttestationRefused(format!("unknown transparency log key id {key_id}"))
    })?;
    let rekor_key = verification_key(raw_key).ok_or_else(|| {
        Error::AttestationRefused(format!("unsupported Rekor public key for log {key_id}"))
    })?;

    let promise = tlog
        .inclusion_promise
        .as_ref()
        .ok_or_else(|| Error::AttestationRefused("bundle has no transparency log SET".into()))?;
    let set_payload = serde_json::json!({
        "body": base64::engine::general_purpose::STANDARD.encode(&tlog.canonicalized_body),
        "integratedTime": tlog.integrated_time,
        "logIndex": tlog.log_index,
        "logID": key_id,
    });
    let canonical = serde_json_canonicalizer::to_vec(&set_payload)
        .map_err(|e| Error::Attestation(format!("cannot canonicalize SET payload: {e}")))?;
    rekor_key
        .verify_signature(Signature::Raw(&promise.signed_entry_timestamp), &canonical)
        .map_err(|e| {
            Error::AttestationRefused(format!("transparency log SET verification failed: {e}"))
        })?;

    let proof = tlog.inclusion_proof.as_ref().ok_or_else(|| {
        Error::AttestationRefused("bundle has no transparency log inclusion proof".into())
    })?;
    let root_hash: [u8; 32] = proof.root_hash.as_slice().try_into().map_err(|_| {
        Error::AttestationRefused(format!(
            "inclusion proof root hash has length {}, expected 32",
            proof.root_hash.len()
        ))
    })?;
    let mut hashes = Vec::with_capacity(proof.hashes.len());
    for (i, hash) in proof.hashes.iter().enumerate() {
        let value: [u8; 32] = hash.as_slice().try_into().map_err(|_| {
            Error::AttestationRefused(format!(
                "inclusion proof hash {i} has length {}, expected 32",
                hash.len()
            ))
        })?;
        hashes.push(value);
    }
    let envelope = proof
        .checkpoint
        .as_ref()
        .map(|c| c.envelope.as_str())
        .ok_or_else(|| {
            Error::AttestationRefused("inclusion proof has no signed checkpoint".into())
        })?;
    let envelope_json = serde_json::to_string(envelope)
        .map_err(|e| Error::Attestation(format!("cannot encode checkpoint: {e}")))?;
    let checkpoint: SignedCheckpoint = serde_json::from_str(&envelope_json).map_err(|e| {
        Error::AttestationRefused(format!("inclusion proof checkpoint is malformed: {e}"))
    })?;
    let tree_size = u64::try_from(proof.tree_size)
        .map_err(|_| Error::AttestationRefused("inclusion proof tree size is negative".into()))?;
    let inclusion = RekorInclusionProof::new(
        proof.log_index,
        root_hash,
        tree_size,
        hashes,
        Some(checkpoint),
    );
    inclusion
        .verify(&tlog.canonicalized_body, &rekor_key)
        .map_err(|e| {
            Error::AttestationRefused(format!("Merkle inclusion proof verification failed: {e}"))
        })?;
    Ok(())
}

fn verification_key(raw: &[u8]) -> Option<CosignVerificationKey> {
    let schemes = [
        SigningScheme::ECDSA_P256_SHA256_ASN1,
        SigningScheme::ECDSA_P384_SHA384_ASN1,
        SigningScheme::ED25519,
        SigningScheme::RSA_PKCS1_SHA256(2048),
        SigningScheme::RSA_PSS_SHA256(2048),
    ];
    schemes
        .into_iter()
        .find_map(|scheme| CosignVerificationKey::from_der(raw, &scheme).ok())
}

fn extension_text(cert: &Certificate, oid: ObjectIdentifier) -> Option<String> {
    let extensions = cert.tbs_certificate().extensions()?;
    let mut values = extensions
        .iter()
        .filter(|ext| ext.extn_id == oid)
        .map(|ext| decode_extension_text(ext.extn_value.as_bytes()));
    match (values.next(), values.next()) {
        (Some(Some(value)), None) => Some(value),
        _ => None,
    }
}

/// Fulcio v1 extensions hold raw UTF-8; the 1.8 and later extensions hold a DER UTF8String
/// (tag 0x0c, length, bytes). A value is taken as DER only when the tag and the length cover the
/// whole content exactly, so raw text can never be mistaken for it.
fn decode_extension_text(bytes: &[u8]) -> Option<String> {
    if let Some((&0x0c, rest)) = bytes.split_first() {
        let (len, body) = match rest.split_first() {
            Some((&first, tail)) if first < 0x80 => (usize::from(first), tail),
            Some((&0x81, tail)) if !tail.is_empty() => (usize::from(tail[0]), &tail[1..]),
            Some((&0x82, tail)) if tail.len() >= 2 => (
                usize::from(u16::from_be_bytes([tail[0], tail[1]])),
                &tail[2..],
            ),
            _ => return std::str::from_utf8(bytes).ok().map(str::to_string),
        };
        if body.len() == len {
            return std::str::from_utf8(body).ok().map(str::to_string);
        }
    }
    std::str::from_utf8(bytes).ok().map(str::to_string)
}

fn first_extension(cert: &Certificate, oids: &[ObjectIdentifier]) -> Option<String> {
    oids.iter().find_map(|oid| extension_text(cert, *oid))
}

fn subject_alt_names(cert: &Certificate) -> Result<Vec<String>> {
    let (_, san): (bool, SubjectAltName) = cert
        .tbs_certificate()
        .get_extension()
        .map_err(|e| Error::AttestationRefused(format!("certificate SAN is malformed: {e}")))?
        .ok_or_else(|| Error::AttestationRefused("certificate has no SAN extension".into()))?;
    let mut names = Vec::new();
    for name in &san.0 {
        if let GeneralName::UniformResourceIdentifier(uri) = name {
            names.push(uri.as_str().to_string());
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TrustRootKind;
    use crate::verify::AttestationRequest;
    use std::path::PathBuf;

    const REAL_DSSE_BUNDLE: &[u8] =
        include_bytes!("../../tests/fixtures/attestation/npm_slsa_bundle_v03.json");
    const REAL_MESSAGE_BUNDLE: &[u8] =
        include_bytes!("../../tests/fixtures/attestation/a.txt.sigstore");

    fn request() -> AttestationRequest {
        AttestationRequest {
            path: PathBuf::from("tests/fixtures/attestation/a.txt"),
            repo: "ddproxy/tuishou".to_string(),
            signer_workflow: ".github/workflows/release.yml".to_string(),
            tag: Some("v0.1.0".to_string()),
            source_digest: None,
            trust_root: TrustRootKind::Embedded,
            cache_dir: PathBuf::from("/tmp/unused"),
            cache_ttl: std::time::Duration::from_secs(3600),
        }
    }

    fn trust_root() -> SigstoreTrustRoot {
        SigstoreTrustRoot::from_trusted_root_json_unchecked(include_bytes!(
            "../../assets/sigstore-trusted-root.json"
        ))
        .unwrap()
    }

    const TUISHOU_V010_BUNDLE: &[u8] =
        include_bytes!("../../tests/fixtures/attestation/tuishou_v0.1.0.bundle.json");
    // SHA-256 of tuishou-v0.1.0-x86_64-unknown-linux-musl.tar.gz, the artifact the bundle attests.
    const TUISHOU_V010_DIGEST: &str =
        "040cce6e7c121e83b475b737459e4d80527964b3ec974f508c690777dcdcafa1";

    fn digest_bytes(hex: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn a_real_release_attestation_verifies() {
        let report = verify_bundle(
            &request(),
            trust_root(),
            TUISHOU_V010_BUNDLE,
            digest_bytes(TUISHOU_V010_DIGEST),
        )
        .unwrap();
        assert_eq!(report.digest, TUISHOU_V010_DIGEST);
        assert_eq!(report.ref_value, "refs/tags/v0.1.0");
    }

    #[test]
    fn a_real_release_attestation_is_refused_when_anything_differs() {
        let good = digest_bytes(TUISHOU_V010_DIGEST);
        let mut other = good;
        other[0] ^= 1;
        let err = verify_bundle(&request(), trust_root(), TUISHOU_V010_BUNDLE, other).unwrap_err();
        assert!(err.to_string().contains("digest"), "{err}");

        let mut wrong_repo = request();
        wrong_repo.repo = "ddproxy/reiki".to_string();
        assert!(verify_bundle(&wrong_repo, trust_root(), TUISHOU_V010_BUNDLE, good).is_err());

        let mut wrong_tag = request();
        wrong_tag.tag = Some("v0.1.1".to_string());
        assert!(verify_bundle(&wrong_tag, trust_root(), TUISHOU_V010_BUNDLE, good).is_err());

        let mut wrong_workflow = request();
        wrong_workflow.signer_workflow = ".github/workflows/ci.yml".to_string();
        assert!(verify_bundle(&wrong_workflow, trust_root(), TUISHOU_V010_BUNDLE, good).is_err());
    }

    #[test]
    fn extension_text_decodes_der_strings_and_raw_text() {
        assert_eq!(
            decode_extension_text(b"\x0c\x03abc").as_deref(),
            Some("abc")
        );
        assert_eq!(decode_extension_text(b"abc").as_deref(), Some("abc"));
        // A length that does not cover the content is raw text, never silently truncated.
        assert_eq!(
            decode_extension_text(b"\x0c\x05abc"),
            std::str::from_utf8(b"\x0c\x05abc").ok().map(str::to_string)
        );
        let mut long = vec![0x0c, 0x81, 130];
        long.extend(std::iter::repeat_n(b'x', 130));
        assert_eq!(
            decode_extension_text(&long).as_deref(),
            Some("x".repeat(130).as_str())
        );
    }

    #[test]
    fn malformed_bundle_is_refused() {
        let err = verify_bundle(&request(), trust_root(), b"not json", [0; 32]).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "{err}");
    }

    #[test]
    fn real_signed_message_bundle_has_no_dsse_envelope() {
        let bundle: Bundle = serde_json::from_slice(REAL_MESSAGE_BUNDLE).unwrap();
        let err = parse_materials(&bundle).unwrap_err();
        assert!(err.to_string().contains("DSSE envelope"), "{err}");
    }

    #[test]
    fn real_dsse_bundle_is_refused_for_wrong_workflow_identity() {
        let err = verify_bundle(&request(), trust_root(), REAL_DSSE_BUNDLE, [0; 32]).unwrap_err();
        assert!(
            err.to_string().contains("workflow identity mismatch")
                || err.to_string().contains("no SAN workflow URI"),
            "{err}"
        );
    }

    #[test]
    fn unsupported_payload_and_predicate_are_refused() {
        let mut value: Value = serde_json::from_slice(REAL_DSSE_BUNDLE).unwrap();
        value["dsseEnvelope"]["payloadType"] = Value::from("text/plain");
        let bad_payload_type = serde_json::to_vec(&value).unwrap();
        let bundle: Bundle = serde_json::from_slice(&bad_payload_type).unwrap();
        let err = parse_materials(&bundle).unwrap_err();
        assert!(
            err.to_string().contains("unsupported DSSE payload type"),
            "{err}"
        );

        let mut value: Value = serde_json::from_slice(REAL_DSSE_BUNDLE).unwrap();
        let payload = value["dsseEnvelope"]["payload"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        let mut statement: Value = serde_json::from_slice(&decoded).unwrap();
        statement["predicateType"] = Value::from("https://example.invalid/unsupported");
        value["dsseEnvelope"]["payload"] = Value::from(
            base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(&statement).unwrap()),
        );
        let bad_predicate = serde_json::to_vec(&value).unwrap();
        let bundle: Bundle = serde_json::from_slice(&bad_predicate).unwrap();
        let err = parse_materials(&bundle).unwrap_err();
        assert!(
            err.to_string().contains("unsupported provenance predicate"),
            "{err}"
        );
    }

    #[test]
    fn bundle_without_verification_material_is_refused() {
        let mut value: Value = serde_json::from_slice(REAL_DSSE_BUNDLE).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("verificationMaterial");
        let bytes = serde_json::to_vec(&value).unwrap();
        let bundle: Bundle = serde_json::from_slice(&bytes).unwrap();
        let err = parse_materials(&bundle).unwrap_err();
        assert!(
            err.to_string().contains("no verification material"),
            "{err}"
        );
    }
}
