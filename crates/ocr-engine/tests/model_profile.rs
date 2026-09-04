use ed25519_dalek::{Signer, SigningKey};
use ocr_engine::{
    ArtifactLimits, Error, ModelProfileVerifier, TensorDataType, TensorDimension, TrustedModelKey,
};
use serde_json::json;
use sha2::{Digest, Sha256};

const KEY_ID: &str = "release-key-2026";

#[test]
fn signed_profile_binds_a_bounded_artifact_and_tensor_contract() {
    let artifact = b"approved-model-artifact";
    let signing_key = SigningKey::from_bytes(&[7; 32]);
    let manifest = signed_manifest(artifact);
    let signature = signing_key.sign(&manifest).to_bytes();
    let verifier = verifier(&signing_key);

    let profile = verifier.verify(KEY_ID, &manifest, &signature).unwrap();

    assert_eq!(profile.profile_id(), "printed-en-v1");
    assert_eq!(profile.runtime_contract_version(), "1.0.0");
    assert_eq!(profile.supported_locales(), ["en"]);
    assert_eq!(profile.inputs().len(), 1);
    assert_eq!(profile.outputs().len(), 1);
    assert_eq!(profile.inputs()[0].name(), "image");
    assert_eq!(profile.inputs()[0].data_type(), TensorDataType::Float32);
    assert_eq!(
        profile.inputs()[0].dimensions(),
        [
            TensorDimension::Fixed { value: 1 },
            TensorDimension::Fixed { value: 3 },
            TensorDimension::Dynamic { maximum: 2048 },
            TensorDimension::Dynamic { maximum: 2048 },
        ]
    );
    profile
        .verify_artifact(artifact, ArtifactLimits::new(1024).unwrap())
        .unwrap();
}

#[test]
fn verifier_rejects_a_signature_over_different_manifest_bytes() {
    let signing_key = SigningKey::from_bytes(&[8; 32]);
    let artifact = b"approved-model-artifact";
    let manifest = signed_manifest(artifact);
    let signature = signing_key.sign(b"different-manifest").to_bytes();

    assert_eq!(
        verifier(&signing_key).verify(KEY_ID, &manifest, &signature),
        Err(Error::InvalidModelSignature)
    );
}

#[test]
fn verifier_rejects_unknown_or_duplicate_trusted_signing_keys() {
    let signing_key = SigningKey::from_bytes(&[11; 32]);
    let artifact = b"approved-model-artifact";
    let manifest = signed_manifest(artifact);
    let signature = signing_key.sign(&manifest).to_bytes();
    let public_key = signing_key.verifying_key().to_bytes();

    assert_eq!(
        verifier(&signing_key).verify("retired-key-2026", &manifest, &signature),
        Err(Error::UnknownModelSigningKey)
    );
    assert_eq!(
        ModelProfileVerifier::new(vec![
            TrustedModelKey::new(KEY_ID, public_key).unwrap(),
            TrustedModelKey::new(KEY_ID, public_key).unwrap(),
        ]),
        Err(Error::InvalidTrustedModelKey)
    );
}

#[test]
fn verifier_rejects_an_oversized_manifest_before_signature_or_json_processing() {
    let signing_key = SigningKey::from_bytes(&[12; 32]);
    let manifest = vec![b' '; 64 * 1024 + 1];

    assert_eq!(
        verifier(&signing_key).verify(KEY_ID, &manifest, &[0; 64]),
        Err(Error::ModelProfileByteLimitExceeded {
            bytes: manifest.len(),
            limit: 64 * 1024,
        })
    );
}

#[test]
fn verifier_rejects_unbounded_tensor_shapes_and_unsupported_fields() {
    let signing_key = SigningKey::from_bytes(&[9; 32]);
    let artifact = b"approved-model-artifact";
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&signed_manifest(artifact)).unwrap();
    manifest["inputs"][0]["dimensions"][2] = json!({ "kind": "dynamic", "maximum": 0 });
    manifest["artifact_uri"] = json!("https://attacker.invalid/model.onnx");
    let manifest = serde_json::to_vec(&manifest).unwrap();
    let signature = signing_key.sign(&manifest).to_bytes();

    assert_eq!(
        verifier(&signing_key).verify(KEY_ID, &manifest, &signature),
        Err(Error::InvalidModelProfile)
    );
}

#[test]
fn verifier_rejects_tensor_element_envelopes_that_exceed_runtime_bounds() {
    let signing_key = SigningKey::from_bytes(&[13; 32]);
    let artifact = b"approved-model-artifact";
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&signed_manifest(artifact)).unwrap();
    manifest["inputs"][0]["dimensions"] = json!([
        { "kind": "fixed", "value": 65536 },
        { "kind": "fixed", "value": 65536 },
        { "kind": "fixed", "value": 65536 },
        { "kind": "fixed", "value": 65536 },
        { "kind": "fixed", "value": 65536 },
        { "kind": "fixed", "value": 65536 },
        { "kind": "fixed", "value": 65536 },
        { "kind": "fixed", "value": 65536 }
    ]);
    let manifest = serde_json::to_vec(&manifest).unwrap();
    let signature = signing_key.sign(&manifest).to_bytes();

    assert_eq!(
        verifier(&signing_key).verify(KEY_ID, &manifest, &signature),
        Err(Error::InvalidModelProfile)
    );
}

#[test]
fn artifact_must_match_the_signed_digest_and_stay_within_its_limit() {
    let signing_key = SigningKey::from_bytes(&[10; 32]);
    let artifact = b"approved-model-artifact";
    let manifest = signed_manifest(artifact);
    let signature = signing_key.sign(&manifest).to_bytes();
    let profile = verifier(&signing_key)
        .verify(KEY_ID, &manifest, &signature)
        .unwrap();

    assert_eq!(
        profile.verify_artifact(b"substituted", ArtifactLimits::new(1024).unwrap()),
        Err(Error::ModelArtifactDigestMismatch)
    );
    assert_eq!(
        profile.verify_artifact(artifact, ArtifactLimits::new(4).unwrap()),
        Err(Error::ModelArtifactByteLimitExceeded {
            bytes: artifact.len(),
            limit: 4,
        })
    );
}

fn verifier(signing_key: &SigningKey) -> ModelProfileVerifier {
    ModelProfileVerifier::new(vec![TrustedModelKey::new(
        KEY_ID,
        signing_key.verifying_key().to_bytes(),
    )
    .unwrap()])
    .unwrap()
}

fn signed_manifest(artifact: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema_version": 1,
        "profile_id": "printed-en-v1",
        "stage": "recognizer",
        "model_version": "1.0.0",
        "artifact_digest": format!("sha256:{:x}", Sha256::digest(artifact)),
        "license_id": "Apache-2.0",
        "dataset_version": "golden-printed-en-v1",
        "calibration_version": "printed-en-calibration-v1",
        "execution_profile": "cpu",
        "runtime_contract_version": "1.0.0",
        "supported_locales": ["en"],
        "rollback_predecessor": null,
        "inputs": [{
            "name": "image",
            "data_type": "float32",
            "dimensions": [
                { "kind": "fixed", "value": 1 },
                { "kind": "fixed", "value": 3 },
                { "kind": "dynamic", "maximum": 2048 },
                { "kind": "dynamic", "maximum": 2048 }
            ]
        }],
        "outputs": [{
            "name": "tokens",
            "data_type": "float32",
            "dimensions": [
                { "kind": "fixed", "value": 1 },
                { "kind": "dynamic", "maximum": 4096 }
            ]
        }]
    }))
    .unwrap()
}
