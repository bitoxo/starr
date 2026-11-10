//! Emby adapter (ADR 0009): [`Probe`] and [`Library`] (read-only
//! availability and "open in" links, spec 0003 O3) and [`Accounts`] (spec
//! 0005: read half slice 1, writes slice 4 via
//! [`mediabrowser::writes`]).
//!
//! Facts used here and their evidence: `docs/concept/capabilities.md`,
//! section "Emby".

use std::time::Duration;

use secrecy::SecretString;
use starr_adapter_kit::mediabrowser::writes::{self, Flavor};
use starr_adapter_kit::mediabrowser::{
    self, GrantKey, LibraryContents, LibraryUser, Lockout, SystemInfo, VirtualFolder,
};
use starr_adapter_kit::{AuthScheme, ServiceClient};
use starr_core::accounts::{
    AccountRules, Accounts, CreateOutcome, DeletePlan, DisableMode, NameMatching, NewAccount,
    NonEmpty, RemoteAccount, VisibilityGranularity,
};
use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, Capability, IdentityRule, Listing, Probe,
    ProbeReport, RateLimit, SecretClass, Version, VersionRange,
};
use starr_core::library::{ItemLink, Library, MediaItem, MediaLibrary, encode_component};

/// Emby takes the API key in `X-Emby-Token`.
pub const AUTH: AuthScheme = AuthScheme::ApiKeyHeader("X-Emby-Token");

/// Cheapest authenticated call that reports version and server ID.
const INFO_PATH: &str = "System/Info";

/// Emby's `LockedOutDate` does not tell whether a lock is still on
/// (`capabilities.md`).
const LOCKOUT: Lockout = Lockout::NotReadable;

pub const DESCRIPTOR: AdapterDescriptor = AdapterDescriptor {
    type_id: "emby",
    display_name: "Emby",
    capabilities: &[Capability::Libraries, Capability::Accounts],
    // Verified 4.10.0.40 (libraries, inventory) and 4.10.1.0 (accounts).
    tested_versions: VersionRange {
        min: Version::new(4, 10, 0, 0),
        max_exclusive: Version::new(4, 11, 0, 0),
    },
    budgets: Budgets {
        // Emby documents no rate limit; item queries are database work on
        // small home servers.
        rate_limit: RateLimit {
            requests: 10,
            per: Duration::from_secs(1),
        },
        login_limit: None,
        expensive_endpoints: &["Users/{id}/Items"],
    },
    identity: IdentityRule::RemoteId,
    // An admin API key: opened only by the worker (ADR 0003).
    secret_class: SecretClass::ServiceCredential,
    lists: &[],
};

#[derive(Debug)]
pub struct Emby {
    client: ServiceClient,
    user: LibraryUser,
    contents: LibraryContents,
}

impl Emby {
    #[must_use]
    pub fn new(client: ServiceClient) -> Self {
        Self {
            client,
            user: LibraryUser::default(),
            contents: LibraryContents::default(),
        }
    }

    async fn info(&self) -> Result<SystemInfo, AdapterError> {
        let info: SystemInfo = self.client.get_json(INFO_PATH).await?;
        // Jellyfin answers the same path with the same shape plus a product
        // name; Emby's `SystemInfo` has none.
        if let Some(product) = &info.product_name {
            return Err(AdapterError::Unknown(format!(
                "the service at this URL is {:?}, not Emby",
                self.client.redactor().redact(product)
            )));
        }
        Ok(info)
    }
}

impl Probe for Emby {
    fn descriptor(&self) -> &'static AdapterDescriptor {
        &DESCRIPTOR
    }

    async fn probe(&self) -> Result<ProbeReport, AdapterError> {
        let info = self.info().await?;
        let version: Version = info.version.parse().map_err(|_| {
            AdapterError::Unknown(format!(
                "unparseable version {:?}",
                self.client.redactor().redact(&info.version)
            ))
        })?;
        DESCRIPTOR.check_version(version)?;
        Ok(ProbeReport { version })
    }
}

impl Library for Emby {
    async fn server_id(&self) -> Result<Option<String>, AdapterError> {
        Ok(Some(self.info().await?.id))
    }

    async fn libraries(&self) -> Result<Listing<MediaLibrary>, AdapterError> {
        let folders: Vec<VirtualFolder> = self.client.get_json("Library/VirtualFolders").await?;
        let listing = mediabrowser::libraries(folders, GrantKey::Guid);
        self.contents.remember(&listing);
        Ok(listing)
    }

    /// Albums of a music library, else movies and series (a library this
    /// adapter has not listed counts as one of movies and series).
    async fn items(&self, library_id: &str) -> Result<Listing<MediaItem>, AdapterError> {
        let user = self.user.get(&self.client).await?;
        let path = format!("Users/{}/Items", encode_component(&user));
        mediabrowser::library_items(&self.client, &path, library_id, &[], &self.contents).await
    }

    /// `…/web/index.html#!/item?id=<id>&serverId=<serverId>`, the item page
    /// of Emby's web client (`capabilities.md`, Emby, links).
    fn item_url(link: &ItemLink<'_>) -> String {
        let mut url = format!(
            "{}/web/index.html#!/item?id={}",
            link.public_url.trim_end_matches('/'),
            encode_component(link.remote_id)
        );
        if let Some(server) = link.server_id {
            url.push_str("&serverId=");
            url.push_str(&encode_component(server));
        }
        url
    }
}

/// What Emby accounts can do (`capabilities.md`, Emby).
pub const ACCOUNT_RULES: AccountRules = AccountRules {
    disable: DisableMode::Flag,
    // `DELETE /Users/{id}`, only for a due deletion order (D16).
    delete: true,
    visibility: VisibilityGranularity::PerLibrary,
    username_matching: NameMatching::CaseInsensitive,
    // Every password change revokes the account's tokens.
    password_change_signs_out: true,
};

const FLAVOR: Flavor = Flavor::Emby;

impl Accounts for Emby {
    fn account_rules(&self) -> &'static AccountRules {
        &ACCOUNT_RULES
    }

    async fn create_account(&self, new: &NewAccount<'_>) -> Result<CreateOutcome, AdapterError> {
        writes::create_account(&self.client, FLAVOR, new).await
    }

    async fn set_enabled(
        &self,
        remote_id: &str,
        enabled: bool,
    ) -> Result<RemoteAccount, AdapterError> {
        writes::set_enabled(&self.client, FLAVOR, remote_id, enabled).await
    }

    async fn set_libraries(
        &self,
        remote_id: &str,
        libraries: &NonEmpty<String>,
    ) -> Result<RemoteAccount, AdapterError> {
        writes::set_libraries(&self.client, FLAVOR, remote_id, libraries).await
    }

    async fn set_password(
        &self,
        remote_id: &str,
        password: &SecretString,
    ) -> Result<(), AdapterError> {
        writes::set_password(&self.client, FLAVOR, remote_id, password).await
    }

    async fn delete_account(&self, remote_id: &str, plan: &DeletePlan) -> Result<(), AdapterError> {
        writes::delete_account(&self.client, FLAVOR, remote_id, plan).await
    }

    async fn accounts(&self) -> Result<Listing<RemoteAccount>, AdapterError> {
        mediabrowser::list_accounts(&self.client, LOCKOUT).await
    }

    async fn account(&self, remote_id: &str) -> Result<RemoteAccount, AdapterError> {
        mediabrowser::read_account(&self.client, remote_id, LOCKOUT).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_links_point_at_the_web_client() {
        let link = ItemLink {
            public_url: "https://emby.example.com",
            server_id: Some("e5f6"),
            remote_id: "68071",
        };
        assert_eq!(
            Emby::item_url(&link),
            "https://emby.example.com/web/index.html#!/item?id=68071&serverId=e5f6"
        );
        let link = ItemLink {
            server_id: None,
            ..link
        };
        assert_eq!(
            Emby::item_url(&link),
            "https://emby.example.com/web/index.html#!/item?id=68071"
        );
    }
}
