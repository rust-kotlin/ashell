use std::sync::Arc;

use anyhow::Result;
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

pub fn private_keys_with_algs(keypair: PrivateKey) -> Vec<PrivateKeyWithHashAlg> {
    let mut algs = Vec::new();
    let key_arc = Arc::new(keypair);

    if key_arc.algorithm().is_rsa() {
        algs.push(PrivateKeyWithHashAlg::new(
            key_arc.clone(),
            Some(HashAlg::Sha512),
        ));
        algs.push(PrivateKeyWithHashAlg::new(
            key_arc.clone(),
            Some(HashAlg::Sha256),
        ));
        algs.push(PrivateKeyWithHashAlg::new(key_arc, None));
    } else {
        algs.push(PrivateKeyWithHashAlg::new(key_arc, None));
    }

    algs
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
                for key in private_keys_with_algs(keypair) {
                    match handle.authenticate_publickey(user, key).await {
                        Ok(result) if result.success() => return Ok(true),
                        Ok(_) | Err(_) => continue,
                    }
                }
            }
            Err(_) => continue,
        }
    }

    Ok(false)
}
