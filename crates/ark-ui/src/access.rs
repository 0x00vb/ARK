//! Access tokens for a runtime that other machines connect to (ADR 0016).
//!
//! A token is created once, shown once, and kept only as its SHA-256 hash in
//! `<store>/access.json` (mode 0600 on Unix). Each has a name ("laptop", "phone") so it can be
//! revoked alone. The file is read on every check, so a revoke takes effect without a restart.
//! A token can do everything the window can: it is the runtime owner's key, not a user account.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The saved tokens of one runtime.
#[derive(Clone, Debug)]
pub struct Tokens {
    path: PathBuf,
}

/// A saved token, without its secret.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenInfo {
    pub name: String,
    pub created_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    #[serde(flatten)]
    info: TokenInfo,
    sha256: String,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    tokens: Vec<Entry>,
}

impl Tokens {
    /// The tokens kept in a store root (`<root>/access.json`).
    pub fn in_store(root: impl AsRef<Path>) -> Tokens {
        Tokens::at(root.as_ref().join("access.json"))
    }

    /// The tokens kept in this file.
    pub fn at(path: impl Into<PathBuf>) -> Tokens {
        Tokens { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create a token named `name` and return it. It is not stored and cannot be shown again.
    pub fn create(&self, name: &str) -> anyhow::Result<String> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("a token needs a name");
        }
        let mut file = self.read()?;
        if file.tokens.iter().any(|t| t.info.name == name) {
            anyhow::bail!("a token named `{name}` exists; revoke it first");
        }
        let token = format!(
            "ark_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        file.tokens.push(Entry {
            info: TokenInfo {
                name: name.to_string(),
                created_ms: now_ms(),
            },
            sha256: hash(&token),
        });
        self.write(&file)?;
        Ok(token)
    }

    /// Every saved token, oldest first.
    pub fn list(&self) -> anyhow::Result<Vec<TokenInfo>> {
        Ok(self.read()?.tokens.into_iter().map(|t| t.info).collect())
    }

    /// Remove the token named `name`. False when there is none.
    pub fn revoke(&self, name: &str) -> anyhow::Result<bool> {
        let mut file = self.read()?;
        let before = file.tokens.len();
        file.tokens.retain(|t| t.info.name != name);
        if file.tokens.len() == before {
            return Ok(false);
        }
        self.write(&file)?;
        Ok(true)
    }

    /// The name of the saved token `token` is, if any.
    pub fn check(&self, token: &str) -> Option<String> {
        if token.is_empty() {
            return None;
        }
        let h = hash(token);
        self.read()
            .ok()?
            .tokens
            .into_iter()
            .find(|t| same(&t.sha256, &h))
            .map(|t| t.info.name)
    }

    fn read(&self) -> anyhow::Result<File> {
        match std::fs::read(&self.path) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
            Err(e) => Err(anyhow::anyhow!("reading {}: {e}", self.path.display())),
        }
    }

    fn write(&self, file: &File) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(file)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Compare without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_check_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::in_store(dir.path());
        assert_eq!(tokens.check("ark_nothing"), None);
        let t = tokens.create("laptop").unwrap();
        assert!(t.starts_with("ark_") && t.len() == 68);
        assert_eq!(tokens.check(&t).as_deref(), Some("laptop"));
        assert!(tokens.create("laptop").is_err(), "names are unique");
        let raw = std::fs::read_to_string(tokens.path()).unwrap();
        assert!(!raw.contains(&t), "only the hash is stored");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tokens.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(tokens.revoke("laptop").unwrap());
        assert!(!tokens.revoke("laptop").unwrap());
        assert_eq!(tokens.check(&t), None);
        assert!(tokens.list().unwrap().is_empty());
    }
}
