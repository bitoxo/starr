//! Emby against the generic accounts contract (read half). Fixtures in
//! `fixtures/4.10.1.0/accounts/` are **recorded** from a disposable
//! `emby/embyserver:4.10.1.0` container (2026-11-02): users created and
//! their policies written with an API key (the open check of spec 0005:
//! an API key may read and write user policies in 4.10.1.0), one
//! restricted to the Movies library (by its `Guid`), one disabled, one
//! promoted to administrator, one locked out by ten failed logins.
#![allow(clippy::unwrap_used)]

use starr_adapter_emby::{AUTH, DESCRIPTOR, Emby};
use starr_adapter_kit::contract::accounts::{AccountsContract, run_accounts_contract};
use starr_adapter_kit::contract::{Fixture, ProbeContract, RecordedResponse, run_probe_contract};
use starr_core::accounts::{LibraryGrant, RemoteAccount};

fn read(file: &str) -> String {
    let path = format!(
        "{}/fixtures/4.10.1.0/accounts/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// `<status>\n<body>` as recorded.
fn recorded(file: &str) -> RecordedResponse {
    let text = read(file);
    let (status, body) = text.split_once('\n').unwrap();
    RecordedResponse {
        status: status.parse().unwrap(),
        body: body.trim_end().to_owned(),
    }
}

/// Recorded with a wrong key.
fn unauthorized() -> RecordedResponse {
    RecordedResponse {
        status: 401,
        body: "Access token is invalid or expired.".into(),
    }
}

fn account(id: &str, name: &str) -> RemoteAccount {
    RemoteAccount {
        remote_id: id.to_owned(),
        username: name.to_owned(),
        enabled: true,
        privileged: false,
        locked_by_service: false,
        libraries: LibraryGrant::All,
    }
}

#[tokio::test]
async fn emby_passes_the_accounts_contract() {
    let expected = vec![
        RemoteAccount {
            privileged: true,
            ..account("1e5fa18b5d9646b7b7f0cfee42fb6689", "admin")
        },
        account("a3f76933e10349be865f91bc2254a913", "Alex"),
        RemoteAccount {
            enabled: false,
            ..account("e343b4703a12474eac9f8f6418153159", "disabled-friend")
        },
        account("5537a7d831824ab3bac7c5b97d439208", "friend"),
        // Locked out (`LockedOutDate` set), but Emby's lock expires by
        // itself and the policy cannot tell whether it still holds.
        account("a1500f21427945409692ee5c447663f8", "locked-friend"),
        RemoteAccount {
            libraries: LibraryGrant::Only(vec!["c26fd8b5c1644eaaac60078a62ba4d00".to_owned()]),
            ..account("5d362e15f5a94d0fae9fbc9b96e655f6", "restricted")
        },
        RemoteAccount {
            privileged: true,
            ..account("d86ceac45158432ab5111b2e6afd5180", "second-admin")
        },
    ];
    assert!(read("users.json").contains("\"LockedOutDate\": 1793647560007"));
    run_accounts_contract(&AccountsContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        build: Emby::new,
        users: read("users.json"),
        expected,
        one: ("5537a7d831824ab3bac7c5b97d439208".into(), read("user.json")),
        unknown: recorded("user_unknown.txt"),
        unauthorized: unauthorized(),
    })
    .await;
}

/// The recorded system info passes the probe (product and version check).
#[tokio::test]
async fn the_recorded_system_info_passes_the_probe() {
    let fixture = |version: &str, body: String| Fixture {
        version: version.parse().unwrap(),
        body,
    };
    let unsupported = std::fs::read_to_string(format!(
        "{}/fixtures/4.9.1.90/system_info.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    run_probe_contract(&ProbeContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        probe_method: "GET",
        probe_path: "/System/Info",
        supported: vec![fixture("4.10.1.0", read("system_info.json"))],
        unsupported: fixture("4.9.1.90", unsupported),
        unauthorized: unauthorized(),
        build: Emby::new,
    })
    .await;
}
