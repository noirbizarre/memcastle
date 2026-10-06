//! Where a signed-in source's tokens are kept: the platform's credential store, or an owner-only file (docs/adr/039).
//!
//! Never the palace database, which holds only digests of secrets (docs/adr/014), and never the configuration file.
//! A stored credential is the refresh token, which is what lets a source stay signed in for months, plus the little
//! needed to notice that it was obtained under terms the installed source no longer states.

use std::path::PathBuf;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{CredentialBackend, CredentialsConfig};
use crate::domain::is_valid_source_name;
use crate::error::{Error, Result};

/// The service name entries are filed under in the platform's credential store.
const SERVICE: &str = "memcastle";

/// Where a credential is kept, as `source auth` and `source show` name it.
pub const KEYRING: &str = "keyring";
/// The owner-only file fallback.
pub const FILE: &str = "file";

/// What is kept for one signed-in source.
///
/// Deliberately small: the Windows credential store refuses a secret much over 2.5 KB, and the access token, which
/// is short-lived and cheap to obtain again, is kept only when the provider issued no refresh token to obtain it from.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredential {
    /// [`crate::domain::OAuthRequirement::fingerprint`] of what this was obtained under.
    pub fingerprint: String,
    /// What a new access token is obtained with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Only when there is no refresh token: the one access token the provider gave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// When the access token kept here stops being valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// What the provider granted, as far as it said.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// When the user last signed in.
    pub obtained_at: DateTime<Utc>,
}

impl std::fmt::Debug for StoredCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // By hand, so that `{:?}` on anything holding one can never print a token.
        f.debug_struct("StoredCredential")
            .field("fingerprint", &self.fingerprint)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

/// A place credentials are kept, one per installed source.
///
/// Blocking: a platform store talks to a daemon or a system service, and a file is a file. Async callers use
/// `spawn_blocking`.
pub trait CredentialStore: Send + Sync {
    /// The credential kept for `source`, if any.
    fn load(&self, source: &str) -> Result<Option<StoredCredential>>;

    /// Keep `credential` for `source`, replacing what was there, and say where it went.
    fn save(&self, source: &str, credential: &StoredCredential) -> Result<&'static str>;

    /// Forget `source`'s credential. Forgetting what is not there is not an error.
    fn delete(&self, source: &str) -> Result<()>;

    /// Whether this store can be used at all here; a platform store on a host with no keyring cannot.
    fn usable(&self) -> bool {
        true
    }
}

fn failed(message: impl Into<String>) -> Error {
    Error::CredentialStoreFailed {
        message: message.into(),
    }
}

/// A source name goes into a file name and a keyring entry, so only a well-formed one gets that far.
fn checked(source: &str) -> Result<&str> {
    if is_valid_source_name(source) {
        Ok(source)
    } else {
        Err(failed(format!("`{source}` is not a source name")))
    }
}

/// The platform's credential store: Keychain, Credential Manager, or the Secret Service.
#[derive(Debug, Default)]
pub struct KeyringStore;

impl KeyringStore {
    fn entry(source: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, &format!("source:{}", checked(source)?)).map_err(|e| {
            failed(format!(
                "the platform credential store is not available: {e}"
            ))
        })
    }
}

impl CredentialStore for KeyringStore {
    fn load(&self, source: &str) -> Result<Option<StoredCredential>> {
        match Self::entry(source)?.get_password() {
            Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| {
                failed(format!(
                    "the stored credential for `{source}` is unreadable: {e}"
                ))
            }),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(failed(format!(
                "cannot read the platform credential store: {e}"
            ))),
        }
    }

    fn save(&self, source: &str, credential: &StoredCredential) -> Result<&'static str> {
        let text = serde_json::to_string(credential).map_err(|e| failed(e.to_string()))?;
        Self::entry(source)?
            .set_password(&text)
            .map_err(|e| failed(format!("cannot write the platform credential store: {e}")))?;
        Ok(KEYRING)
    }

    fn delete(&self, source: &str) -> Result<()> {
        match Self::entry(source)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(failed(format!(
                "cannot update the platform credential store: {e}"
            ))),
        }
    }

    fn usable(&self) -> bool {
        // Write, read back and delete a throwaway entry: a Linux host without a running Secret Service builds an entry
        // happily and only fails when it is used, and a store that cannot be used must be known before a token is
        // handed to it, not after the user has signed in.
        let probe = || -> keyring::Result<()> {
            let entry = keyring::Entry::new(SERVICE, "probe")?;
            entry.set_password("probe")?;
            let read = entry.get_password();
            let _ = entry.delete_credential();
            if read? == "probe" {
                Ok(())
            } else {
                Err(keyring::Error::Invalid(
                    "probe".into(),
                    "read back differently".into(),
                ))
            }
        };
        match probe() {
            Ok(()) => true,
            Err(e) => {
                tracing::info!(error = %e, "no usable platform credential store; falling back to a file");
                false
            }
        }
    }
}

/// An owner-only file per source under one directory.
#[derive(Debug)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// A store under `dir`, created on first write.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, source: &str) -> Result<PathBuf> {
        Ok(self.dir.join(format!("{}.json", checked(source)?)))
    }
}

impl CredentialStore for FileStore {
    fn load(&self, source: &str) -> Result<Option<StoredCredential>> {
        let path = self.path(source)?;
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| {
                failed(format!(
                    "the credential file {} is unreadable ({e}); delete it and sign in again",
                    path.display()
                ))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(failed(format!("cannot read {}: {e}", path.display()))),
        }
    }

    fn save(&self, source: &str, credential: &StoredCredential) -> Result<&'static str> {
        use std::io::Write as _;

        if !self.dir.is_absolute() {
            return Err(failed(format!(
                "the credentials directory {} is not absolute; set `credentials.dir`",
                self.dir.display()
            )));
        }
        let path = self.path(source)?;
        let text = serde_json::to_string(credential).map_err(|e| failed(e.to_string()))?;
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| failed(format!("cannot create {}: {e}", self.dir.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // Owner only, so another account on the machine cannot list who is signed in to what.
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| failed(format!("cannot restrict {}: {e}", self.dir.display())))?;
        }
        // Written beside the target and renamed over it, so that a crash mid-write never leaves half a token that
        // `load` would then refuse, signing the user out.
        let temporary = path.with_extension("json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            // At creation, not after: a file made world-readable and then restricted has been readable in between.
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|e| failed(format!("cannot write {}: {e}", temporary.display())))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| failed(format!("cannot write {}: {e}", temporary.display())))?;
        drop(file);
        std::fs::rename(&temporary, &path)
            .map_err(|e| failed(format!("cannot replace {}: {e}", path.display())))?;
        Ok(FILE)
    }

    fn delete(&self, source: &str) -> Result<()> {
        let path = self.path(source)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(failed(format!("cannot remove {}: {e}", path.display()))),
        }
    }
}

/// The platform store when it works, the file when it does not.
///
/// Whether the platform store works is decided once, on first use and not at startup: probing it can prompt for a
/// keychain password, which a daemon that never signs anything in should not provoke. A credential is looked for in
/// both, so one saved to the file while the keyring was down is still found once it is back.
pub struct AutoStore {
    primary: Box<dyn CredentialStore>,
    fallback: Box<dyn CredentialStore>,
    primary_usable: OnceLock<bool>,
}

impl AutoStore {
    /// `primary` when it is usable, `fallback` otherwise.
    #[must_use]
    pub fn new(primary: Box<dyn CredentialStore>, fallback: Box<dyn CredentialStore>) -> Self {
        Self {
            primary,
            fallback,
            primary_usable: OnceLock::new(),
        }
    }

    fn primary_works(&self) -> bool {
        *self.primary_usable.get_or_init(|| self.primary.usable())
    }
}

impl CredentialStore for AutoStore {
    fn load(&self, source: &str) -> Result<Option<StoredCredential>> {
        if self.primary_works()
            && let Some(found) = self.primary.load(source)?
        {
            return Ok(Some(found));
        }
        self.fallback.load(source)
    }

    fn save(&self, source: &str, credential: &StoredCredential) -> Result<&'static str> {
        if self.primary_works() {
            let kept = self.primary.save(source, credential)?;
            // A stale copy in the fallback would be found first by nobody, but it would outlive a sign-out.
            self.fallback.delete(source)?;
            return Ok(kept);
        }
        self.fallback.save(source, credential)
    }

    fn delete(&self, source: &str) -> Result<()> {
        if self.primary_works() {
            self.primary.delete(source)?;
        }
        self.fallback.delete(source)
    }
}

/// The store the configuration asks for.
#[must_use]
pub fn from_config(config: &CredentialsConfig) -> Box<dyn CredentialStore> {
    match config.backend {
        CredentialBackend::Keyring => Box::new(KeyringStore),
        CredentialBackend::File => Box::new(FileStore::new(config.dir())),
        CredentialBackend::Auto => Box::new(AutoStore::new(
            Box::new(KeyringStore),
            Box::new(FileStore::new(config.dir())),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    pub(crate) fn sample() -> StoredCredential {
        StoredCredential {
            fingerprint: "f".into(),
            refresh_token: Some("refresh-secret".into()),
            access_token: None,
            expires_at: None,
            scopes: vec!["read".into()],
            // Fixed: a test compares two samples, and two readings of the clock are never equal.
            obtained_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    #[test]
    fn a_file_store_keeps_loads_replaces_and_forgets_a_credential() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("credentials"));
        assert_eq!(store.load("demo").unwrap(), None);

        assert_eq!(store.save("demo", &sample()).unwrap(), FILE);
        assert_eq!(store.load("demo").unwrap(), Some(sample()));

        let mut newer = sample();
        newer.refresh_token = Some("rotated".into());
        store.save("demo", &newer).unwrap();
        assert_eq!(store.load("demo").unwrap(), Some(newer));

        store.delete("demo").unwrap();
        assert_eq!(store.load("demo").unwrap(), None);
        store.delete("demo").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_credential_file_and_its_directory_are_readable_by_the_owner_alone() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("credentials");
        let store = FileStore::new(root.clone());
        store.save("demo", &sample()).unwrap();

        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&root.join("demo.json")), 0o600);
        // Rewriting keeps it that way, and leaves no half-written file behind.
        store.save("demo", &sample()).unwrap();
        assert_eq!(mode(&root.join("demo.json")), 0o600);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn a_name_that_is_not_a_source_name_never_reaches_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());
        for bad in ["../x", "a/b", "", "UPPER"] {
            assert!(store.save(bad, &sample()).is_err(), "{bad:?}");
            assert!(store.load(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_corrupt_credential_file_is_an_error_that_says_to_sign_in_again_and_not_a_missing_credential()
     {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("demo.json"), "{ not json").unwrap();
        let error = FileStore::new(dir.path().to_path_buf())
            .load("demo")
            .unwrap_err()
            .to_string();
        assert!(error.contains("sign in again"), "{error}");
    }

    #[test]
    fn a_relative_credentials_directory_is_refused_rather_than_written_into_the_working_directory()
    {
        let error = FileStore::new(PathBuf::from("credentials"))
            .save("demo", &sample())
            .unwrap_err()
            .to_string();
        assert!(error.contains("credentials.dir"), "{error}");
    }

    #[test]
    fn a_stored_credential_never_prints_a_token() {
        let printed = format!("{:?}", sample());
        assert!(!printed.contains("refresh-secret"), "{printed}");
        assert!(printed.contains("REDACTED"));
    }

    /// A store that records what it is asked, and can be made unusable.
    struct Fake {
        usable: bool,
        kind: &'static str,
        held: Mutex<Option<StoredCredential>>,
    }

    impl Fake {
        fn boxed(usable: bool, kind: &'static str) -> Box<Self> {
            Box::new(Self {
                usable,
                kind,
                held: Mutex::new(None),
            })
        }
    }

    impl CredentialStore for Fake {
        fn load(&self, _: &str) -> Result<Option<StoredCredential>> {
            Ok(self.held.lock().unwrap().clone())
        }
        fn save(&self, _: &str, credential: &StoredCredential) -> Result<&'static str> {
            *self.held.lock().unwrap() = Some(credential.clone());
            Ok(self.kind)
        }
        fn delete(&self, _: &str) -> Result<()> {
            *self.held.lock().unwrap() = None;
            Ok(())
        }
        fn usable(&self) -> bool {
            self.usable
        }
    }

    #[test]
    fn the_automatic_store_uses_the_platform_store_when_it_works() {
        let store = AutoStore::new(Fake::boxed(true, KEYRING), Fake::boxed(true, FILE));
        assert_eq!(store.save("demo", &sample()).unwrap(), KEYRING);
        assert_eq!(store.load("demo").unwrap(), Some(sample()));
    }

    #[test]
    fn the_automatic_store_falls_back_to_the_file_when_there_is_no_usable_keyring() {
        let store = AutoStore::new(Fake::boxed(false, KEYRING), Fake::boxed(true, FILE));
        assert_eq!(store.save("demo", &sample()).unwrap(), FILE);
        assert_eq!(store.load("demo").unwrap(), Some(sample()));
        store.delete("demo").unwrap();
        assert_eq!(store.load("demo").unwrap(), None);
    }

    #[test]
    fn saving_to_the_platform_store_removes_an_older_copy_from_the_file() {
        let file = Fake::boxed(true, FILE);
        file.save("demo", &sample()).unwrap();
        let store = AutoStore::new(Fake::boxed(true, KEYRING), file);
        store.save("demo", &sample()).unwrap();
        store.delete("demo").unwrap();
        assert_eq!(
            store.load("demo").unwrap(),
            None,
            "a sign-out must not leave the file copy to be found"
        );
    }

    #[test]
    fn the_configured_backend_picks_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let config = CredentialsConfig {
            backend: CredentialBackend::File,
            dir: Some(dir.path().to_path_buf()),
        };
        let store = from_config(&config);
        assert_eq!(store.save("demo", &sample()).unwrap(), FILE);
        assert!(dir.path().join("demo.json").is_file());
    }
}
