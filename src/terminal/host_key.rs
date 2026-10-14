use crate::{error::Error, model::Connection};
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::{
    ssh_key::known_hosts::{Entry, HostPatterns, Marker},
    PublicKeyOrCertificate,
};
use sha1::Sha1;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

pub(crate) struct HostVerifier {
    strict: bool,
    host: String,
    entries: Vec<Entry>,
    pub rejected: Arc<AtomicBool>,
    pub identity: Arc<Mutex<Option<String>>>,
}

impl HostVerifier {
    pub fn new(connection: &Connection) -> Result<Self, Error> {
        let host = connection
            .host
            .trim_matches(['[', ']'])
            .to_ascii_lowercase();
        let host = if connection.port == 22 {
            host
        } else {
            format!("[{host}]:{}", connection.port)
        };
        let mut entries = Vec::new();
        if connection.strict_host_key_check {
            for line in connection.known_hosts.as_deref().unwrap_or("").lines() {
                let line = line.split('#').next().unwrap_or("").trim();
                if line.is_empty() {
                    continue;
                }
                let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
                entries.push(
                    normalized
                        .parse()
                        .map_err(|_| Error::Invalid("known_hosts 格式或密钥不合法".into()))?,
                );
            }
            if entries.is_empty() {
                return Err(Error::Invalid("known_hosts 不能为空".into()));
            }
        }
        Ok(Self {
            strict: connection.strict_host_key_check,
            host,
            entries,
            rejected: Arc::new(AtomicBool::new(false)),
            identity: Arc::new(Mutex::new(None)),
        })
    }

    fn accepts(&self, key: &PublicKeyOrCertificate) -> bool {
        if !self.strict {
            return true;
        }
        // Certificate authorities require signature/validity/principal validation;
        // do not accept certificates by just comparing their embedded public key.
        if key.certificate().is_some() {
            return false;
        }
        let key = key.public_key();
        let mut trusted = false;
        for entry in &self.entries {
            if !matches_host(entry.host_patterns(), &self.host) {
                continue;
            }
            if entry.public_key().key_data() != key.key_data() {
                continue;
            }
            match entry.marker() {
                Some(Marker::Revoked) => return false,
                Some(Marker::CertAuthority) => {}
                None => trusted = true,
            }
        }
        trusted
    }
}

impl russh::client::Handler for HostVerifier {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let accepted = self.accepts(key);
        if !accepted {
            self.rejected.store(true, Ordering::SeqCst);
        } else {
            *self.identity.lock().unwrap() = Some(
                key.public_key()
                    .fingerprint(russh::keys::HashAlg::Sha256)
                    .to_string(),
            );
        }
        Ok(accepted)
    }
}

fn matches_host(patterns: &HostPatterns, host: &str) -> bool {
    match patterns {
        HostPatterns::HashedName { salt, hash } => Hmac::<Sha1>::new_from_slice(salt)
            .is_ok_and(|h| h.chain_update(host).verify_slice(hash).is_ok()),
        HostPatterns::Patterns(patterns) => {
            let mut matched = false;
            for pattern in patterns {
                if let Some(negative) = pattern.strip_prefix('!') {
                    if glob(negative, host) {
                        return false;
                    }
                } else if glob(pattern, host) {
                    matched = true;
                }
            }
            matched
        }
    }
}

// OpenSSH * and ? matching. Brackets are literal (host/port notation), not globs.
fn glob(pattern: &str, host: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let p = pattern.as_bytes();
    let h = host.as_bytes();
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < h.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == h[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            retry = j;
        } else if let Some(s) = star {
            retry += 1;
            j = retry;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::{key::safe_rng, Algorithm, PrivateKey};
    fn connection(hosts: String) -> Connection {
        Connection {
            connection_id: "test".into(),
            connection_name: "test".into(),
            host: "example.test".into(),
            port: 2222,
            username: "test".into(),
            auth_type: 1,
            user_id: "default".into(),
            encrypted: 1,
            created_at: "".into(),
            updated_at: "".into(),
            connect_timeout: 10,
            keepalive_interval: 60,
            startup_command: None,
            compression: false,
            strict_host_key_check: true,
            known_hosts: Some(hosts),
        }
    }
    #[test]
    fn host_port_key_comments_and_revocation_are_checked() {
        let key = PrivateKey::random(&mut safe_rng(), Algorithm::Ed25519).unwrap();
        let public = key.public_key().to_openssh().unwrap();
        let verifier = HostVerifier::new(&connection(format!(
            "[example.test]:2222 {public} extra comment"
        )))
        .unwrap();
        assert!(verifier.accepts(&key.public_key().clone().into()));
        let other = PrivateKey::random(&mut safe_rng(), Algorithm::Ed25519).unwrap();
        assert!(!verifier.accepts(&other.public_key().clone().into()));
        let verifier = HostVerifier::new(&connection(format!("example.test {public}"))).unwrap();
        assert!(!verifier.accepts(&key.public_key().clone().into()));
        let verifier = HostVerifier::new(&connection(format!(
            "[example.test]:2222 {public}\n@revoked [example.test]:2222 {public}"
        )))
        .unwrap();
        assert!(!verifier.accepts(&key.public_key().clone().into()));
        let verifier = HostVerifier::new(&connection(format!(
            "@cert-authority [example.test]:2222 {public}"
        )))
        .unwrap();
        assert!(!verifier.accepts(&key.public_key().clone().into()));
    }
    #[test]
    fn glob_negation_and_hashed_names_match_correctly() {
        let patterns =
            HostPatterns::Patterns(vec!["[*.test]:2222".into(), "![blocked.test]:2222".into()]);
        assert!(matches_host(&patterns, "[example.test]:2222"));
        assert!(!matches_host(&patterns, "[blocked.test]:2222"));
        assert!(!matches_host(&patterns, "[example.test]:22"));
        let salt = b"twenty-byte-test-salt".to_vec();
        let hash: [u8; 20] = Hmac::<Sha1>::new_from_slice(&salt)
            .unwrap()
            .chain_update("[example.test]:2222")
            .finalize()
            .into_bytes()
            .into();
        let patterns = HostPatterns::HashedName { salt, hash };
        assert!(matches_host(&patterns, "[example.test]:2222"));
        assert!(!matches_host(&patterns, "[other.test]:2222"));
    }
}
