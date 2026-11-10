//! Sealed secrets (ADR 0003): values are encrypted with `age` (X25519) to a
//! recipient (public key). Sealing needs only the recipient, opening needs the
//! identity (private key) from the key file.
//!
//! Every key has a [`Purpose`]; a value sealed for one purpose can only be
//! opened with a key of the same purpose, so the type system keeps service
//! credentials and TOTP seeds apart (they use different keys, ADR 0003).
//!
//! Who may hold which half is decided by the crates that use this one:
//! the web side gets a [`Sealer`] only; the [`SecretKey`] for
//! [`ServiceCredentials`] is wrapped by `starr-worker` so only worker code
//! can open credentials. Catalog read tokens ([`CatalogTokens`], ADR 0003
//! amendment 1) are sealed to the web-held key, like TOTP seeds, and
//! opened through the narrow [`CatalogTokenKey`].

use std::fmt;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::x25519;
use secrecy::{ExposeSecret, SecretString};

/// What a key protects. Sealed: only the purposes below exist.
pub trait Purpose: private::Sealed + Send + Sync + 'static {
    /// Short name for messages.
    const NAME: &'static str;
}

mod private {
    pub trait Sealed {}
}

/// Credentials Starr uses to talk to services (API keys). Opened only by
/// worker code.
#[derive(Debug)]
pub enum ServiceCredentials {}

/// TOTP seeds of admin accounts. Opened by the web process's auth module.
#[derive(Debug)]
pub enum TotpSecrets {}

/// Read-only catalog tokens (TMDB, …). Sealed to the web-held key, so the
/// web process can call the catalog (ADR 0003 amendment 1).
#[derive(Debug)]
pub enum CatalogTokens {}

impl private::Sealed for ServiceCredentials {}
impl private::Sealed for TotpSecrets {}
impl private::Sealed for CatalogTokens {}
impl Purpose for CatalogTokens {
    const NAME: &'static str = "catalog read tokens";
}
impl Purpose for ServiceCredentials {
    const NAME: &'static str = "service credentials";
}
impl Purpose for TotpSecrets {
    const NAME: &'static str = "TOTP secrets";
}

/// Seals service credentials (web side).
pub type CredentialSealer = Sealer<ServiceCredentials>;
/// A sealed service credential as stored in the database.
pub type SealedCredential = SealedSecret<ServiceCredentials>;
/// Seals catalog read tokens (needs only the web key's recipient).
pub type CatalogTokenSealer = Sealer<CatalogTokens>;

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("cannot read or write the key file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "the key file {path} is writable by other users (mode {mode:o}); restrict it with `chmod 600` (or 400/440/444 for a read-only mount)"
    )]
    Permissions { path: PathBuf, mode: u32 },
    #[error("the key file {0} does not contain a valid age identity")]
    MalformedKey(PathBuf),
    #[error("not a valid age recipient")]
    InvalidRecipient,
    #[error("sealing failed")]
    Seal,
    #[error("this value was sealed for recipient {found}, not for this key")]
    WrongKey { found: String },
    #[error("the sealed value cannot be opened (corrupt or not for this key)")]
    Open,
}

/// The public half: seals values for one recipient.
pub struct Sealer<P: Purpose> {
    recipient: x25519::Recipient,
    id: String,
    purpose: PhantomData<P>,
}

impl<P: Purpose> Clone for Sealer<P> {
    fn clone(&self) -> Self {
        Self {
            recipient: self.recipient.clone(),
            id: self.id.clone(),
            purpose: PhantomData,
        }
    }
}

impl<P: Purpose> fmt::Debug for Sealer<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sealer")
            .field("purpose", &P::NAME)
            .field("recipient", &self.id)
            .finish()
    }
}

impl<P: Purpose> Sealer<P> {
    /// From a recipient string (`age1…`), e.g. as stored in `app_meta`.
    pub fn parse(recipient: &str) -> Result<Self, SecretsError> {
        let parsed =
            x25519::Recipient::from_str(recipient).map_err(|_| SecretsError::InvalidRecipient)?;
        Ok(Self {
            id: parsed.to_string(),
            recipient: parsed,
            purpose: PhantomData,
        })
    }

    /// The recipient string (`age1…`).
    #[must_use]
    pub fn recipient(&self) -> &str {
        &self.id
    }

    pub fn seal(&self, plaintext: &SecretString) -> Result<SealedSecret<P>, SecretsError> {
        let ciphertext = age::encrypt(&self.recipient, plaintext.expose_secret().as_bytes())
            .map_err(|_| SecretsError::Seal)?;
        Ok(SealedSecret {
            recipient: self.id.clone(),
            ciphertext,
            purpose: PhantomData,
        })
    }
}

/// An encrypted value plus the recipient it was sealed for (age's X25519
/// header does not name the recipient, so it is kept alongside).
pub struct SealedSecret<P: Purpose> {
    recipient: String,
    ciphertext: Vec<u8>,
    purpose: PhantomData<P>,
}

impl<P: Purpose> fmt::Debug for SealedSecret<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedSecret")
            .field("purpose", &P::NAME)
            .field("recipient", &self.recipient)
            .field(
                "ciphertext",
                &format_args!("<{} bytes>", self.ciphertext.len()),
            )
            .finish()
    }
}

impl<P: Purpose> SealedSecret<P> {
    /// Rebuild from stored parts.
    #[must_use]
    pub fn from_parts(recipient: String, ciphertext: Vec<u8>) -> Self {
        Self {
            recipient,
            ciphertext,
            purpose: PhantomData,
        }
    }

    #[must_use]
    pub fn recipient(&self) -> &str {
        &self.recipient
    }

    #[must_use]
    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }

    /// Split into stored parts.
    #[must_use]
    pub fn into_parts(self) -> (String, Vec<u8>) {
        (self.recipient, self.ciphertext)
    }
}

/// The private half: opens values sealed for its recipient.
pub struct SecretKey<P: Purpose> {
    identity: x25519::Identity,
    sealer: Sealer<P>,
}

impl<P: Purpose> fmt::Debug for SecretKey<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretKey")
            .field("purpose", &P::NAME)
            .field("recipient", &self.sealer.id)
            .finish_non_exhaustive()
    }
}

/// Whether [`SecretKey::load_or_create`] found the file or wrote a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOrigin {
    /// An existing file. `shared_read`: group or others may read it (e.g. a
    /// read-only Docker secret); allowed, worth a note.
    Loaded {
        shared_read: bool,
    },
    Created,
}

impl<P: Purpose> SecretKey<P> {
    /// A fresh key that lives only in memory (tests, first start).
    #[must_use]
    pub fn generate() -> Self {
        Self::from_identity(x25519::Identity::generate())
    }

    fn from_identity(identity: x25519::Identity) -> Self {
        let recipient = identity.to_public();
        let sealer = Sealer {
            id: recipient.to_string(),
            recipient,
            purpose: PhantomData,
        };
        Self { identity, sealer }
    }

    /// Load the key file at `path`, or create it (mode 0600) if it does not
    /// exist. Refuses a file that group or others may write (anyone could
    /// swap the key), and a file that cannot be read or parsed. Read-only
    /// files (0400, 0440, 0444, e.g. Docker secrets) are fine.
    pub fn load_or_create(path: &Path) -> Result<(Self, KeyOrigin), SecretsError> {
        let io = |source| SecretsError::Io {
            path: path.to_path_buf(),
            source,
        };
        match Self::load(path) {
            Ok(loaded) => Ok(loaded),
            Err(SecretsError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                let key = Self::generate();
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
                let mut file = match options.open(path) {
                    Ok(file) => file,
                    // Another process created it first (split mode): use theirs.
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Self::load(path);
                    }
                    Err(err) => return Err(io(err)),
                };
                let encoded = key.identity.to_string();
                file.write_all(encoded.expose_secret().as_bytes())
                    .and_then(|()| file.write_all(b"\n"))
                    .and_then(|()| file.sync_all())
                    .map_err(io)?;
                Ok((key, KeyOrigin::Created))
            }
            Err(err) => Err(err),
        }
    }

    fn load(path: &Path) -> Result<(Self, KeyOrigin), SecretsError> {
        let io = |source| SecretsError::Io {
            path: path.to_path_buf(),
            source,
        };
        let mut file = std::fs::File::open(path).map_err(io)?;
        #[cfg(unix)]
        let shared_read = {
            use std::os::unix::fs::PermissionsExt;
            let mode = file.metadata().map_err(io)?.permissions().mode() & 0o777;
            if mode & 0o022 != 0 {
                return Err(SecretsError::Permissions {
                    path: path.to_path_buf(),
                    mode,
                });
            }
            mode & 0o044 != 0
        };
        #[cfg(not(unix))]
        let shared_read = false;
        let mut raw = String::new();
        file.read_to_string(&mut raw).map_err(io)?;
        let raw = SecretString::from(raw);
        let identity = x25519::Identity::from_str(raw.expose_secret().trim())
            .map_err(|_| SecretsError::MalformedKey(path.to_path_buf()))?;
        Ok((
            Self::from_identity(identity),
            KeyOrigin::Loaded { shared_read },
        ))
    }

    /// The sealer for this key's recipient.
    #[must_use]
    pub fn sealer(&self) -> &Sealer<P> {
        &self.sealer
    }

    /// The recipient string (`age1…`).
    #[must_use]
    pub fn recipient(&self) -> &str {
        &self.sealer.id
    }

    /// Decrypt a value sealed for this key.
    pub fn open(&self, sealed: &SealedSecret<P>) -> Result<SecretString, SecretsError> {
        self.open_stored(&sealed.recipient, &sealed.ciphertext)
    }

    /// [`SecretKey::open`] on stored parts, without building a
    /// [`SealedSecret`] first.
    pub fn open_stored(
        &self,
        recipient: &str,
        ciphertext: &[u8],
    ) -> Result<SecretString, SecretsError> {
        if recipient != self.sealer.id {
            return Err(SecretsError::WrongKey {
                found: recipient.to_owned(),
            });
        }
        let plain = age::decrypt(&self.identity, ciphertext).map_err(|_| SecretsError::Open)?;
        let text = String::from_utf8(plain).map_err(|_| SecretsError::Open)?;
        Ok(SecretString::from(text))
    }
}

/// The web process's key for TOTP seeds (ADR 0003: "sealed with a separate
/// key held by the web process"). A narrow wrapper so the server crate can
/// hold it without ever naming [`SecretKey`], which keeps the guard against
/// it touching [`ServiceCredentials`] keys simple.
#[derive(Debug)]
pub struct TotpKey(SecretKey<TotpSecrets>);

impl TotpKey {
    /// Load the key file at `path` or create it (mode 0600).
    pub fn load_or_create(path: &Path) -> Result<Self, SecretsError> {
        SecretKey::load_or_create(path).map(|(key, _)| Self(key))
    }

    /// A key that lives only in memory (tests).
    #[must_use]
    pub fn generate() -> Self {
        Self(SecretKey::generate())
    }

    #[must_use]
    pub fn recipient(&self) -> &str {
        self.0.recipient()
    }

    pub fn seal(
        &self,
        plaintext: &SecretString,
    ) -> Result<SealedSecret<TotpSecrets>, SecretsError> {
        self.0.sealer().seal(plaintext)
    }

    pub fn open(&self, sealed: &SealedSecret<TotpSecrets>) -> Result<SecretString, SecretsError> {
        self.0.open(sealed)
    }
}

/// The web-held key (`web.key`) in its role for catalog read tokens. Same
/// file as [`TotpKey`], separate type: it opens [`CatalogTokens`] only, so
/// holding it never reaches a service credential.
#[derive(Debug)]
pub struct CatalogTokenKey(SecretKey<CatalogTokens>);

impl CatalogTokenKey {
    /// Load the key file at `path` or create it (mode 0600).
    pub fn load_or_create(path: &Path) -> Result<Self, SecretsError> {
        SecretKey::load_or_create(path).map(|(key, _)| Self(key))
    }

    /// Load the key file at `path` if it exists; `Ok(None)` if it does not
    /// (a worker in split mode without the web key mounted). Never creates
    /// it: a second, different key would make every token unreadable.
    pub fn load_existing(path: &Path) -> Result<Option<Self>, SecretsError> {
        match SecretKey::load(path) {
            Ok((key, _)) => Ok(Some(Self(key))),
            Err(SecretsError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    /// A key that lives only in memory (tests, and the default before the
    /// key file is installed).
    #[must_use]
    pub fn generate() -> Self {
        Self(SecretKey::generate())
    }

    #[must_use]
    pub fn recipient(&self) -> &str {
        self.0.recipient()
    }

    #[must_use]
    pub fn sealer(&self) -> &CatalogTokenSealer {
        self.0.sealer()
    }

    /// Decrypt a stored catalog token (recipient plus ciphertext).
    pub fn open_token(
        &self,
        recipient: &str,
        ciphertext: &[u8],
    ) -> Result<SecretString, SecretsError> {
        self.0.open_stored(recipient, ciphertext)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "starr-secrets-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn secret(s: &str) -> SecretString {
        SecretString::from(s.to_owned())
    }

    #[test]
    fn sealing_needs_only_the_recipient_and_opening_round_trips() {
        let key = SecretKey::<ServiceCredentials>::generate();
        let public = CredentialSealer::parse(key.recipient()).unwrap();
        let sealed = public.seal(&secret("radarr-api-key")).unwrap();
        assert_eq!(sealed.recipient(), key.recipient());
        assert!(
            !sealed
                .ciphertext()
                .windows(14)
                .any(|w| w == b"radarr-api-key")
        );
        assert_eq!(key.open(&sealed).unwrap().expose_secret(), "radarr-api-key");
    }

    #[test]
    fn another_key_cannot_open() {
        let key = SecretKey::<ServiceCredentials>::generate();
        let other = SecretKey::<ServiceCredentials>::generate();
        let sealed = key.sealer().seal(&secret("x")).unwrap();
        assert!(matches!(
            other.open(&sealed),
            Err(SecretsError::WrongKey { .. })
        ));
        // Lying about the recipient does not help either.
        let (_, ciphertext) = sealed.into_parts();
        let forged = SealedSecret::from_parts(other.recipient().to_owned(), ciphertext);
        assert!(matches!(other.open(&forged), Err(SecretsError::Open)));
    }

    #[test]
    fn debug_output_never_shows_key_material() {
        let key = SecretKey::<ServiceCredentials>::generate();
        let sealed = key.sealer().seal(&secret("plaintext-value")).unwrap();
        let printed = format!("{key:?} {sealed:?} {:?}", key.sealer());
        assert!(!printed.contains("AGE-SECRET-KEY"), "{printed}");
        assert!(!printed.contains("plaintext-value"), "{printed}");
    }

    #[test]
    fn key_file_is_created_once_with_mode_0600_and_reloaded() {
        let dir = TempDir::new();
        let path = dir.0.join("secret.key");
        let (created, origin) = SecretKey::<ServiceCredentials>::load_or_create(&path).unwrap();
        assert_eq!(origin, KeyOrigin::Created);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let (loaded, origin) = SecretKey::<ServiceCredentials>::load_or_create(&path).unwrap();
        assert_eq!(origin, KeyOrigin::Loaded { shared_read: false });
        assert_eq!(loaded.recipient(), created.recipient());
        let sealed = created.sealer().seal(&secret("v")).unwrap();
        assert_eq!(loaded.open(&sealed).unwrap().expose_secret(), "v");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_key_files_are_accepted_writable_by_others_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new();
        let path = dir.0.join("secret.key");
        let (created, _) = SecretKey::<ServiceCredentials>::load_or_create(&path).unwrap();
        let set = |mode| std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode));

        for (mode, shared) in [(0o400, false), (0o440, true), (0o444, true), (0o640, true)] {
            set(mode).unwrap();
            let (key, origin) = SecretKey::<ServiceCredentials>::load_or_create(&path)
                .unwrap_or_else(|e| panic!("{mode:o}: {e}"));
            assert_eq!(
                origin,
                KeyOrigin::Loaded {
                    shared_read: shared
                },
                "{mode:o}"
            );
            assert_eq!(key.recipient(), created.recipient());
        }
        for mode in [0o620, 0o602, 0o666] {
            set(mode).unwrap();
            let err = SecretKey::<ServiceCredentials>::load_or_create(&path).unwrap_err();
            assert!(
                matches!(err, SecretsError::Permissions { mode: m, .. } if m == mode),
                "{mode:o}: {err}"
            );
        }
    }

    #[test]
    fn malformed_or_unreadable_key_file_is_refused() {
        let dir = TempDir::new();
        let path = dir.0.join("secret.key");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(&path).unwrap().write_all(b"garbage").unwrap();
        assert!(matches!(
            SecretKey::<ServiceCredentials>::load_or_create(&path),
            Err(SecretsError::MalformedKey(_))
        ));
        // A directory in place of the file: unreadable, not silently replaced.
        let dir_path = dir.0.join("dir.key");
        std::fs::create_dir(&dir_path).unwrap();
        assert!(SecretKey::<ServiceCredentials>::load_or_create(&dir_path).is_err());
        assert!(dir_path.is_dir());
    }

    #[test]
    fn catalog_tokens_open_with_the_web_key_only() {
        let dir = TempDir::new();
        let path = dir.0.join("web.key");
        assert!(CatalogTokenKey::load_existing(&path).unwrap().is_none());
        let totp = TotpKey::load_or_create(&path).unwrap();
        // Same file, same identity, other purpose.
        let catalog = CatalogTokenKey::load_existing(&path).unwrap().unwrap();
        assert_eq!(catalog.recipient(), totp.recipient());
        let public = CatalogTokenSealer::parse(catalog.recipient()).unwrap();
        let (recipient, ciphertext) = public.seal(&secret("tmdb-token")).unwrap().into_parts();
        assert_eq!(
            catalog
                .open_token(&recipient, &ciphertext)
                .unwrap()
                .expose_secret(),
            "tmdb-token"
        );
        // A service credential is sealed for the worker's key: the web
        // key refuses it before trying to decrypt.
        let worker = SecretKey::<ServiceCredentials>::generate();
        let (recipient, ciphertext) = worker
            .sealer()
            .seal(&secret("radarr"))
            .unwrap()
            .into_parts();
        assert!(matches!(
            catalog.open_token(&recipient, &ciphertext),
            Err(SecretsError::WrongKey { .. })
        ));
    }

    #[test]
    fn invalid_recipient_is_rejected() {
        assert!(matches!(
            CredentialSealer::parse("age1nope"),
            Err(SecretsError::InvalidRecipient)
        ));
    }
}
