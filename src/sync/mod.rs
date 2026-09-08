use anyhow::{Context, Result, anyhow};
use argon2::Argon2;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use reqwest::{Client, StatusCode, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::session::config::Session;

const SYNC_FILE_NAME: &str = "ashell-sync.json";
const FORMAT_VERSION: u32 = 1;

fn sync_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("create configuration sync client")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPayload {
    pub schema_version: u32,
    pub revision: String,
    pub updated_at: String,
    pub device_id: String,
    pub sessions: Vec<Session>,
}

impl SyncPayload {
    pub fn new(device_id: String, sessions: Vec<Session>) -> Self {
        Self {
            schema_version: FORMAT_VERSION,
            revision: Uuid::new_v4().to_string(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            device_id,
            sessions,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedEnvelope {
    format_version: u32,
    kdf: String,
    cipher: String,
    salt: String,
    nonce: String,
    payload: String,
}

#[derive(Clone)]
pub struct SyncCredentials {
    pub backend: SyncBackendCredentials,
    pub encryption_password: String,
}

#[derive(Clone)]
pub enum SyncBackendCredentials {
    WebDav {
        endpoint: String,
        username: String,
        password: String,
    },
    S3 {
        endpoint: String,
        region: String,
        bucket: String,
        object_key: String,
        access_key: String,
        secret_key: String,
        session_token: String,
    },
}

#[derive(Debug, Clone)]
pub enum SyncResult {
    Uploaded {
        etag: Option<String>,
    },
    Downloaded {
        payload: SyncPayload,
        etag: Option<String>,
    },
    Failed(String),
}

pub async fn upload(
    credentials: SyncCredentials,
    payload: SyncPayload,
    expected_etag: Option<String>,
) -> Result<Option<String>> {
    validate_credentials(&credentials)?;
    let body = encrypt_payload(&payload, &credentials.encryption_password)?;
    match credentials.backend {
        SyncBackendCredentials::WebDav {
            endpoint,
            username,
            password,
        } => upload_webdav(&endpoint, &username, &password, body, expected_etag).await,
        SyncBackendCredentials::S3 {
            endpoint,
            region,
            bucket,
            object_key,
            access_key,
            secret_key,
            session_token,
        } => {
            let config = S3Config {
                endpoint,
                region,
                bucket,
                object_key,
                access_key,
                secret_key,
                session_token,
            };
            upload_s3(&config, body, expected_etag).await
        }
    }
}

async fn upload_webdav(
    endpoint: &str,
    username: &str,
    password: &str,
    body: Vec<u8>,
    expected_etag: Option<String>,
) -> Result<Option<String>> {
    let client = sync_client()?;
    let mut request = client
        .put(sync_url(endpoint))
        .basic_auth(username, Some(password))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body);
    request = if let Some(etag) = expected_etag {
        request.header(header::IF_MATCH, etag)
    } else {
        // An uninitialized client may only create a new remote file. This keeps
        // it from silently replacing configuration uploaded by another device.
        request.header(header::IF_NONE_MATCH, "*")
    };
    let response = request.send().await.context("send WebDAV upload")?;
    if response.status() == StatusCode::PRECONDITION_FAILED
        || response.status() == StatusCode::CONFLICT
    {
        return Err(anyhow!(
            "remote configuration changed; download it before uploading"
        ));
    }
    if !response.status().is_success() {
        return Err(anyhow!("WebDAV upload failed: HTTP {}", response.status()));
    }
    Ok(response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string))
}

pub async fn download(credentials: SyncCredentials) -> Result<(SyncPayload, Option<String>)> {
    validate_credentials(&credentials)?;
    let encryption_password = credentials.encryption_password;
    let (body, etag) = match credentials.backend {
        SyncBackendCredentials::WebDav {
            endpoint,
            username,
            password,
        } => download_webdav(&endpoint, &username, &password).await?,
        SyncBackendCredentials::S3 {
            endpoint,
            region,
            bucket,
            object_key,
            access_key,
            secret_key,
            session_token,
        } => {
            let config = S3Config {
                endpoint,
                region,
                bucket,
                object_key,
                access_key,
                secret_key,
                session_token,
            };
            download_s3(&config).await?
        }
    };
    let payload = decrypt_payload(&body, &encryption_password)?;
    Ok((payload, etag))
}

async fn download_webdav(
    endpoint: &str,
    username: &str,
    password: &str,
) -> Result<(Vec<u8>, Option<String>)> {
    let response = sync_client()?
        .get(sync_url(endpoint))
        .basic_auth(username, Some(password))
        .send()
        .await
        .context("send WebDAV download")?;
    if response.status() == StatusCode::NOT_FOUND {
        return Err(anyhow!("no remote configuration exists yet"));
    }
    if !response.status().is_success() {
        return Err(anyhow!(
            "WebDAV download failed: HTTP {}",
            response.status()
        ));
    }
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response
        .bytes()
        .await
        .context("read WebDAV response")?
        .to_vec();
    Ok((body, etag))
}

fn validate_credentials(credentials: &SyncCredentials) -> Result<()> {
    if credentials.encryption_password.len() < 8 {
        return Err(anyhow!(
            "encryption password must contain at least 8 characters"
        ));
    }
    match &credentials.backend {
        SyncBackendCredentials::WebDav { endpoint, .. } if endpoint.trim().is_empty() => {
            Err(anyhow!("WebDAV endpoint is required"))
        }
        SyncBackendCredentials::S3 {
            region,
            bucket,
            access_key,
            secret_key,
            ..
        } if region.trim().is_empty()
            || bucket.trim().is_empty()
            || access_key.trim().is_empty()
            || secret_key.is_empty() =>
        {
            Err(anyhow!(
                "S3 region, bucket, access key and secret key are required"
            ))
        }
        _ => Ok(()),
    }
}

struct S3Config {
    endpoint: String,
    region: String,
    bucket: String,
    object_key: String,
    access_key: String,
    secret_key: String,
    session_token: String,
}

async fn upload_s3(
    config: &S3Config,
    body: Vec<u8>,
    expected_etag: Option<String>,
) -> Result<Option<String>> {
    let url = s3_url(config)?;
    let mut headers = signed_s3_headers("PUT", &url, &body, config)?;
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    if let Some(etag) = expected_etag {
        headers.insert(header::IF_MATCH, header_value(&etag, "S3 ETag")?);
    } else {
        headers.insert(header::IF_NONE_MATCH, header::HeaderValue::from_static("*"));
    }
    let response = sync_client()?
        .put(url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .context("send S3 upload")?;
    if response.status() == StatusCode::PRECONDITION_FAILED
        || response.status() == StatusCode::CONFLICT
    {
        return Err(anyhow!(
            "remote configuration changed; download it before uploading"
        ));
    }
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        return Err(anyhow!("S3 upload failed: HTTP {status}: {detail}"));
    }
    Ok(response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string))
}

async fn download_s3(config: &S3Config) -> Result<(Vec<u8>, Option<String>)> {
    let url = s3_url(config)?;
    let headers = signed_s3_headers("GET", &url, &[], config)?;
    let response = sync_client()?
        .get(url)
        .headers(headers)
        .send()
        .await
        .context("send S3 download")?;
    if response.status() == StatusCode::NOT_FOUND {
        return Err(anyhow!("no remote configuration exists yet"));
    }
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        return Err(anyhow!("S3 download failed: HTTP {status}: {detail}"));
    }
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response.bytes().await.context("read S3 response")?.to_vec();
    Ok((body, etag))
}

fn s3_url(config: &S3Config) -> Result<reqwest::Url> {
    let endpoint = if config.endpoint.trim().is_empty() {
        format!("https://s3.{}.amazonaws.com", config.region.trim())
    } else {
        config.endpoint.trim().trim_end_matches('/').to_string()
    };
    let key = if config.object_key.trim().is_empty() {
        SYNC_FILE_NAME
    } else {
        config.object_key.trim().trim_start_matches('/')
    };
    let url = format!(
        "{}/{}/{}",
        endpoint,
        aws_uri_encode(config.bucket.trim(), true),
        aws_uri_encode(key, false)
    );
    reqwest::Url::parse(&url).context("parse S3 object URL")
}

fn signed_s3_headers(
    method: &str,
    url: &reqwest::Url,
    body: &[u8],
    config: &S3Config,
) -> Result<header::HeaderMap> {
    let now = chrono::Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("S3 endpoint has no host"))?;
    let host = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let payload_hash = hex_sha256(body);
    let mut canonical_headers =
        format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n");
    let mut signed_headers = "host;x-amz-content-sha256;x-amz-date".to_string();
    if !config.session_token.is_empty() {
        canonical_headers.push_str(&format!(
            "x-amz-security-token:{}\n",
            config.session_token.trim()
        ));
        signed_headers.push_str(";x-amz-security-token");
    }
    let canonical_request = format!(
        "{method}\n{}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        url.path()
    );
    let scope = format!("{date}/{}/s3/aws4_request", config.region.trim());
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );
    let date_key = hmac_sha256(
        format!("AWS4{}", config.secret_key).as_bytes(),
        date.as_bytes(),
    )?;
    let region_key = hmac_sha256(&date_key, config.region.trim().as_bytes())?;
    let service_key = hmac_sha256(&region_key, b"s3")?;
    let signing_key = hmac_sha256(&service_key, b"aws4_request")?;
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        config.access_key.trim()
    );
    let mut headers = header::HeaderMap::new();
    headers.insert("x-amz-date", header_value(&amz_date, "S3 date")?);
    headers.insert(
        "x-amz-content-sha256",
        header_value(&payload_hash, "S3 payload hash")?,
    );
    headers.insert(
        header::AUTHORIZATION,
        header_value(&authorization, "S3 authorization")?,
    );
    if !config.session_token.is_empty() {
        headers.insert(
            "x-amz-security-token",
            header_value(config.session_token.trim(), "S3 session token")?,
        );
    }
    Ok(headers)
}

fn header_value(value: &str, name: &str) -> Result<header::HeaderValue> {
    header::HeaderValue::from_str(value).with_context(|| format!("invalid {name}"))
}

fn hmac_sha256(key: &[u8], value: &[u8]) -> Result<Vec<u8>> {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| anyhow!("initialize S3 signer"))?;
    mac.update(value);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex_sha256(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn aws_uri_encode(value: &str, encode_slash: bool) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (!encode_slash && byte == b'/')
        {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn sync_url(endpoint: &str) -> String {
    let endpoint = endpoint.trim();
    if endpoint.ends_with('/') {
        format!("{endpoint}{SYNC_FILE_NAME}")
    } else if endpoint.ends_with(".json") {
        endpoint.to_string()
    } else {
        format!("{endpoint}/{SYNC_FILE_NAME}")
    }
}

fn encrypt_payload(payload: &SyncPayload, password: &str) -> Result<Vec<u8>> {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);
    let key = derive_key(password, &salt)?;
    let plaintext = serde_json::to_vec(payload).context("serialize sync payload")?;
    let ciphertext = XChaCha20Poly1305::new((&key).into())
        .encrypt(XNonce::from_slice(&nonce), plaintext.as_ref())
        .map_err(|_| anyhow!("encrypt sync payload"))?;
    serde_json::to_vec_pretty(&EncryptedEnvelope {
        format_version: FORMAT_VERSION,
        kdf: "argon2id".to_string(),
        cipher: "xchacha20poly1305".to_string(),
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce),
        payload: STANDARD.encode(ciphertext),
    })
    .context("serialize encrypted sync envelope")
}

fn decrypt_payload(raw: &[u8], password: &str) -> Result<SyncPayload> {
    let envelope: EncryptedEnvelope =
        serde_json::from_slice(raw).context("parse encrypted sync envelope")?;
    if envelope.format_version != FORMAT_VERSION
        || envelope.kdf != "argon2id"
        || envelope.cipher != "xchacha20poly1305"
    {
        return Err(anyhow!("unsupported remote sync format"));
    }
    let salt = STANDARD.decode(envelope.salt).context("decode sync salt")?;
    let nonce = STANDARD
        .decode(envelope.nonce)
        .context("decode sync nonce")?;
    if nonce.len() != 24 {
        return Err(anyhow!("invalid sync nonce"));
    }
    let ciphertext = STANDARD
        .decode(envelope.payload)
        .context("decode encrypted sync payload")?;
    let key = derive_key(password, &salt)?;
    let plaintext = XChaCha20Poly1305::new((&key).into())
        .decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| anyhow!("cannot decrypt remote configuration; check the password"))?;
    let payload: SyncPayload =
        serde_json::from_slice(&plaintext).context("parse decrypted sync payload")?;
    if payload.schema_version != FORMAT_VERSION {
        return Err(anyhow!("unsupported synchronized configuration version"));
    }
    Ok(payload)
}

fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|err| anyhow!("derive encryption key: {err}"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_payload_round_trip() {
        let payload = SyncPayload::new("test-device".into(), Vec::new());
        let encrypted = encrypt_payload(&payload, "correct horse battery staple").unwrap();
        assert!(!String::from_utf8_lossy(&encrypted).contains("test-device"));
        let decrypted = decrypt_payload(&encrypted, "correct horse battery staple").unwrap();
        assert_eq!(decrypted.revision, payload.revision);
    }

    #[test]
    fn wrong_password_is_rejected() {
        let payload = SyncPayload::new("test-device".into(), Vec::new());
        let encrypted = encrypt_payload(&payload, "correct horse battery staple").unwrap();
        assert!(decrypt_payload(&encrypted, "incorrect password").is_err());
    }

    #[test]
    fn endpoint_can_be_a_collection_or_file() {
        assert_eq!(
            sync_url("https://example.test/dav/"),
            "https://example.test/dav/ashell-sync.json"
        );
        assert_eq!(
            sync_url("https://example.test/config.json"),
            "https://example.test/config.json"
        );
    }

    #[test]
    fn s3_url_uses_path_style_and_encodes_object_key() {
        let config = S3Config {
            endpoint: "https://s3.example.test".into(),
            region: "us-east-1".into(),
            bucket: "my-bucket".into(),
            object_key: "configs/my file.json".into(),
            access_key: "access".into(),
            secret_key: "secret".into(),
            session_token: String::new(),
        };
        assert_eq!(
            s3_url(&config).unwrap().as_str(),
            "https://s3.example.test/my-bucket/configs/my%20file.json"
        );
    }

    #[test]
    fn aws_uri_encoding_preserves_only_object_key_slashes() {
        assert_eq!(aws_uri_encode("a b/c", false), "a%20b/c");
        assert_eq!(aws_uri_encode("a/b", true), "a%2Fb");
    }
}
