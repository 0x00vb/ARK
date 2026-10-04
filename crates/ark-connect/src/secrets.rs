//! Where account tokens are kept: the OS keychain (macOS Keychain, Windows Credential Manager,
//! the Linux kernel keyring), never a file in the store. Tests and headless apps can bring
//! another store.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::ConnectError;

pub trait Secrets: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, ConnectError>;
    fn set(&self, key: &str, value: &str) -> Result<(), ConnectError>;
    fn delete(&self, key: &str) -> Result<(), ConnectError>;
}

/// The OS keychain, under the service name `ark`.
pub struct Keychain;

const SERVICE: &str = "ark";

fn keychain_error(e: keyring::Error) -> ConnectError {
    ConnectError::Secrets(e.to_string())
}

impl Secrets for Keychain {
    fn get(&self, key: &str) -> Result<Option<String>, ConnectError> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(keychain_error)?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keychain_error(e)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), ConnectError> {
        keyring::Entry::new(SERVICE, key)
            .and_then(|e| e.set_password(value))
            .map_err(keychain_error)
    }

    fn delete(&self, key: &str) -> Result<(), ConnectError> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(keychain_error)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keychain_error(e)),
        }
    }
}

/// Secrets in memory, gone when the process ends (tests).
#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<String, String>>);

impl Secrets for MemorySecrets {
    fn get(&self, key: &str) -> Result<Option<String>, ConnectError> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), ConnectError> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), ConnectError> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real OS keychain, so it runs only when asked:
    /// `cargo test -p ark-connect -- --ignored keychain`.
    #[test]
    #[ignore]
    fn keychain_round_trip() {
        let k = Keychain;
        let key = format!("test:{}", std::process::id());
        assert_eq!(k.get(&key).unwrap(), None);
        k.set(&key, "v1").unwrap();
        assert_eq!(k.get(&key).unwrap().as_deref(), Some("v1"));
        k.set(&key, "v2").unwrap();
        assert_eq!(k.get(&key).unwrap().as_deref(), Some("v2"));
        k.delete(&key).unwrap();
        assert_eq!(k.get(&key).unwrap(), None);
        k.delete(&key).unwrap();
    }
}
