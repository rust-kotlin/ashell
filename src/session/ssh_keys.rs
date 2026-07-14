use std::sync::Arc;

use anyhow::{Result, anyhow};
use directories::BaseDirs;
use russh::{
    client::{self, Handler},
    keys::{HashAlg, PrivateKey, key::PrivateKeyWithHashAlg, load_secret_key},
};

use crate::session::config::Session;

pub const DEFAULT_KEY_NAMES: &[&str] = &["id_ed25519", "id_rsa", "id_ecdsa", "id_dsa"];

pub fn session_has_explicit_key(session: &Session) -> bool {
    !session.private_key_path.trim().is_empty()
        || !normalize_inline_private_key(&session.private_key_inline).is_empty()
}

pub fn normalize_inline_private_key(value: &str) -> String {
    let mut normalized = value
        .trim()
        .replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\r\n", "\n");
    if normalized.is_empty() {
        return String::new();
    }
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    normalized
}

/// Decode a private key from inline content, optionally with a passphrase.
/// Returns the parsed `PrivateKey` on success.
pub fn decode_key(content: &str, passphrase: Option<&str>) -> Result<PrivateKey> {
    let normalized = normalize_inline_private_key(content);
    Ok(russh::keys::decode_secret_key(&normalized, passphrase)?)
}

/// Detect the key type string from parsed key content.
/// Returns one of: "ed25519", "rsa", "ecdsa", "dsa", or "unknown".
pub fn detect_key_type(key: &PrivateKey) -> String {
    let alg = key.algorithm();
    let s = alg.as_str();
    if s.contains("ed25519") {
        "ed25519".to_string()
    } else if s.contains("rsa") {
        "rsa".to_string()
    } else if s.contains("ecdsa") {
        "ecdsa".to_string()
    } else if s.contains("dsa") {
        "dsa".to_string()
    } else {
        "unknown".to_string()
    }
}

/// Compute the SHA256 fingerprint string (e.g. "SHA256:xxxx...") for a key.
pub fn compute_fingerprint(key: &PrivateKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Validate, detect type, and compute fingerprint for raw key content.
/// Returns `(key_type, fingerprint)` on success.
pub fn validate_and_inspect(content: &str, passphrase: Option<&str>) -> Result<(String, String)> {
    let key = decode_key(content, passphrase)?;
    Ok((detect_key_type(&key), compute_fingerprint(&key)))
}

pub fn private_keys_with_algs(keypair: PrivateKey) -> Result<Vec<PrivateKeyWithHashAlg>> {
    let mut algs = Vec::new();
    let key_arc = Arc::new(keypair);

    if key_arc.algorithm().is_rsa() {
        if let Ok(k) = PrivateKeyWithHashAlg::new(key_arc.clone(), Some(HashAlg::Sha512)) {
            algs.push(k);
        }
        if let Ok(k) = PrivateKeyWithHashAlg::new(key_arc.clone(), Some(HashAlg::Sha256)) {
            algs.push(k);
        }
        if let Ok(k) = PrivateKeyWithHashAlg::new(key_arc.clone(), None) {
            algs.push(k);
        }
    } else if let Ok(k) = PrivateKeyWithHashAlg::new(key_arc.clone(), None) {
        algs.push(k);
    }

    if algs.is_empty() {
        return Err(anyhow!(
            "Failed to construct PrivateKeyWithHashAlg for any supported hash algorithm"
        ));
    }

    Ok(algs)
}

pub async fn authenticate_with_default_keys<H>(
    handle: &mut client::Handle<H>,
    user: &str,
    passphrase: Option<&str>,
) -> Result<bool>
where
    H: Handler + Send + Sync,
    H::Error: Into<anyhow::Error>,
{
    let Some(ssh_dir) = BaseDirs::new().map(|d| d.home_dir().join(".ssh")) else {
        return Ok(false);
    };

    for key_name in DEFAULT_KEY_NAMES {
        let key_path = ssh_dir.join(key_name);
        if !key_path.exists() {
            continue;
        }
        tracing::debug!("[ssh] trying default key {}", key_path.display());
        match load_secret_key(&key_path, passphrase) {
            Ok(keypair) => {
                if let Ok(keys) = private_keys_with_algs(keypair) {
                    for key in keys {
                        match handle.authenticate_publickey(user, key).await {
                            Ok(true) => return Ok(true),
                            Ok(false) | Err(_) => continue,
                        }
                    }
                }
            }
            Err(_) => continue,
        }
    }

    Ok(false)
}
