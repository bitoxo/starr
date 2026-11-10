//! Jellyfin against the account writes contract (spec 0005 slice 4), on a
//! stateful fake seeded from the recorded 12.3.0 account fixtures.

use starr_adapter_jellyfin::{AUTH, DESCRIPTOR, Jellyfin};
use starr_adapter_kit::contract::account_writes::{WritesContract, run_writes_contract};
use starr_adapter_kit::contract::media_server::FakeFlavor;

#[tokio::test]
async fn jellyfin_passes_the_account_writes_contract() {
    run_writes_contract(&WritesContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        build: Jellyfin::new,
        flavor: FakeFlavor::Jellyfin,
        recorded_dir: format!("{}/fixtures/12.3.0/accounts", env!("CARGO_MANIFEST_DIR")),
        plain: "friend",
        admin: "admin",
        locked: Some("locked-friend"),
        // Movies, Shows (`ItemId`).
        libraries: [
            "f137a2dd21bbc1b99aa5c0f6bf02a805",
            "a656b907eb3a73532e40e44b968d0225",
        ],
    })
    .await;
}
