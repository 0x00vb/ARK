//! Durable model-account identities. Vendor credentials stay in their isolated profile.

use crate::{ConnectError, Secrets};
use ark_model::accounts::{AccountState, ModelAccount};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};
type StoreLock = Weak<Mutex<()>>;
static LOCKS: LazyLock<Mutex<HashMap<PathBuf, StoreLock>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
pub struct ModelAccounts {
    root: PathBuf,
    secrets: Arc<dyn Secrets>,
    mutation: Arc<Mutex<()>>,
}

impl ModelAccounts {
    pub fn open(root: &Path, secrets: Arc<dyn Secrets>) -> Self {
        let root = root
            .canonicalize()
            .unwrap_or_else(|_| root.to_path_buf())
            .join("model-accounts");
        let mut locks = LOCKS.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        let mutation = locks.get(&root).and_then(Weak::upgrade).unwrap_or_default();
        locks.insert(root.clone(), Arc::downgrade(&mutation));
        Self {
            root,
            secrets,
            mutation,
        }
    }

    pub fn list(&self) -> Result<Vec<ModelAccount>, ConnectError> {
        Ok(self.records()?.into_iter().filter(|a| !a.removed).collect())
    }
    fn records(&self) -> Result<Vec<ModelAccount>, ConnectError> {
        let bytes = match std::fs::read(self.root.join("accounts.json")) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let accounts: Vec<ModelAccount> = serde_json::from_slice(&bytes)
            .map_err(|_| ConnectError::Provider("Could not read model accounts.".into()))?;
        let mut ids = std::collections::HashSet::new();
        for account in &accounts {
            validate_id(&account.id, &account.provider)?;
            if account.revision >= u64::MAX - 2 || !ids.insert(&account.id) {
                return Err(ConnectError::Provider(
                    "Duplicate model account record.".into(),
                ));
            }
        }
        Ok(accounts)
    }

    pub fn directory(&self, id: &str) -> Result<PathBuf, ConnectError> {
        let account = self.find(id)?;
        validate_id(id, &account.provider)?;
        let path = self.root.join(id);
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(ConnectError::Provider(
                "The account folder is a link.".into(),
            ));
        }
        Ok(path)
    }

    pub fn find(&self, id: &str) -> Result<ModelAccount, ConnectError> {
        self.list()?
            .into_iter()
            .find(|a| a.id == id && !a.removed)
            .ok_or_else(|| {
                ConnectError::NotConnected(
                    "This account was removed. Choose an account in Models.".into(),
                )
            })
    }

    pub(crate) fn create(
        &self,
        provider: &str,
        request: &str,
        name: &str,
        key: Option<&str>,
    ) -> Result<ModelAccount, ConnectError> {
        let id = format!("{provider}~{request}");
        validate_id(&id, provider)?;
        let _lock = self.mutation.lock().unwrap();
        let mut accounts = self.records()?;
        if let Some(account) = accounts.iter().find(|a| a.id == id) {
            if account.removed {
                return Err(ConnectError::Cancelled(
                    "This setup was removed. Start Add account again.".into(),
                ));
            }
            return Ok(account.clone());
        }
        if accounts.iter().filter(|a| !a.removed).count() >= 32 {
            return Err(ConnectError::Provider(
                "Remove an account before adding another.".into(),
            ));
        }
        private_directory(&self.root)?;
        private_directory(&self.root.join(&id))?;
        if key.is_none() {
            std::fs::write(self.root.join(&id).join(".ark-setup-unused"), b"")?;
        }
        let name = if name.trim().is_empty() {
            format!(
                "Account {}",
                2 + accounts.iter().filter(|a| a.provider == provider).count()
            )
        } else {
            valid_name(name)?
        };
        let account = ModelAccount {
            id,
            provider: provider.into(),
            name,
            revision: 1,
            state: if key.is_some() {
                AccountState::Active
            } else {
                AccountState::Reconnecting
            },
            identity: None,
            removed: false,
        };
        if let Some(key) = key {
            self.secrets.set(&secret(&account.id), key)?;
        }
        accounts.push(account.clone());
        if let Err(error) = self.write(&accounts) {
            if key.is_some() {
                self.secrets.delete(&secret(&account.id))?;
            }
            return Err(error);
        }
        Ok(account)
    }

    pub fn activate(
        &self,
        expected: &ModelAccount,
        identity: Option<&str>,
    ) -> Result<ModelAccount, ConnectError> {
        self.update(&expected.id, |a| {
            if a.revision != expected.revision || a.state == AccountState::Removing {
                return Err(ConnectError::Cancelled(
                    "This sign-in is no longer active.".into(),
                ));
            }
            if a.identity.is_some() && identity.is_none() {
                return Err(ConnectError::Refused(
                    "Could not verify this account. Try signing in again.".into(),
                ));
            }
            if let (Some(before), Some(after)) = (a.identity.as_deref(), identity) {
                if !before.eq_ignore_ascii_case(after) {
                    return Err(ConnectError::Refused(
                        "That is a different account. Add it with Add account.".into(),
                    ));
                }
            }
            if let Some(identity) = identity {
                a.identity = Some(identity.into());
                if a.name.starts_with("Account ") {
                    a.name = identity.into();
                }
            }
            a.state = AccountState::Active;
            Ok(())
        })
    }

    pub fn replace_key(&self, id: &str, key: &str) -> Result<ModelAccount, ConnectError> {
        self.update(id, |a| {
            if a.state != AccountState::Reconnecting || ark_model::accounts::active(id) != 0 {
                return Err(ConnectError::Provider(
                    "This account is busy. Try again when its request finishes.".into(),
                ));
            }
            self.secrets.set(&secret(id), key)?;
            a.revision += 1;
            a.state = AccountState::Active;
            Ok(())
        })
    }
    pub fn rename(&self, id: &str, name: &str) -> Result<ModelAccount, ConnectError> {
        let name = valid_name(name)?;
        self.update(id, |a| {
            a.name = name;
            Ok(())
        })
    }
    pub fn reconnect(&self, id: &str) -> Result<ModelAccount, ConnectError> {
        self.update(id, |a| {
            if a.state != AccountState::Reconnecting || ark_model::accounts::active(id) != 0 {
                return Err(ConnectError::Provider(
                    "This account is busy or being removed.".into(),
                ));
            }
            a.revision += 1;
            a.state = AccountState::Active;
            Ok(())
        })
    }
    pub fn begin_reconnect(&self, id: &str) -> Result<ModelAccount, ConnectError> {
        self.update(id, |a| {
            if a.state == AccountState::Removing {
                return Err(ConnectError::Provider(
                    "Finish removing this account first.".into(),
                ));
            }
            if a.state == AccountState::Active {
                a.revision += 1;
                a.state = AccountState::Reconnecting;
            }
            Ok(())
        })
    }
    pub fn begin_remove(&self, id: &str) -> Result<ModelAccount, ConnectError> {
        self.update(id, |a| {
            if a.state != AccountState::Removing {
                a.revision += 1;
                a.state = AccountState::Removing;
            }
            Ok(())
        })
    }

    pub fn finish_remove(&self, id: &str) -> Result<(), ConnectError> {
        let _lock = self.mutation.lock().unwrap();
        let mut accounts = self.records()?;
        let Some(account) = accounts.iter().find(|a| a.id == id) else {
            return Ok(());
        };
        if account.state != AccountState::Removing {
            return Err(ConnectError::Provider(
                "Stop account access before removing it.".into(),
            ));
        }
        if account.removed {
            return Ok(());
        }
        if ark_model::accounts::active(id) != 0 {
            return Err(ConnectError::Provider(
                "This account is still in use.".into(),
            ));
        }
        self.secrets.delete(&secret(id))?;
        let dir = self.root.join(id);
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(ConnectError::Provider(
                    "The account folder is a link.".into(),
                ))
            }
            Ok(_) => std::fs::remove_dir_all(dir)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let account = accounts
            .iter_mut()
            .find(|a| a.id == id)
            .expect("account was found");
        account.removed = true;
        account.revision += 1;
        account.name.clear();
        account.identity = None;
        self.write(&accounts)
    }

    pub fn key(&self, id: &str) -> Result<Option<String>, ConnectError> {
        let account = self.find(id)?;
        if account.state != AccountState::Active {
            return Ok(None);
        }
        self.secrets.get(&secret(id))
    }

    fn update(
        &self,
        id: &str,
        change: impl FnOnce(&mut ModelAccount) -> Result<(), ConnectError>,
    ) -> Result<ModelAccount, ConnectError> {
        let _lock = self.mutation.lock().unwrap();
        let mut accounts = self.records()?;
        let account = accounts
            .iter_mut()
            .find(|a| a.id == id && !a.removed)
            .ok_or_else(|| ConnectError::NotConnected("This account was removed.".into()))?;
        change(account)?;
        let updated = account.clone();
        self.write(&accounts)?;
        Ok(updated)
    }

    fn write(&self, accounts: &[ModelAccount]) -> Result<(), ConnectError> {
        private_directory(&self.root)?;
        let temporary = self.root.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let bytes = serde_json::to_vec(accounts)
            .map_err(|_| ConnectError::Provider("Could not save model accounts.".into()))?;
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(temporary, self.root.join("accounts.json"))?;
        Ok(())
    }
}

fn secret(id: &str) -> String {
    format!("model-account:{id}")
}
fn valid_name(name: &str) -> Result<String, ConnectError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(ConnectError::Provider(
            "Use a name of 1 to 80 characters.".into(),
        ));
    }
    Ok(name.into())
}
fn validate_id(id: &str, provider: &str) -> Result<(), ConnectError> {
    let Some((prefix, uuid)) = id.split_once('~') else {
        return Err(ConnectError::Provider("Invalid account identity.".into()));
    };
    if prefix != provider
        || provider.is_empty()
        || !provider
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
        || uuid::Uuid::parse_str(uuid).is_err()
    {
        return Err(ConnectError::Provider("Invalid account identity.".into()));
    }
    Ok(())
}
fn private_directory(path: &Path) -> Result<(), ConnectError> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(ConnectError::Provider(
                "The account folder is not a directory.".into(),
            ));
        }
    }
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
