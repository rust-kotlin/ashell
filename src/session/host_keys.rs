use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result, anyhow};
use russh::keys::{HashAlg, PublicKey, PublicKeyOrCertificate};
use serde::{Deserialize, Serialize};

use crate::terminal::BackendEvent;

static TRUST_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostIdentity {
    public_key: String,
    #[serde(default)]
    authority_key: Option<String>,
}

impl HostIdentity {
    fn from_server(host: &str, key: &PublicKeyOrCertificate) -> Result<Self> {
        let expected_host = canonical_host(host)?;
        let public_key = key.public_key().to_openssh()?;
        let authority_key = if let Some(certificate) = key.certificate() {
            if certificate.cert_type() != russh::keys::ssh_key::certificate::CertType::Host
                || !certificate.critical_options().is_empty()
                || (!certificate.valid_principals().is_empty()
                    && !certificate.valid_principals().iter().any(|principal| {
                        canonical_host(principal).is_ok_and(|principal| principal == expected_host)
                    }))
            {
                return Err(anyhow!("SSH host certificate does not authorize this host"));
            }
            let authority = PublicKey::new(certificate.signature_key().clone(), "");
            let fingerprint = authority.fingerprint(HashAlg::Sha256);
            // The exact host and issuer keys are pinned below. Validation here
            // also checks certificate signature and expiry; it does not grant
            // this issuer trust for any other endpoint.
            certificate
                .validate([&fingerprint])
                .context("invalid SSH host certificate")?;
            Some(authority.to_openssh()?)
        } else {
            None
        };
        Ok(Self {
            public_key,
            authority_key,
        })
    }

    pub(crate) fn fingerprint(&self) -> Result<String> {
        Ok(PublicKey::from_openssh(&self.public_key)?
            .fingerprint(HashAlg::Sha256)
            .to_string())
    }

    pub(crate) fn authority_fingerprint(&self) -> Result<Option<String>> {
        self.authority_key
            .as_deref()
            .map(|key| {
                Ok(PublicKey::from_openssh(key)?
                    .fingerprint(HashAlg::Sha256)
                    .to_string())
            })
            .transpose()
    }

    fn validate(&self) -> Result<()> {
        self.fingerprint()?;
        self.authority_fingerprint()?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct HostKeyRequest {
    pub(crate) attempt: Option<crate::terminal::BackendAttempt>,
    pub(crate) sftp_attempt: Option<crate::terminal::BackendAttempt>,
    pub(crate) tab_id: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) identity: HostIdentity,
    pub(crate) previous: Option<HostIdentity>,
}

impl HostKeyRequest {
    /// An SFTP prompt must still belong to both its terminal attempt and its
    /// own file connection, which can be replaced without reconnecting SSH.
    pub(crate) fn is_current(&self) -> bool {
        self.attempt
            .as_ref()
            .is_some_and(|attempt| attempt.is_current())
            && self
                .sftp_attempt
                .as_ref()
                .is_none_or(|attempt| attempt.is_current())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustedHosts {
    version: u32,
    hosts: BTreeMap<String, HostIdentity>,
}

fn trust_path() -> Result<PathBuf> {
    let home = directories::BaseDirs::new().context("cannot locate host trust store")?;
    Ok(home.home_dir().join(".config/ashell/trusted-hosts.json"))
}

fn canonical_host(host: &str) -> Result<String> {
    if host.is_empty() || host.trim() != host || host.chars().any(char::is_control) {
        return Err(anyhow!("invalid SSH host identity"));
    }
    if let Some(address) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        return address
            .parse::<std::net::Ipv6Addr>()
            .map(|address| address.to_string())
            .context("invalid bracketed SSH host address");
    }
    if let Ok(address) = host.parse::<std::net::IpAddr>() {
        return Ok(address.to_string());
    }
    let host = host.trim_end_matches('.').to_lowercase();
    if host.is_empty() || host.contains(['[', ']']) {
        return Err(anyhow!("invalid SSH hostname"));
    }
    Ok(host)
}

fn endpoint(host: &str, port: u16) -> Result<String> {
    Ok(format!("[{}]:{port}", canonical_host(host)?))
}

fn read_store(path: &Path) -> Result<TrustedHosts> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TrustedHosts {
                version: 1,
                hosts: BTreeMap::new(),
            });
        }
        Err(error) => return Err(error).context("read SSH host trust store"),
    };
    let store: TrustedHosts =
        serde_json::from_slice(&bytes).context("invalid SSH host trust store")?;
    if store.version != 1 {
        return Err(anyhow!("unsupported SSH host trust store version"));
    }
    for identity in store.hosts.values() {
        identity.validate()?;
    }
    Ok(store)
}

pub(crate) fn is_trusted(request: &HostKeyRequest) -> Result<bool> {
    let _guard = TRUST_LOCK
        .lock()
        .map_err(|_| anyhow!("SSH host trust lock poisoned"))?;
    let store = read_store(&trust_path()?)?;
    Ok(store.hosts.get(&endpoint(&request.host, request.port)?) == Some(&request.identity))
}

/// Authentication never starts until the user has approved this exact endpoint/key.
pub(crate) fn verify(
    tab_id: &str,
    host: &str,
    port: u16,
    key: &PublicKeyOrCertificate,
    emit: impl FnOnce(BackendEvent),
) -> Result<bool> {
    verify_at(tab_id, host, port, key, emit, &trust_path()?)
}

fn verify_at(
    tab_id: &str,
    host: &str,
    port: u16,
    key: &PublicKeyOrCertificate,
    emit: impl FnOnce(BackendEvent),
    path: &Path,
) -> Result<bool> {
    let identity = HostIdentity::from_server(host, key)?;
    let _guard = TRUST_LOCK
        .lock()
        .map_err(|_| anyhow!("SSH host trust lock poisoned"))?;
    let store = read_store(path)?;
    let previous = store.hosts.get(&endpoint(host, port)?).cloned();
    if previous.as_ref() == Some(&identity) {
        return Ok(true);
    }
    emit(BackendEvent::HostKeyVerification(HostKeyRequest {
        attempt: None,
        sftp_attempt: None,
        tab_id: tab_id.to_string(),
        host: host.to_string(),
        port,
        identity,
        previous,
    }));
    Err(anyhow!("{}", rust_i18n::t!("ssh_verify_host_required")))
}

/// Compare against the key shown in the dialog before saving; a stale approval
/// cannot overwrite a different decision from another pending connection.
pub(crate) fn approve(request: &HostKeyRequest) -> Result<()> {
    approve_at(request, &trust_path()?)
}

fn approve_at(request: &HostKeyRequest, path: &Path) -> Result<()> {
    let _guard = TRUST_LOCK
        .lock()
        .map_err(|_| anyhow!("SSH host trust lock poisoned"))?;
    request.identity.validate()?;
    let mut store = read_store(path)?;
    let endpoint = endpoint(&request.host, request.port)?;
    let current = store.hosts.get(&endpoint);
    if current == Some(&request.identity) {
        return Ok(());
    }
    if current != request.previous.as_ref() {
        return Err(anyhow!(
            "SSH host trust changed while confirmation was open; reconnect to verify again"
        ));
    }
    store.hosts.insert(endpoint, request.identity.clone());
    let parent = path.parent().context("host trust path has no parent")?;
    fs::create_dir_all(parent).context("create SSH host trust directory")?;
    super::config::persist_config_bytes(path, &serde_json::to_vec_pretty(&store)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_normalization_preserves_the_port_and_ipv6_identity() {
        assert_eq!(
            endpoint("SERVER.TEST.", 22).unwrap(),
            endpoint("server.test", 22).unwrap()
        );
        assert_eq!(
            endpoint("[2001:db8::1]", 2222).unwrap(),
            endpoint("2001:0db8::1", 2222).unwrap()
        );
        assert_ne!(
            endpoint("server.test", 22).unwrap(),
            endpoint("server.test", 2222).unwrap()
        );
        assert!(endpoint("server.test\nother.test", 22).is_err());
        assert!(endpoint(".", 22).is_err());
    }

    fn public_key(seed: u8, rsa: bool) -> PublicKeyOrCertificate {
        let mut bytes = Vec::new();
        let mut string = |data: &[u8]| {
            bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
            bytes.extend_from_slice(data);
        };
        if rsa {
            string(b"ssh-rsa");
            string(&[1, 0, 1]);
            let mut modulus = vec![seed | 1; 129];
            modulus[0] = 0;
            modulus[1] = 0x80;
            string(&modulus);
        } else {
            string(b"ssh-ed25519");
            string(&[seed; 32]);
        }
        PublicKey::from_bytes(&bytes).unwrap().into()
    }

    fn request(key: &PublicKeyOrCertificate, previous: Option<HostIdentity>) -> HostKeyRequest {
        HostKeyRequest {
            attempt: None,
            sftp_attempt: None,
            tab_id: "tab".into(),
            host: "server.test".into(),
            port: 2222,
            identity: HostIdentity::from_server("server.test", key).unwrap(),
            previous,
        }
    }

    #[test]
    fn sftp_verification_expires_even_when_its_terminal_attempt_is_unchanged() {
        use crate::terminal::GuardedBackendEventSender;
        let (sender, _receiver) = std::sync::mpsc::channel();
        let terminal = GuardedBackendEventSender::new(sender.clone());
        let sftp = GuardedBackendEventSender::new(sender);
        let mut request = request(&public_key(1, false), None);
        request.attempt = Some(terminal.attempt());
        request.sftp_attempt = Some(sftp.attempt());
        assert!(request.is_current());
        sftp.invalidate();
        assert!(terminal.attempt().is_current());
        assert!(!request.is_current());

        request.sftp_attempt = None;
        assert!(request.is_current());
        terminal.invalidate();
        assert!(!request.is_current());
    }

    #[test]
    fn first_connection_requires_approval_and_a_different_algorithm_is_a_key_change() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hosts.json");
        let key = public_key(1, false);
        let mut prompt = None;
        assert!(
            verify_at(
                "tab",
                "server.test",
                2222,
                &key,
                |event| {
                    if let BackendEvent::HostKeyVerification(request) = event {
                        prompt = Some(request);
                    }
                },
                &path
            )
            .is_err()
        );
        let prompt = prompt.unwrap();
        assert!(prompt.previous.is_none());
        approve_at(&prompt, &path).unwrap();
        assert!(
            verify_at(
                "tab",
                "SERVER.TEST.",
                2222,
                &key,
                |_| panic!("known host must not prompt"),
                &path
            )
            .unwrap()
        );

        let mut change = None;
        let other = public_key(3, true);
        assert!(
            verify_at(
                "tab",
                "server.test",
                2222,
                &other,
                |event| {
                    if let BackendEvent::HostKeyVerification(request) = event {
                        change = Some(request);
                    }
                },
                &path
            )
            .is_err()
        );
        assert_eq!(change.unwrap().previous, Some(prompt.identity));
    }

    #[test]
    fn competing_first_approvals_cannot_silently_replace_each_other() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hosts.json");
        let first = request(&public_key(1, false), None);
        let second = request(&public_key(2, false), None);
        approve_at(&first, &path).unwrap();
        approve_at(&first, &path).unwrap();
        assert!(approve_at(&second, &path).is_err());
        assert_eq!(
            read_store(&path).unwrap().hosts["[server.test]:2222"],
            first.identity
        );
        let reviewed_change = HostKeyRequest {
            previous: Some(first.identity),
            ..second
        };
        approve_at(&reviewed_change, &path).unwrap();
    }

    #[test]
    fn corrupt_or_unreadable_stores_do_not_fall_back_to_first_use_trust() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hosts.json");
        fs::write(&path, b"broken trust data").unwrap();
        let key = public_key(1, false);
        let directory_path = directory.path().to_path_buf();
        for path in [&path, &directory_path] {
            assert!(
                verify_at(
                    "tab",
                    "server.test",
                    2222,
                    &key,
                    |_| panic!("invalid store must not prompt for replacement"),
                    path
                )
                .is_err()
            );
        }
        assert_eq!(fs::read(&path).unwrap(), b"broken trust data");
    }
}
