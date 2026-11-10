//! The service credential key (ADR 0003). Only worker code can open
//! service credentials: [`CredentialKey::open`] is `pub(crate)`, so other
//! crates (the web server in particular) can hold and pass the key on to
//! the worker but never decrypt with it. The web side seals with the
//! recipient stored in `app_meta` under [`RECIPIENT_META_KEY`].

use std::path::Path;

use secrecy::{ExposeSecret, SecretString};
use starr_core::adapters::SecretClass;
use starr_db::services::StoredCredential;
use starr_db::{Database, DbError};
use starr_secrets::{CatalogTokenSealer, KeyOrigin, SecretKey, SecretsError, ServiceCredentials};

use crate::adapters;
use crate::catalog::CATALOG_RECIPIENT_META_KEY;
use crate::services::CODE_CREDENTIALS_UNREADABLE;

/// The public half for the web side: seals service credentials, cannot open
/// them. Re-exported so the server crate needs no `starr-secrets` dependency
/// (and so cannot reach `SecretKey` without a visible manifest change).
pub use starr_secrets::CredentialSealer;

/// `app_meta` key holding the recipient (public key) for service credentials.
pub const RECIPIENT_META_KEY: &str = "service_credentials_recipient";

/// What [`CredentialKey::reseal_catalog_tokens`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResealReport {
    pub resealed: usize,
    /// Could not be opened with this key; marked `credentials_unreadable`.
    pub unreadable: usize,
    /// Left for later (no web recipient yet, or sealing failed).
    pub waiting: usize,
}

/// The identity that opens service credentials. Not `Clone`: there is one.
pub struct CredentialKey(SecretKey<ServiceCredentials>);

impl std::fmt::Debug for CredentialKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CredentialKey").field(&self.0).finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretsInitError {
    /// The key file exists but cannot be read or parsed, or others may
    /// write it: an operator error, so Starr refuses to start.
    #[error(transparent)]
    Key(#[from] SecretsError),
    #[error(transparent)]
    Db(#[from] DbError),
}

impl CredentialKey {
    /// [`CredentialKey::load`] followed by [`CredentialKey::publish`].
    pub async fn init(db: &Database, path: &Path) -> Result<Self, SecretsInitError> {
        let key = Self::load(path)?;
        key.publish(db).await?;
        Ok(key)
    }

    /// Load (or create) the key file. Touches no database, so a worker
    /// waiting for the worker lease can fail fast on a bad key file without
    /// disturbing the active worker.
    pub fn load(path: &Path) -> Result<Self, SecretsInitError> {
        let (key, origin) = SecretKey::<ServiceCredentials>::load_or_create(path)?;
        match origin {
            KeyOrigin::Created => tracing::info!(
                path = %path.display(),
                "created the secret key file; back it up together with starr.db"
            ),
            KeyOrigin::Loaded { shared_read: true } => tracing::info!(
                path = %path.display(),
                "the secret key file is readable by group or others (read-only mount?); fine as long as only Starr's host can read it"
            ),
            KeyOrigin::Loaded { shared_read: false } => {}
        }
        Ok(Self(key))
    }

    /// Publish this key's recipient for the web side. Called by the active
    /// worker only (it holds the worker lease), so two workers with
    /// different key files never take turns overwriting it.
    ///
    /// A lost or replaced key file is not fatal: Starr runs with the key it
    /// has (a new one if the file was missing), logs an error, and marks
    /// every service whose credential was sealed for another key as
    /// `credentials_unreadable`, so the admin re-enters those API keys.
    pub async fn publish(&self, db: &Database) -> Result<(), DbError> {
        let lost = db
            .mark_credentials_unreadable(
                SecretClass::ServiceCredential,
                self.0.recipient(),
                CODE_CREDENTIALS_UNREADABLE,
                chrono::Utc::now(),
            )
            .await?;
        if lost > 0 {
            tracing::error!(
                services = lost,
                "the secret key does not match the one stored service credentials were sealed with; those services need their API key re-entered. Restore secret.key from the backup of starr.db to avoid this"
            );
        }
        db.meta_set(RECIPIENT_META_KEY, self.0.recipient()).await
    }

    #[must_use]
    pub fn recipient(&self) -> &str {
        self.0.recipient()
    }

    /// ADR 0003 amendment 1: catalog tokens (TMDB) entered before the
    /// amendment are still sealed with this key. Re-seal each to the web
    /// key's recipient (published by the web side in `app_meta`) and mark
    /// those this key cannot open as unreadable. Does nothing until the web
    /// side published its recipient (split mode, web not started yet); runs
    /// again on the next lease acquisition and every housekeeping.
    pub async fn reseal_catalog_tokens(&self, db: &Database) -> Result<ResealReport, DbError> {
        let mut report = ResealReport::default();
        let catalog_types: Vec<&str> = adapters::descriptors()
            .filter(|d| d.secret_class == SecretClass::CatalogReadToken)
            .map(|d| d.type_id)
            .collect();
        let mut pending = Vec::new();
        for type_id in catalog_types {
            pending.extend(
                db.service_connections_of(type_id, SecretClass::ServiceCredential)
                    .await?,
            );
        }
        if pending.is_empty() {
            return Ok(report);
        }
        let Some(recipient) = db.meta_get(CATALOG_RECIPIENT_META_KEY).await? else {
            tracing::info!(
                services = pending.len(),
                "catalog tokens wait for the web key's recipient before they are re-sealed"
            );
            report.waiting = pending.len();
            return Ok(report);
        };
        let web = match CatalogTokenSealer::parse(&recipient) {
            Ok(web) => web,
            Err(err) => {
                tracing::error!(error = %err, "catalog token recipient in app_meta is malformed");
                report.waiting = pending.len();
                return Ok(report);
            }
        };
        for conn in pending {
            // A row the web side could have tampered with (adapter type
            // set to `tmdb` on a Radarr row) must not move a service key
            // to the web key: TMDB read tokens never have the 32-hex shape
            // of Radarr/Sonarr keys (the API refuses those for TMDB).
            let opened = self
                .open(&conn.credential)
                .ok()
                .filter(|plain| !starr_adapter_tmdb::is_hex_api_key(plain.expose_secret()));
            let resealing = opened.as_ref().map(|plain| web.seal(plain));
            match resealing {
                Some(Ok(for_web)) => {
                    let (recipient, ciphertext) = for_web.into_parts();
                    let resealed = StoredCredential {
                        ciphertext,
                        recipient,
                        class: SecretClass::CatalogReadToken,
                    };
                    if db.reseal_credential(&conn, &resealed).await? {
                        tracing::info!(service = conn.id, "catalog token re-sealed to the web key");
                        report.resealed += 1;
                    }
                }
                Some(Err(err)) => {
                    tracing::error!(service = conn.id, error = %err, "re-sealing a catalog token failed");
                    report.waiting += 1;
                }
                None => {
                    tracing::error!(
                        service = conn.id,
                        "a catalog token cannot be opened with this key, or does not look like one; re-enter it under Services"
                    );
                    db.mark_credential_unreadable(
                        conn.id,
                        CODE_CREDENTIALS_UNREADABLE,
                        chrono::Utc::now(),
                    )
                    .await?;
                    report.unreadable += 1;
                }
            }
        }
        Ok(report)
    }

    /// Decrypt a stored credential. Crate-private on purpose (ADR 0003).
    pub(crate) fn open(&self, stored: &StoredCredential) -> Result<SecretString, SecretsError> {
        self.0.open_stored(&stored.recipient, &stored.ciphertext)
    }

    /// An in-memory key for this crate's tests.
    #[cfg(test)]
    pub(crate) fn ephemeral() -> Self {
        Self(SecretKey::generate())
    }

    /// Open a password handoff of member `member_id` at password
    /// `generation` (ADR 0010). `None` if it was sealed for another key,
    /// another member or another generation, or does not open: the bound
    /// context makes a handoff useless anywhere else. Crate-private, like
    /// [`CredentialKey::open`].
    pub(crate) fn open_handoff(
        &self,
        member_id: i64,
        generation: i64,
        recipient: &str,
        sealed: &[u8],
    ) -> Option<SecretString> {
        let plain = self.0.open_stored(recipient, sealed).ok()?;
        let (context, password) = plain.expose_secret().split_once('\n')?;
        if context != handoff_context(member_id, generation) || password.is_empty() {
            return None;
        }
        Some(SecretString::from(password.to_owned()))
    }
}

/// The context a handoff is bound to (spec 0005 M10 4: "member id and
/// password generation as associated data"). `age` has no associated data,
/// so the context is the first line of the sealed plaintext and checked on
/// opening; `age` authenticates the whole ciphertext.
fn handoff_context(member_id: i64, generation: i64) -> String {
    format!("member:{member_id}:password:{generation}")
}

/// Seal member `member_id`'s password at `generation` to the worker's
/// recipient as a password handoff (web side, ADR 0010): the stored
/// recipient and ciphertext. The web side can seal but never open it.
pub fn seal_password_handoff(
    sealer: &CredentialSealer,
    member_id: i64,
    generation: i64,
    password: &SecretString,
) -> Result<(String, Vec<u8>), SecretsError> {
    let plain = SecretString::from(format!(
        "{}\n{}",
        handoff_context(member_id, generation),
        password.expose_secret()
    ));
    Ok(sealer.seal(&plain)?.into_parts())
}

#[cfg(test)]
mod handoff_tests {
    use super::*;

    #[test]
    fn a_handoff_opens_only_for_its_member_and_generation() {
        let key = CredentialKey::ephemeral();
        let sealer = CredentialSealer::parse(key.recipient()).unwrap();
        let password = SecretString::from("line one\nline two ✓".to_owned());
        let (recipient, ciphertext) = seal_password_handoff(&sealer, 7, 3, &password).unwrap();
        let opened = key.open_handoff(7, 3, &recipient, &ciphertext).unwrap();
        assert_eq!(opened.expose_secret(), "line one\nline two ✓");
        assert!(key.open_handoff(8, 3, &recipient, &ciphertext).is_none());
        assert!(key.open_handoff(7, 4, &recipient, &ciphertext).is_none());
        let other = CredentialKey::ephemeral();
        assert!(other.open_handoff(7, 3, &recipient, &ciphertext).is_none());
        let mut tampered = ciphertext.clone();
        if let Some(last) = tampered.last_mut() {
            *last ^= 1;
        }
        assert!(key.open_handoff(7, 3, &recipient, &tampered).is_none());
        assert!(
            !ciphertext.windows(8).any(|w| w == b"line one"),
            "sealed, not plain"
        );
    }
}
