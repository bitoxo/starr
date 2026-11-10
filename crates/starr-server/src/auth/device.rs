//! OWASP "device cookies": after a successful login the browser gets a signed
//! cookie naming the username. Login attempts presenting a valid one are
//! counted per device instead of per username, so an attacker hammering a
//! username cannot lock its owner out on a device they already used.
//!
//! Format: `v1.<base64url(username)>.<base64url(nonce)>.<base64url(hmac)>`,
//! HMAC-SHA256 over everything before the last dot, keyed by a random server
//! key stored in the database. The nonce identifies the device for throttling.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::crypto::{hmac_sha256, random_key_b64, verify_hmac_sha256};

const VERSION: &str = "v1";

/// A new device cookie value for `username` (normalized).
#[must_use]
pub fn issue(key: &[u8; 32], username: &str) -> String {
    let payload = format!(
        "{VERSION}.{}.{}",
        URL_SAFE_NO_PAD.encode(username),
        random_key_b64()
    );
    let mac = URL_SAFE_NO_PAD.encode(hmac_sha256(key, payload.as_bytes()));
    format!("{payload}.{mac}")
}

/// The device id if `value` is a genuine device cookie for `username`.
#[must_use]
pub fn verify(key: &[u8; 32], value: &str, username: &str) -> Option<String> {
    let (payload, mac) = value.rsplit_once('.')?;
    let mac = URL_SAFE_NO_PAD.decode(mac).ok()?;
    if !verify_hmac_sha256(key, payload.as_bytes(), &mac) {
        return None;
    }
    let mut parts = payload.split('.');
    let (Some(VERSION), Some(user), Some(nonce), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let user = URL_SAFE_NO_PAD.decode(user).ok()?;
    (user == username.as_bytes()).then(|| nonce.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn cookie_verifies_for_its_username_only() {
        let cookie = issue(&KEY, "root");
        let id = verify(&KEY, &cookie, "root").unwrap();
        assert_eq!(id.len(), 43);
        assert!(verify(&KEY, &cookie, "other").is_none());
        assert!(verify(&[8; 32], &cookie, "root").is_none());
    }

    #[test]
    fn tampering_is_rejected() {
        let cookie = issue(&KEY, "root");
        let (payload, mac) = cookie.rsplit_once('.').unwrap();
        let forged_user = payload.replacen(
            &URL_SAFE_NO_PAD.encode("root"),
            &URL_SAFE_NO_PAD.encode("mia"),
            1,
        );
        assert!(verify(&KEY, &format!("{forged_user}.{mac}"), "mia").is_none());
        assert!(verify(&KEY, "garbage", "root").is_none());
        assert!(verify(&KEY, "", "root").is_none());
    }

    #[test]
    fn each_login_gets_a_new_device_id() {
        let a = verify(&KEY, &issue(&KEY, "root"), "root");
        let b = verify(&KEY, &issue(&KEY, "root"), "root");
        assert_ne!(a, b);
    }
}
