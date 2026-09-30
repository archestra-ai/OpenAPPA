//! Standalone credential storage. No secret is part of a policy or trajectory.
#[cfg(any(feature = "daemon", test))]
use std::collections::BTreeMap;
use std::ffi::OsString;
#[cfg(any(feature = "daemon", test))]
use std::path::Path;
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone)]
pub(crate) struct CredentialStore {
    path: PathBuf,
    deployment: String,
}

impl CredentialStore {
    #[cfg(any(feature = "daemon", test))]
    pub(crate) fn for_config(config: &Path) -> Result<Self, String> {
        let absolute = std::path::absolute(config).map_err(|_| "Cannot resolve configuration path")?;
        let parent = absolute.parent().ok_or("Configuration has no parent directory")?;
        let parent = parent
            .canonicalize()
            .map_err(|_| "Configuration directory does not exist")?;
        let config = config
            .canonicalize()
            .unwrap_or_else(|_| parent.join(config.file_name().unwrap_or_default()));
        Ok(Self {
            path: config
                .parent()
                .ok_or("Configuration has no parent directory")?
                .join("credentials.db"),
            deployment: config.to_string_lossy().into_owned(),
        })
    }

    fn open(&self, write: bool) -> Result<Option<Connection>, String> {
        if !write && !self.path.exists() {
            return Ok(None);
        }
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path)
            && (!metadata.is_file() || metadata.file_type().is_symlink())
        {
            return Err("Credential database must be a regular file".into());
        }
        if write {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            let file = options
                .open(&self.path)
                .map_err(|_| "Cannot open credential database")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|_| "Cannot protect credential database")?;
            }
            drop(file);
        }
        let flags = if write {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        let connection =
            Connection::open_with_flags(&self.path, flags).map_err(|_| "Cannot open credential database")?;
        connection
            .busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| "Cannot configure credential database")?;
        if write {
            connection
                .execute_batch(
                    "PRAGMA secure_delete = ON; CREATE TABLE IF NOT EXISTS credentials (
                deployment TEXT NOT NULL, variable TEXT NOT NULL, value TEXT NOT NULL,
                PRIMARY KEY (deployment, variable));",
                )
                .map_err(|_| "Cannot initialize credential database")?;
        }
        Ok(Some(connection))
    }

    #[cfg(any(feature = "daemon", test))]
    pub(crate) fn values(&self) -> Result<BTreeMap<String, String>, String> {
        let Some(connection) = self.open(false)? else {
            return Ok(BTreeMap::new());
        };
        let mut statement = connection
            .prepare("SELECT variable, value FROM credentials WHERE deployment = ?1")
            .map_err(|_| "Cannot read credential database")?;
        let rows = statement
            .query_map([&self.deployment], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|_| "Cannot read credential database")?;
        rows.collect::<Result<_, _>>()
            .map_err(|_| "Cannot read credential database".into())
    }

    fn get(&self, variable: &str) -> Result<Option<String>, String> {
        let Some(connection) = self.open(false)? else {
            return Ok(None);
        };
        connection
            .query_row(
                "SELECT value FROM credentials WHERE deployment = ?1 AND variable = ?2",
                params![self.deployment, variable],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "Cannot read credential database".into())
    }

    /// A batch is atomic. None deletes a saved value; an empty value is refused.
    #[cfg(any(feature = "daemon", test))]
    pub(crate) fn update(&self, changes: &BTreeMap<String, Option<String>>) -> Result<(), String> {
        if changes.iter().any(|(var, value)| {
            !var.starts_with("APPA_")
                || !var
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                || value
                    .as_ref()
                    .is_some_and(|v| v.trim().is_empty() || v.len() > 16384 || v.contains('\0'))
        }) {
            return Err("Invalid credential variable or value".into());
        }
        let mut connection = self.open(true)?.ok_or("Cannot open credential database")?;
        let transaction = connection.transaction().map_err(|_| "Cannot update credentials")?;
        for (variable, value) in changes {
            match value {
                Some(value) => transaction.execute(
                    "INSERT INTO credentials (deployment, variable, value) VALUES (?1, ?2, ?3)
                    ON CONFLICT(deployment, variable) DO UPDATE SET value = excluded.value",
                    params![self.deployment, variable, value],
                ),
                None => transaction.execute(
                    "DELETE FROM credentials WHERE deployment = ?1 AND variable = ?2",
                    params![self.deployment, variable],
                ),
            }
            .map_err(|_| "Cannot update credentials")?;
        }
        transaction.commit().map_err(|_| "Cannot commit credentials".into())
    }
}

/// Preserve environment precedence, including an explicitly empty environment value.
pub(crate) fn resolve(store: Option<&CredentialStore>, variable: &str) -> Result<Option<OsString>, String> {
    if let Some(value) = std::env::var_os(variable) {
        return Ok(Some(value));
    }
    store
        .map(|store| store.get(variable))
        .transpose()
        .map(|value| value.flatten().map(OsString::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_credentials_are_scoped_replaceable_and_removable() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::for_config(&dir.path().join("one.toml")).unwrap();
        let other = CredentialStore::for_config(&dir.path().join("two.toml")).unwrap();
        let variable = "APPA_PROVIDER_STORAGE_TEST";
        assert!(store.values().unwrap().is_empty());
        store
            .update(&BTreeMap::from([(variable.into(), Some("first".into()))]))
            .unwrap();
        assert_eq!(resolve(Some(&store), variable).unwrap(), Some("first".into()));
        assert!(other.values().unwrap().is_empty());
        assert!(resolve(None, variable).unwrap().is_none());
        store
            .update(&BTreeMap::from([(variable.into(), Some("second".into()))]))
            .unwrap();
        assert_eq!(resolve(Some(&store), variable).unwrap(), Some("second".into()));
        store.update(&BTreeMap::from([(variable.into(), None)])).unwrap();
        assert!(store.values().unwrap().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&store.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn environment_wins_and_invalid_batch_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::for_config(&dir.path().join("appa.toml")).unwrap();
        // PATH is already set, so this test never changes the process environment.
        let expected = std::env::var_os("PATH").unwrap();
        assert_eq!(resolve(Some(&store), "PATH").unwrap(), Some(expected));
        assert!(
            store
                .update(&BTreeMap::from([
                    ("APPA_PROVIDER_A".into(), Some("secret".into())),
                    ("APPA_PROVIDER_B".into(), Some("".into()))
                ]))
                .is_err()
        );
        assert!(store.values().unwrap().is_empty());
    }
}
