//! Jellyfin against the generic accounts contract (read half). Fixtures in
//! `fixtures/12.3.0/accounts/` are **recorded** from a disposable
//! `jellyfin/jellyfin:12.3` container (2026-11-02): users created through
//! the API key, one restricted to the Movies library, one disabled, one
//! promoted to administrator, one locked out by three failed logins with
//! `LoginAttemptsBeforeLockout: 3`.
#![allow(clippy::unwrap_used)]

use starr_adapter_jellyfin::{AUTH, DESCRIPTOR, Jellyfin};
use starr_adapter_kit::contract::RecordedResponse;
use starr_adapter_kit::contract::accounts::{AccountsContract, run_accounts_contract};
use starr_core::accounts::{LibraryGrant, RemoteAccount};

fn read(file: &str) -> String {
    let path = format!(
        "{}/fixtures/12.3.0/accounts/{file}",
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

fn expected() -> Vec<RemoteAccount> {
    vec![
        RemoteAccount {
            privileged: true,
            ..account("951b2f041bac4fe0a0c762f02f5117b7", "admin")
        },
        account("53bf201374d74f19b2dbc1986457da92", "Alex"),
        RemoteAccount {
            enabled: false,
            ..account("254f3ef0e4194030bd47ef359ee659ee", "disabled-friend")
        },
        account("08552f025edb4744beacbbbb80ea75fe", "friend"),
        RemoteAccount {
            enabled: false,
            locked_by_service: true,
            ..account("568240d2f3f1480ab0908ca5992e53ba", "locked-friend")
        },
        RemoteAccount {
            libraries: LibraryGrant::Only(vec!["f137a2dd21bbc1b99aa5c0f6bf02a805".to_owned()]),
            ..account("237f7023497e4c7893306d3a7ece5044", "restricted")
        },
        RemoteAccount {
            privileged: true,
            ..account("db5c5122ead8422e9435fb6d79e177b1", "second-admin")
        },
    ]
}

fn contract(unknown: RecordedResponse) -> AccountsContract<Jellyfin> {
    AccountsContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        build: Jellyfin::new,
        users: read("users.json"),
        expected: expected(),
        one: ("08552f025edb4744beacbbbb80ea75fe".into(), read("user.json")),
        unknown,
        // Jellyfin answers a wrong or missing key with an empty 401.
        unauthorized: RecordedResponse {
            status: 401,
            body: String::new(),
        },
    }
}

#[tokio::test]
async fn jellyfin_passes_the_accounts_contract() {
    run_accounts_contract(&contract(recorded("user_unknown.txt"))).await;
}

/// Jellyfin 10.11 answers an unknown user ID with 400 instead of 404
/// (`capabilities.md`); it must still be "not found".
#[tokio::test]
async fn an_unknown_user_answered_with_400_is_not_found() {
    run_accounts_contract(&contract(RecordedResponse {
        status: 400,
        body: "Error processing request.".into(),
    }))
    .await;
}

/// The open check of spec 0005: API-created users are hidden from the
/// login screen in 12.3 (`IsHidden: true` on every recorded user).
#[test]
fn api_created_users_are_hidden_from_the_login_screen() {
    let users = read("users.json");
    assert_eq!(
        users.matches("\"IsHidden\": true").count(),
        expected().len()
    );
    assert!(!users.contains("\"IsHidden\": false"));
}

/// The recorded system info passes the probe (product and version check).
#[tokio::test]
async fn the_recorded_system_info_passes_the_probe() {
    use starr_adapter_kit::contract::{Fixture, ProbeContract, run_probe_contract};
    let fixture = |version: &str, body: String| Fixture {
        version: version.parse().unwrap(),
        body,
    };
    let unsupported = std::fs::read_to_string(format!(
        "{}/fixtures/10.10.7/system_info.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    run_probe_contract(&ProbeContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        probe_method: "GET",
        probe_path: "/System/Info",
        supported: vec![fixture("12.3.0", read("system_info.json"))],
        unsupported: fixture("10.10.7", unsupported),
        unauthorized: RecordedResponse {
            status: 401,
            body: String::new(),
        },
        build: Jellyfin::new,
    })
    .await;
}
