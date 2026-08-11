//! Signed plugin registry — a marketplace foundation for first-class plugins.
//!
//! The sandbox already guarantees a plugin *can't* do harm at runtime (computation-only,
//! fuel/memory capped, hot-swappable). The registry adds the missing supply-chain guarantee:
//! a plugin is published **signed** by a known publisher, and the host verifies the signature
//! against a registered publisher key before it is ever loaded. This is exactly the trust model
//! a plugin marketplace needs — operators curate a set of publisher keys, and only code those
//! publishers signed can enter the runtime.
//!
//! Signing uses Ed25519 (via `ed25519-dalek`); the signed message is the raw component bytes, so
//! tampering with the wasm after signing is detected. Built on the existing sandbox + hot-swap
//! machinery, a registry turns "anyone can ship a plugin" into "only vetted, signed plugins run".

use std::collections::HashMap;

use ed25519_dalek::{Signer, SigningKey, Signature, Verifier, VerifyingKey};
use rand::rngs::OsRng;

/// Errors raised by the registry.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    /// The plugin's claimed publisher is not a registered, trusted key.
    #[error("unknown publisher: {0}")]
    UnknownPublisher(String),
    /// The plugin's signature does not match its bytes under the publisher's key.
    #[error("invalid signature for plugin {0}")]
    InvalidSignature(String),
    /// A plugin with this name is already published.
    #[error("plugin {0} already published")]
    AlreadyExists(String),
}

/// A trusted plugin publisher, identified by its Ed25519 verifying key.
#[derive(Debug, Clone)]
pub struct PluginPublisher {
    /// Human-readable publisher name (e.g. "acme-logistics").
    pub name: String,
    /// The public key whose signatures the registry will accept.
    pub verifying_key: VerifyingKey,
}

impl PluginPublisher {
    /// Register a publisher under `name` with the given verifying key.
    pub fn new(name: impl Into<String>, verifying_key: VerifyingKey) -> Self {
        Self {
            name: name.into(),
            verifying_key,
        }
    }
}

/// A plugin component together with its publisher attribution and Ed25519 signature.
#[derive(Debug, Clone)]
pub struct SignedPlugin {
    /// Operator-facing plugin name (unique within the registry).
    pub name: String,
    /// The name of the trusted publisher that signed this plugin.
    pub publisher_name: String,
    /// The raw, compiled component bytes.
    pub wasm: Vec<u8>,
    /// The publisher's public key (redundant with the registry's known publishers, but
    /// carried so a plugin is self-describing and verifiable in isolation).
    pub publisher: VerifyingKey,
    /// Ed25519 signature over `wasm` under the publisher's signing key.
    pub signature: Signature,
}

impl SignedPlugin {
    /// Sign `wasm` with `key` on behalf of `publisher_name`, producing a self-describing
    /// [`SignedPlugin`].
    pub fn sign(
        name: impl Into<String>,
        publisher_name: impl Into<String>,
        wasm: Vec<u8>,
        key: &SigningKey,
    ) -> Self {
        let signature = key.sign(&wasm);
        Self {
            name: name.into(),
            publisher_name: publisher_name.into(),
            wasm,
            publisher: key.verifying_key(),
            signature,
        }
    }

    /// Verify this plugin's signature against its own publisher key.
    pub fn verify(&self) -> bool {
        self.publisher.verify(&self.wasm, &self.signature).is_ok()
    }
}

/// The signed-plugin registry / marketplace.
///
/// Operators register trusted publisher keys, then publish plugins whose signatures are
/// verified against those keys. Fetching a plugin returns the verified artifact; the registry
/// never stores an unverifiable or untrusted plugin.
#[derive(Debug, Clone, Default)]
pub struct PluginRegistry {
    publishers: HashMap<String, VerifyingKey>,
    plugins: HashMap<String, SignedPlugin>,
}

impl PluginRegistry {
    /// An empty registry (no trusted publishers yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// Trust a publisher's verifying key.
    pub fn register_publisher(&mut self, publisher: PluginPublisher) {
        self.publishers.insert(publisher.name, publisher.verifying_key);
    }

    /// Publish a signed plugin, verifying (a) its publisher is trusted and (b) its signature
    /// is valid. On success the plugin is stored and available via [`fetch`].
    ///
    /// [`fetch`]: PluginRegistry::fetch
    pub fn publish(&mut self, plugin: SignedPlugin) -> Result<(), RegistryError> {
        // The plugin must name a publisher the registry trusts.
        let trusted = self
            .publishers
            .get(&plugin.publisher_name)
            .copied()
            .ok_or_else(|| RegistryError::UnknownPublisher(plugin.publisher_name.clone()))?;

        // Reject if the embedded publisher key is not the trusted one, or the signature
        // does not verify under it. Both cases mean the artifact is not authentic, so we
        // report an invalid signature rather than revealing the key mismatch.
        if plugin.publisher != trusted {
            return Err(RegistryError::InvalidSignature(plugin.name.clone()));
        }
        trusted
            .verify(&plugin.wasm, &plugin.signature)
            .map_err(|_| RegistryError::InvalidSignature(plugin.name.clone()))?;

        if self.plugins.contains_key(&plugin.name) {
            return Err(RegistryError::AlreadyExists(plugin.name.clone()));
        }
        self.plugins.insert(plugin.name.clone(), plugin);
        Ok(())
    }

    /// Fetch a published (and previously verified) plugin by name.
    pub fn fetch(&self, name: &str) -> Option<&SignedPlugin> {
        self.plugins.get(name)
    }

    /// The set of trusted publisher names.
    pub fn publishers(&self) -> Vec<&str> {
        self.publishers.keys().map(|s| s.as_str()).collect()
    }

    /// Re-verify every stored plugin's signature (e.g. a periodic integrity sweep).
    pub fn verify_all(&self) -> bool {
        self.plugins.values().all(|p| p.verify())
    }
}

/// Generate a fresh Ed25519 signing key (used by publishers to bootstrap their identity).
pub fn generate_signing_key() -> SigningKey {
    SigningKey::generate(&mut OsRng)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let key = generate_signing_key();
        let wasm = b"\0asm\x01\x00\x00\x00demo".to_vec();
        let plugin = SignedPlugin::sign("pricing", "acme", wasm.clone(), &key);
        assert!(plugin.verify());
        assert_eq!(plugin.wasm, wasm);
        assert_eq!(plugin.publisher, key.verifying_key());
    }

    #[test]
    fn tampered_bytes_fail_verification() {
        let key = generate_signing_key();
        let mut plugin = SignedPlugin::sign("pricing", "acme", vec![1, 2, 3, 4], &key);
        plugin.wasm.push(0xFF); // mutate after signing
        assert!(!plugin.verify());
    }

    #[test]
    fn registry_accepts_trusted_signed_plugin() {
        let key = generate_signing_key();
        let mut reg = PluginRegistry::new();
        reg.register_publisher(PluginPublisher::new("acme", key.verifying_key()));

        let plugin = SignedPlugin::sign("acme/pricing", "acme", vec![9, 9, 9], &key);
        reg.publish(plugin).expect("trusted + valid signature");

        let fetched = reg.fetch("acme/pricing").expect("published");
        assert!(fetched.verify());
        assert!(reg.verify_all());
    }

    #[test]
    fn registry_rejects_unknown_publisher() {
        let key = generate_signing_key();
        let mut reg = PluginRegistry::new(); // no publishers registered
        let plugin = SignedPlugin::sign("ghost", "ghost", vec![1, 2, 3], &key);
        assert!(matches!(
            reg.publish(plugin),
            Err(RegistryError::UnknownPublisher(_))
        ));
    }

    #[test]
    fn registry_rejects_invalid_signature() {
        let good = generate_signing_key();
        let evil = generate_signing_key(); // different key
        let mut reg = PluginRegistry::new();
        reg.register_publisher(PluginPublisher::new("acme", good.verifying_key()));

        // Signed by `evil` but claims the `acme` name (whose trusted key is `good`).
        let plugin = SignedPlugin::sign("acme", "acme", vec![1, 2, 3], &evil);
        assert!(matches!(
            reg.publish(plugin),
            Err(RegistryError::InvalidSignature(_))
        ));
    }

    #[test]
    fn registry_rejects_duplicate_name() {
        let key = generate_signing_key();
        let mut reg = PluginRegistry::new();
        reg.register_publisher(PluginPublisher::new("acme", key.verifying_key()));
        let p1 = SignedPlugin::sign("dup", "acme", vec![1], &key);
        let p2 = SignedPlugin::sign("dup", "acme", vec![2], &key);
        reg.publish(p1).unwrap();
        assert!(matches!(
            reg.publish(p2),
            Err(RegistryError::AlreadyExists(_))
        ));
    }
}
