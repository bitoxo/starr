//! Emby against the account writes contract (spec 0005 slice 4), on a
//! stateful fake seeded from the recorded 4.10.1.0 account fixtures. The
//! fake resets every policy field a partial write leaves out, as Emby does.

use starr_adapter_emby::{AUTH, DESCRIPTOR, Emby};
use starr_adapter_kit::contract::account_writes::{WritesContract, run_writes_contract};
use starr_adapter_kit::contract::media_server::FakeFlavor;

#[tokio::test]
async fn emby_passes_the_account_writes_contract() {
    run_writes_contract(&WritesContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        build: Emby::new,
        flavor: FakeFlavor::Emby,
        recorded_dir: format!("{}/fixtures/4.10.1.0/accounts", env!("CARGO_MANIFEST_DIR")),
        plain: "friend",
        admin: "admin",
        // Emby's lock state is not readable (`capabilities.md`).
        locked: None,
        // Movies, Shows (`Guid`).
        libraries: [
            "c26fd8b5c1644eaaac60078a62ba4d00",
            "4032dc584f6a4f569ae151647f5b31dd",
        ],
    })
    .await;
}
