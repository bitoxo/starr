//! HTTP API under `/api`. Every handler is annotated with `#[utoipa::path]` so
//! the `OpenAPI` document (and the generated TypeScript types) stay in sync.

mod account_writes;
mod accounts;
mod admins;
mod attention;
mod auth;
mod catalog;
mod categories;
pub(crate) mod common;
mod discover;
pub mod error;
mod follows;
pub(crate) mod images;
mod invitations;
mod jobs;
mod journey;
mod libraries;
mod login_art;
pub(crate) use login_art::LoginArtMemo;
mod me;
mod members;
mod music;
mod my_services;
mod notifications;
mod password_tokens;
pub(crate) mod privacy;
pub(crate) mod private_catalog;
pub(crate) mod private_follows;
pub(crate) mod private_requests;
mod requests;
mod runs;
mod service_accounts;
mod services;
mod setup;
mod system;
mod title_facts;
mod totp;

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

#[derive(OpenApi)]
#[openapi(
    info(title = "Starr API"),
    tags(
        (name = "system", description = "Health and status"),
        (name = "setup", description = "First-run setup"),
        (name = "auth", description = "Login, logout and the current account"),
        (name = "runs", description = "Background job runs"),
        (name = "jobs", description = "Background jobs"),
        (name = "members", description = "Member administration"),
        (name = "invitations", description = "Invitation links"),
        (name = "me", description = "The logged-in member's profile and sessions"),
        (name = "admins", description = "Admin accounts"),
        (name = "services", description = "Connected services (Radarr, …)"),
        (name = "images", description = "Posters and backdrops, proxied and cached by Starr"),
        (name = "categories", description = "Categories, their service roles and settings"),
        (name = "catalog", description = "Catalog reference data and previews; source lists"),
        (name = "requests", description = "Requests, approvals and acquisitions"),
        (name = "admin", description = "What needs an admin: the Inbox's attention model"),
        (name = "discover", description = "Browsing, search and title details for members"),
        (name = "notifications", description = "Notification streams, preferences and channels"),
        (name = "follows", description = "A member's follows of series (new seasons) and artists (new albums)"),
        (name = "accounts", description = "Accounts in consume services (Jellyfin, Emby): inventory, adoption, the account plan and account writes (plan only until an admin switches an instance live)"),
        (name = "libraries", description = "Libraries of consume services and which category they belong to"),
        (name = "private", description = "The private category's own area: lanes, search, pages, scene requests and performer or studio follows, only through the privacy gate"),
    )
)]
struct ApiDoc;

/// All API routes plus their `OpenAPI` description. Handler paths are written
/// in full (`/api/v1/...`) in their `#[utoipa::path]` attributes.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(system::health))
        .routes(routes!(system::status))
        .routes(routes!(setup::state))
        .routes(routes!(setup::create_admin))
        .routes(routes!(auth::login))
        .routes(routes!(auth::logout))
        .routes(routes!(auth::login_totp))
        .routes(routes!(auth::me))
        .routes(routes!(auth::put_linked_member))
        .routes(routes!(auth::switch))
        .routes(routes!(auth::elevate))
        .routes(routes!(privacy::set_private_unlock))
        .routes(routes!(totp::totp_status))
        .routes(routes!(totp::enroll))
        .routes(routes!(totp::confirm))
        .routes(routes!(totp::disable))
        .routes(routes!(totp::regenerate_recovery_codes))
        .routes(routes!(runs::list_runs))
        .routes(routes!(runs::get_run))
        .routes(routes!(jobs::trigger_job))
        .routes(routes!(members::list_members, members::create_member))
        .routes(routes!(members::get_member, members::delete_member))
        .routes(routes!(members::suspend_member))
        .routes(routes!(members::reactivate_member))
        .routes(routes!(members::reset_password))
        .routes(routes!(members::restore_member))
        .routes(routes!(members::put_entitlements))
        .routes(routes!(
            invitations::list_invitations,
            invitations::create_invitation
        ))
        .routes(routes!(invitations::revoke_invitation))
        .routes(routes!(invitations::preview_invitation))
        .routes(routes!(invitations::accept_invitation))
        .routes(routes!(me::get_profile, me::update_profile))
        .routes(routes!(me::change_password))
        .routes(routes!(me::list_sessions, me::revoke_all_sessions))
        .routes(routes!(me::revoke_session))
        .routes(routes!(my_services::get_my_services))
        .routes(routes!(my_services::activate))
        .routes(routes!(my_services::use_starr_password))
        .routes(routes!(services::list_service_types))
        .routes(routes!(services::list_services, services::create_service))
        .routes(routes!(
            services::get_service,
            services::update_service,
            services::delete_service
        ))
        .routes(routes!(services::check_service))
        .routes(routes!(services::set_writes_paused))
        .routes(routes!(services::held))
        .routes(routes!(images::image))
        .routes(routes!(catalog::catalog_reference))
        .routes(routes!(catalog::catalog_providers))
        .routes(routes!(catalog::catalog_preview))
        .routes(routes!(catalog::get_source_lists))
        .routes(routes!(catalog::refresh_source_lists))
        .routes(routes!(categories::list_categories))
        .routes(routes!(categories::update_category))
        .routes(routes!(categories::add_role))
        .routes(routes!(categories::remove_role))
        .routes(routes!(categories::set_default_catalog))
        .routes(routes!(categories::preview_source_switch))
        .routes(routes!(categories::set_source))
        .routes(routes!(categories::set_relevance))
        .routes(routes!(categories::set_follow_cap))
        .routes(routes!(categories::set_source_settings))
        .routes(routes!(categories::set_role_libraries))
        .routes(routes!(
            requests::list_my_requests,
            requests::create_request
        ))
        .routes(routes!(requests::withdraw_request))
        .routes(routes!(requests::cancel_request))
        .routes(routes!(requests::set_request_hidden))
        .routes(routes!(requests::request_status))
        .routes(routes!(requests::list_approvals))
        .routes(routes!(requests::approve_requests))
        .routes(routes!(requests::undo_approvals))
        .routes(routes!(requests::reject_requests))
        .routes(routes!(attention::admin_attention))
        .routes(routes!(requests::revoke_requests))
        .routes(routes!(requests::list_requests))
        .routes(routes!(requests::get_request))
        .routes(routes!(requests::list_acquisitions))
        .routes(routes!(requests::list_unlisted_albums))
        .routes(routes!(requests::retry_acquisition))
        .routes(routes!(follows::list_follows, follows::create_follow))
        .routes(routes!(follows::delete_follow))
        .merge(admin_routes())
        .merge(notification_routes())
        .merge(account_routes())
        .merge(discover_routes())
        .merge(sign_in_page_routes())
        .merge(private_routes())
}

/// Admin accounts and the signed-in admin's own sessions and password.
fn admin_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(admins::list_admins, admins::create_admin_account))
        .routes(routes!(admins::suspend_admin))
        .routes(routes!(admins::reactivate_admin))
        .routes(routes!(admins::remove_admin))
        .routes(routes!(admins::list_own_sessions))
        .routes(routes!(admins::revoke_own_session))
        .routes(routes!(admins::change_own_password))
}

/// Public routes of the sign-in pages: set-password links and the login
/// page's poster art (spec 0008 slice 11).
fn sign_in_page_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            password_tokens::check_password_token,
            password_tokens::use_password_token
        ))
        .routes(routes!(login_art::login_art))
        .routes(routes!(login_art::login_art_image))
}

/// Discover, search and title details for members (`discover`).
fn discover_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(discover::discover_categories))
        .routes(routes!(discover::discover_highlights))
        .routes(routes!(discover::discover_list))
        .routes(routes!(discover::search))
        .routes(routes!(discover::title_details))
        .routes(routes!(discover::season_episodes))
        .routes(routes!(music::artist_details))
        .routes(routes!(music::album_details))
}

/// The private category's own area (`private`, spec 0007 slice 2).
fn private_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(private_catalog::private_area))
        .routes(routes!(private_catalog::private_lane))
        .routes(routes!(private_catalog::private_search))
        .routes(routes!(private_catalog::private_scene))
        .routes(routes!(private_catalog::private_performer))
        .routes(routes!(private_catalog::private_studio))
        .routes(routes!(
            private_requests::create_private_requests,
            private_requests::list_private_requests
        ))
        .routes(routes!(
            private_follows::list_private_follows,
            private_follows::create_private_follow
        ))
        .routes(routes!(private_follows::restore_private_follow))
}

/// Accounts in consume services, adoption, the account plan and libraries
/// (`accounts`, `libraries`).
fn account_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(accounts::list_accounts))
        .routes(routes!(accounts::set_ignored))
        .routes(routes!(service_accounts::account_matrix))
        .routes(routes!(service_accounts::decide_service_account))
        .routes(routes!(service_accounts::confirm_suggestions))
        .routes(routes!(service_accounts::service_account_plan))
        .routes(routes!(service_accounts::member_account_plan))
        .routes(routes!(account_writes::confirm_plan))
        .routes(routes!(account_writes::pause_writes))
        .routes(routes!(account_writes::attention))
        .routes(routes!(libraries::list_service_libraries))
        .routes(routes!(libraries::set_library_ignored))
        .routes(routes!(libraries::list_unmapped_libraries))
        .routes(routes!(libraries::member_libraries))
}

/// Streams, preferences and channels (`notifications`).
fn notification_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(notifications::list_my_notifications))
        .routes(routes!(notifications::mark_my_notifications_read))
        .routes(routes!(
            notifications::get_notification_preferences,
            notifications::put_notification_preferences
        ))
        .routes(routes!(notifications::list_admin_notifications))
        .routes(routes!(notifications::mark_admin_notifications_read))
        .routes(routes!(
            notifications::list_channels,
            notifications::create_channel
        ))
        .routes(routes!(
            notifications::update_channel,
            notifications::delete_channel
        ))
        .routes(routes!(notifications::test_channel))
}
