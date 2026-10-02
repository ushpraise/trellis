use std::io;

/// Keystore service for managing Stellar secret keys securely.
/// Supports OS keychain (via keyring crate) and encrypted file storage.
pub struct Keystore {
    keyring_service: String,
}

impl Keystore {
    pub fn new() -> Self {
        Keystore {
            keyring_service: "trellis".to_string(),
        }
    }

    /// Retrieve a key from the OS keychain by identity name.
    pub fn get_from_keychain(&self, identity: &str) -> io::Result<String> {
        keyring::Entry::new(&self.keyring_service, identity)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            .get_password()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }

    /// Store a key in the OS keychain by identity name.
    pub fn store_in_keychain(&self, identity: &str, key: &str) -> io::Result<()> {
        keyring::Entry::new(&self.keyring_service, identity)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            .set_password(key)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }

    /// Remove a key from the OS keychain.
    pub fn remove_from_keychain(&self, identity: &str) -> io::Result<()> {
        keyring::Entry::new(&self.keyring_service, identity)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            .delete_credential()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }

    /// List all identities stored in the OS keychain.
    /// Note: keyring crate does not provide a direct listing API,
    /// so this returns an empty list. In production, maintain a separate
    /// config file or use native keychain APIs for enumeration.
    pub fn list_keychain_identities(&self) -> Vec<String> {
        vec![]
    }

    /// Resolve a key source: keychain, env var, or literal.
    /// Priority: TRELLIS_SOURCE_KEY (plaintext, shows warning) > keychain lookup > literal
    pub fn resolve_source_key(&self, identity_or_key: &str) -> io::Result<String> {
        if let Ok(env_key) = std::env::var("TRELLIS_SOURCE_KEY") {
            eprintln!(
                "⚠️  Warning: TRELLIS_SOURCE_KEY contains a plaintext secret key.\n\
                    This is insecure and may be logged by shells and system tools.\n\
                    Consider using keystore instead:\n\
                      trellis keys add <identity> <secret-key>\n\
                      trellis keys set-default <identity>"
            );
            return Ok(env_key);
        }

        if let Ok(keychain_key) = self.get_from_keychain(identity_or_key) {
            return Ok(keychain_key);
        }

        if identity_or_key.starts_with('S') && identity_or_key.len() == 56 {
            return Ok(identity_or_key.to_string());
        }

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Key not found: {} (not in keychain, env var, or valid Stellar key)", identity_or_key),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keystore_creation() {
        let ks = Keystore::new();
        assert_eq!(ks.keyring_service, "trellis");
    }

    #[test]
    fn list_identities_returns_empty() {
        let ks = Keystore::new();
        assert_eq!(ks.list_keychain_identities(), vec![] as Vec<String>);
    }
}
