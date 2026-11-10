//! The adapters this build knows: the single registry from adapter type id
//! to descriptor, auth scheme and capability implementations.

use secrecy::SecretString;
use starr_adapter_kit::{AuthScheme, ClientOptions, ServiceClient};
use starr_core::accounts::{
    Accounts, CreateOutcome, DeletePlan, NewAccount, NonEmpty, RemoteAccount,
};
use starr_core::adapters::{AdapterDescriptor, AdapterError, Listing, Probe, ProbeReport};
use starr_core::library::{ItemLink, Library};
use starr_core::members::Category;
use starr_core::source::{
    AddOutcome, AddRequest, Lookup, MetadataProfile, MonitorScope, NaturalKey, Progress,
    QualityProfile, RootFolder, Source, WriteReport,
};

struct Entry {
    descriptor: &'static AdapterDescriptor,
    auth: AuthScheme,
    /// Categories the adapter can serve (Radarr movies, Sonarr series,
    /// Lidarr music; media servers every category they can hold a library
    /// of).
    categories: &'static [Category],
}

const REGISTRY: [Entry; 10] = [
    Entry {
        descriptor: &starr_adapter_radarr::DESCRIPTOR,
        auth: starr_adapter_radarr::AUTH,
        categories: &[Category::Movies],
    },
    Entry {
        descriptor: &starr_adapter_sonarr::DESCRIPTOR,
        auth: starr_adapter_sonarr::AUTH,
        categories: &[Category::Series],
    },
    Entry {
        descriptor: &starr_adapter_lidarr::DESCRIPTOR,
        auth: starr_adapter_lidarr::AUTH,
        categories: &[Category::Music],
    },
    Entry {
        descriptor: &starr_adapter_whisparr::DESCRIPTOR,
        auth: starr_adapter_whisparr::AUTH,
        categories: &[Category::Adult],
    },
    Entry {
        descriptor: &starr_adapter_tmdb::DESCRIPTOR,
        auth: starr_adapter_tmdb::AUTH,
        categories: &[Category::Movies, Category::Series],
    },
    Entry {
        descriptor: &starr_adapter_musicbrainz::DESCRIPTOR,
        auth: starr_adapter_musicbrainz::AUTH,
        categories: &[Category::Music],
    },
    Entry {
        descriptor: &starr_adapter_stashdb::DESCRIPTOR,
        auth: starr_adapter_stashdb::AUTH,
        categories: &[Category::Adult],
    },
    Entry {
        descriptor: &starr_adapter_jellyfin::DESCRIPTOR,
        auth: starr_adapter_jellyfin::AUTH,
        categories: &Category::ALL,
    },
    Entry {
        descriptor: &starr_adapter_emby::DESCRIPTOR,
        auth: starr_adapter_emby::AUTH,
        categories: &Category::ALL,
    },
    Entry {
        descriptor: &starr_adapter_navidrome::DESCRIPTOR,
        auth: starr_adapter_navidrome::AUTH,
        categories: &[Category::Music],
    },
];

/// Every adapter type, in display order.
pub fn descriptors() -> impl Iterator<Item = &'static AdapterDescriptor> {
    REGISTRY.iter().map(|e| e.descriptor)
}

/// The descriptor of `type_id`, if this build knows it.
#[must_use]
pub fn descriptor(type_id: &str) -> Option<&'static AdapterDescriptor> {
    descriptors().find(|d| d.type_id == type_id)
}

/// How adapter type `type_id` authenticates.
pub(crate) fn auth(type_id: &str) -> Option<AuthScheme> {
    REGISTRY
        .iter()
        .find(|e| e.descriptor.type_id == type_id)
        .map(|e| e.auth)
}

/// Whether adapter type `type_id` takes a credential (an API key or token);
/// `MusicBrainz` takes none. Unknown types count as taking one.
#[must_use]
pub fn needs_credential(type_id: &str) -> bool {
    auth(type_id) != Some(AuthScheme::None)
}

/// Whether adapter type `type_id` takes an account's username and password
/// instead of an API key (Navidrome, [`AuthScheme::Session`]).
#[must_use]
pub fn takes_login(type_id: &str) -> bool {
    matches!(auth(type_id), Some(AuthScheme::Session(_)))
}

/// The credential value of a login ([`takes_login`]), as it is sealed and
/// stored.
#[must_use]
pub fn login_credential(username: &str, password: &SecretString) -> SecretString {
    starr_adapter_kit::LoginCredential::encode(username, password)
}

/// The base URL most operators enter for adapter type `type_id`: the
/// public service's own (TMDB, `MusicBrainz`); `None` for self-hosted
/// services.
#[must_use]
pub fn default_base_url(type_id: &str) -> Option<&'static str> {
    match type_id {
        "tmdb" => Some(starr_adapter_tmdb::API_BASE),
        "musicbrainz" => Some(starr_adapter_musicbrainz::API_BASE),
        "stashdb" => Some(starr_adapter_stashdb::API_BASE),
        _ => None,
    }
}

/// How the client of an instance of `type_id` is built in a process that
/// holds `percent` of the host's budget for it: `MusicBrainz` and `StashDB`
/// split their per-host budget between web and worker in split mode and
/// read 503 as "slow down" (spec 0006 Q14, spec 0007 Q1); `StashDB` and
/// Whisparr hold private content (ADR 0014: no paths or service words in
/// logs and errors); every other adapter keeps its descriptor's budget
/// whole.
pub(crate) fn client_options(descriptor: &AdapterDescriptor, percent: u32) -> ClientOptions {
    match descriptor.type_id {
        "musicbrainz" => starr_adapter_musicbrainz::client_options(percent),
        "stashdb" => starr_adapter_stashdb::client_options(percent),
        "whisparr" => starr_adapter_whisparr::client_options(),
        _ => ClientOptions::of(descriptor),
    }
}

/// The categories adapter type `type_id` can serve (empty if unknown).
#[must_use]
pub fn categories(type_id: &str) -> &'static [Category] {
    REGISTRY
        .iter()
        .find(|e| e.descriptor.type_id == type_id)
        .map_or(&[], |e| e.categories)
}

/// Whether adapter type `type_id` can serve `category` (a Sonarr is no
/// source for movies, whatever its capabilities).
#[must_use]
pub fn serves(type_id: &str, category: Category) -> bool {
    categories(type_id).contains(&category)
}

/// What a category's source settings are picked from (D9, spec 0006 Q6).
#[derive(Debug)]
pub(crate) struct SourceLists {
    pub(crate) quality_profiles: Listing<QualityProfile>,
    pub(crate) root_folders: Listing<RootFolder>,
    /// Empty for sources without metadata profiles.
    pub(crate) metadata_profiles: Listing<MetadataProfile>,
}

/// Quality profiles, root folders and metadata profiles of a source;
/// `None` if adapter type `type_id` is no source.
pub(crate) async fn source_lists(
    type_id: &str,
    client: ServiceClient,
) -> Option<Result<SourceLists, AdapterError>> {
    async fn read(source: impl Source) -> Result<SourceLists, AdapterError> {
        Ok(SourceLists {
            quality_profiles: source.quality_profiles().await?,
            root_folders: source.root_folders().await?,
            metadata_profiles: source.metadata_profiles().await?,
        })
    }
    match type_id {
        "radarr" => Some(read(starr_adapter_radarr::Radarr::new(client)).await),
        "sonarr" => Some(read(starr_adapter_sonarr::Sonarr::new(client)).await),
        "lidarr" => Some(read(starr_adapter_lidarr::Lidarr::new(client)).await),
        "whisparr" => Some(read(starr_adapter_whisparr::Whisparr::new(client)).await),
        _ => None,
    }
}

/// A source adapter of one of the known types. [`Source`] is not object
/// safe (its methods return `impl Future`), so the worker dispatches here.
pub(crate) enum AnySource {
    Radarr(starr_adapter_radarr::Radarr),
    Sonarr(starr_adapter_sonarr::Sonarr),
    Lidarr(starr_adapter_lidarr::Lidarr),
    Whisparr(starr_adapter_whisparr::Whisparr),
}

impl AnySource {
    /// The source adapter of type `type_id`; `None` if it is no source.
    pub(crate) fn new(type_id: &str, client: ServiceClient) -> Option<Self> {
        match type_id {
            "radarr" => Some(Self::Radarr(starr_adapter_radarr::Radarr::new(client))),
            "sonarr" => Some(Self::Sonarr(starr_adapter_sonarr::Sonarr::new(client))),
            "lidarr" => Some(Self::Lidarr(starr_adapter_lidarr::Lidarr::new(client))),
            "whisparr" => Some(Self::Whisparr(starr_adapter_whisparr::Whisparr::new(
                client,
            ))),
            _ => None,
        }
    }

    pub(crate) async fn lookup(&self, key: NaturalKey) -> Result<Lookup, AdapterError> {
        match self {
            Self::Radarr(s) => s.lookup(key).await,
            Self::Sonarr(s) => s.lookup(key).await,
            Self::Lidarr(s) => s.lookup(key).await,
            Self::Whisparr(s) => s.lookup(key).await,
        }
    }

    pub(crate) async fn add(&self, request: &AddRequest) -> Result<AddOutcome, AdapterError> {
        match self {
            Self::Radarr(s) => s.add(request).await,
            Self::Sonarr(s) => s.add(request).await,
            Self::Lidarr(s) => s.add(request).await,
            Self::Whisparr(s) => s.add(request).await,
        }
    }

    pub(crate) async fn extend_monitoring(
        &self,
        key: NaturalKey,
        scope: &MonitorScope,
        search: bool,
    ) -> Result<WriteReport, AdapterError> {
        match self {
            Self::Radarr(s) => s.extend_monitoring(key, scope, search).await,
            Self::Sonarr(s) => s.extend_monitoring(key, scope, search).await,
            Self::Lidarr(s) => s.extend_monitoring(key, scope, search).await,
            Self::Whisparr(s) => s.extend_monitoring(key, scope, search).await,
        }
    }

    pub(crate) async fn stop_monitoring(
        &self,
        key: NaturalKey,
        scope: &MonitorScope,
    ) -> Result<WriteReport, AdapterError> {
        match self {
            Self::Radarr(s) => s.stop_monitoring(key, scope).await,
            Self::Sonarr(s) => s.stop_monitoring(key, scope).await,
            Self::Lidarr(s) => s.stop_monitoring(key, scope).await,
            Self::Whisparr(s) => s.stop_monitoring(key, scope).await,
        }
    }

    pub(crate) async fn progress(&self, key: NaturalKey) -> Result<Progress, AdapterError> {
        match self {
            Self::Radarr(s) => s.progress(key).await,
            Self::Sonarr(s) => s.progress(key).await,
            Self::Lidarr(s) => s.progress(key).await,
            Self::Whisparr(s) => s.progress(key).await,
        }
    }
}

/// Run the probe of adapter type `type_id` over `client`.
pub(crate) async fn probe(
    type_id: &str,
    client: ServiceClient,
) -> Result<ProbeReport, AdapterError> {
    match type_id {
        "radarr" => starr_adapter_radarr::Radarr::new(client).probe().await,
        "sonarr" => starr_adapter_sonarr::Sonarr::new(client).probe().await,
        "lidarr" => starr_adapter_lidarr::Lidarr::new(client).probe().await,
        "whisparr" => starr_adapter_whisparr::Whisparr::new(client).probe().await,
        "tmdb" => starr_adapter_tmdb::Tmdb::new(client).probe().await,
        "musicbrainz" => {
            starr_adapter_musicbrainz::MusicBrainz::new(client)
                .probe()
                .await
        }
        "stashdb" => starr_adapter_stashdb::StashDb::new(client).probe().await,
        "jellyfin" => starr_adapter_jellyfin::Jellyfin::new(client).probe().await,
        "emby" => starr_adapter_emby::Emby::new(client).probe().await,
        "navidrome" => {
            starr_adapter_navidrome::Navidrome::new(client)
                .probe()
                .await
        }
        other => Err(AdapterError::Unknown(format!(
            "adapter type {other:?} has no probe"
        ))),
    }
}

/// Whether sources of adapter type `type_id` have metadata profiles to pick
/// per category (Lidarr, spec 0006 Q6).
#[must_use]
pub fn has_metadata_profiles(type_id: &str) -> bool {
    type_id == starr_adapter_lidarr::DESCRIPTOR.type_id
}

/// Whether adapter type `type_id` implements [`Library`] (consume read).
#[must_use]
pub fn has_library(type_id: &str) -> bool {
    matches!(type_id, "jellyfin" | "emby" | "navidrome")
}

/// Whether adapter type `type_id` implements [`Accounts`] (`accounts.scan`
/// reads, `accounts.reconcile` writes).
#[must_use]
pub fn has_accounts(type_id: &str) -> bool {
    matches!(type_id, "jellyfin" | "emby" | "navidrome")
}

/// The static account rules of adapter type `type_id`; `None` if it has no
/// accounts (shown to members before a password switch, M2).
#[must_use]
pub fn account_rules(type_id: &str) -> Option<&'static starr_core::accounts::AccountRules> {
    match type_id {
        "jellyfin" => Some(&starr_adapter_jellyfin::ACCOUNT_RULES),
        "emby" => Some(&starr_adapter_emby::ACCOUNT_RULES),
        "navidrome" => Some(&starr_adapter_navidrome::ACCOUNT_RULES),
        _ => None,
    }
}

/// An [`Accounts`] adapter of one of the known types ([`Accounts`] is not
/// object safe, so the worker dispatches here). Only the writes a live run
/// or a password handoff executes are exposed (deletes only with a
/// `DeletePlan`).
pub(crate) enum AnyAccounts {
    Jellyfin(starr_adapter_jellyfin::Jellyfin),
    Emby(starr_adapter_emby::Emby),
    Navidrome(starr_adapter_navidrome::Navidrome),
}

impl AnyAccounts {
    /// The accounts adapter of type `type_id`; `None` if it has none.
    pub(crate) fn new(type_id: &str, client: ServiceClient) -> Option<Self> {
        match type_id {
            "jellyfin" => Some(Self::Jellyfin(starr_adapter_jellyfin::Jellyfin::new(
                client,
            ))),
            "emby" => Some(Self::Emby(starr_adapter_emby::Emby::new(client))),
            "navidrome" => Some(Self::Navidrome(starr_adapter_navidrome::Navidrome::new(
                client,
            ))),
            _ => None,
        }
    }

    /// The adapter's static account rules.
    pub(crate) fn rules(&self) -> &'static starr_core::accounts::AccountRules {
        match self {
            Self::Jellyfin(a) => a.account_rules(),
            Self::Emby(a) => a.account_rules(),
            Self::Navidrome(a) => a.account_rules(),
        }
    }

    pub(crate) async fn set_enabled(
        &self,
        remote_id: &str,
        enabled: bool,
    ) -> Result<RemoteAccount, AdapterError> {
        match self {
            Self::Jellyfin(a) => a.set_enabled(remote_id, enabled).await,
            Self::Emby(a) => a.set_enabled(remote_id, enabled).await,
            Self::Navidrome(a) => a.set_enabled(remote_id, enabled).await,
        }
    }

    pub(crate) async fn set_libraries(
        &self,
        remote_id: &str,
        libraries: &NonEmpty<String>,
    ) -> Result<RemoteAccount, AdapterError> {
        match self {
            Self::Jellyfin(a) => a.set_libraries(remote_id, libraries).await,
            Self::Emby(a) => a.set_libraries(remote_id, libraries).await,
            Self::Navidrome(a) => a.set_libraries(remote_id, libraries).await,
        }
    }

    pub(crate) async fn account(&self, remote_id: &str) -> Result<RemoteAccount, AdapterError> {
        match self {
            Self::Jellyfin(a) => a.account(remote_id).await,
            Self::Emby(a) => a.account(remote_id).await,
            Self::Navidrome(a) => a.account(remote_id).await,
        }
    }

    /// Only from a password handoff (`accounts.password`, ADR 0010).
    pub(crate) async fn set_password(
        &self,
        remote_id: &str,
        password: &SecretString,
    ) -> Result<(), AdapterError> {
        match self {
            Self::Jellyfin(a) => a.set_password(remote_id, password).await,
            Self::Emby(a) => a.set_password(remote_id, password).await,
            Self::Navidrome(a) => a.set_password(remote_id, password).await,
        }
    }

    /// Only with the planner's [`DeletePlan`] of a due deletion (D16).
    pub(crate) async fn delete_account(
        &self,
        remote_id: &str,
        plan: &DeletePlan,
    ) -> Result<(), AdapterError> {
        match self {
            Self::Jellyfin(a) => a.delete_account(remote_id, plan).await,
            Self::Emby(a) => a.delete_account(remote_id, plan).await,
            Self::Navidrome(a) => a.delete_account(remote_id, plan).await,
        }
    }

    /// Only from a password handoff (`accounts.password`, ADR 0010).
    pub(crate) async fn create_account(
        &self,
        new: &NewAccount<'_>,
    ) -> Result<CreateOutcome, AdapterError> {
        match self {
            Self::Jellyfin(a) => a.create_account(new).await,
            Self::Emby(a) => a.create_account(new).await,
            Self::Navidrome(a) => a.create_account(new).await,
        }
    }
}

/// The "open in" URL of an item in a service of type `type_id`; `None` for
/// types without [`Library`]. Pure: the web process calls it with stored
/// inventory data (public URL, server ID, remote ID).
#[must_use]
pub fn item_url(type_id: &str, link: &ItemLink<'_>) -> Option<String> {
    match type_id {
        "jellyfin" => Some(starr_adapter_jellyfin::Jellyfin::item_url(link)),
        "emby" => Some(starr_adapter_emby::Emby::item_url(link)),
        "navidrome" => Some(starr_adapter_navidrome::Navidrome::item_url(link)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_ids_are_unique_and_have_auth() {
        let ids: Vec<_> = descriptors().map(|d| d.type_id).collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(ids.len(), unique.len(), "{ids:?}");
        assert_ne!(ids, Vec::<&str>::new());
        for id in ids {
            assert!(auth(id).is_some(), "{id}");
            assert_eq!(descriptor(id).map(|d| d.type_id), Some(id));
        }
        assert!(serves("lidarr", Category::Music));
        assert!(!serves("lidarr", Category::Series));
        assert!(serves("radarr", Category::Movies));
        assert!(!serves("radarr", Category::Series));
        assert!(serves("sonarr", Category::Series));
        assert!(serves("tmdb", Category::Series));
        assert!(!serves("tmdb", Category::Music));
        assert!(serves("musicbrainz", Category::Music));
        assert!(!serves("musicbrainz", Category::Movies));
        assert!(!needs_credential("musicbrainz"));
        assert!(needs_credential("tmdb"));
        assert!(needs_credential("lidarr"));
        assert!(serves("navidrome", Category::Music));
        assert!(!serves("navidrome", Category::Movies));
        assert!(needs_credential("navidrome") && takes_login("navidrome"));
        assert!(!takes_login("jellyfin") && !takes_login("musicbrainz"));
        assert_eq!(
            default_base_url("musicbrainz"),
            Some("https://musicbrainz.org")
        );
        assert_eq!(default_base_url("radarr"), None);
    }

    #[test]
    fn only_musicbrainz_splits_its_budget_and_throttles_on_503() {
        let mb = client_options(&starr_adapter_musicbrainz::DESCRIPTOR, 70);
        assert!(mb.unavailable_is_throttle);
        assert!(mb.rate_limit.per > std::time::Duration::from_secs(1));
        let tmdb = client_options(&starr_adapter_tmdb::DESCRIPTOR, 70);
        assert_eq!(tmdb, ClientOptions::of(&starr_adapter_tmdb::DESCRIPTOR));
    }

    #[test]
    fn whisparr_serves_adult_only_and_its_client_keeps_private_content_out_of_logs() {
        assert!(serves("whisparr", Category::Adult));
        for other in [Category::Movies, Category::Series, Category::Music] {
            assert!(!serves("whisparr", other), "{other:?}");
        }
        assert!(needs_credential("whisparr") && !takes_login("whisparr"));
        assert!(!has_metadata_profiles("whisparr"));
        let whisparr = descriptor("whisparr").unwrap();
        // Every process builds it the same way, whatever its share.
        for percent in [30, 70, 100] {
            assert!(client_options(whisparr, percent).private_content);
        }
        // No other source is private.
        for other in ["radarr", "sonarr", "lidarr"] {
            assert!(!client_options(descriptor(other).unwrap(), 100).private_content);
        }
    }

    #[test]
    fn library_adapters_declare_libraries_and_build_links() {
        use starr_core::adapters::Capability;
        for d in descriptors() {
            assert_eq!(
                has_library(d.type_id),
                d.capabilities.contains(&Capability::Libraries),
                "{}",
                d.type_id
            );
            assert_eq!(
                has_accounts(d.type_id),
                d.capabilities.contains(&Capability::Accounts),
                "{}",
                d.type_id
            );
        }
        let link = ItemLink {
            public_url: "https://watch.example.com",
            server_id: Some("s"),
            remote_id: "1",
        };
        assert!(item_url("jellyfin", &link).is_some_and(|u| u.contains("/web/#/details?id=1")));
        assert!(item_url("emby", &link).is_some_and(|u| u.contains("#!/item?id=1")));
        assert_eq!(
            item_url("navidrome", &link).as_deref(),
            Some("https://watch.example.com/app/#/album/1/show")
        );
        assert_eq!(item_url("radarr", &link), None);
    }
}
