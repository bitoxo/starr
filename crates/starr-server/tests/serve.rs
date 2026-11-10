//! The real serving path must provide the socket address: without it every
//! client would share one throttling bucket, so the request fails instead.
#![allow(clippy::unwrap_used)]

use starr_db::Database;
use starr_server::{AppState, HttpSettings};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

#[tokio::test]
async fn served_requests_carry_the_client_address() {
    let db = Database::open_in_memory().await.unwrap();
    let state = AppState::new(
        db,
        HttpSettings {
            public_url: Some(Url::parse("https://starr.test").unwrap()),
            ..HttpSettings::default()
        },
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(starr_server::serve(listener, state, async {
        let _ = stopped.await;
    }));

    let body = r#"{"username":"nobody","password":"wrong password"}"#;
    let request = format!(
        "POST /api/v1/auth/login HTTP/1.1\r\nHost: {addr}\r\nOrigin: https://starr.test\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();

    // 401 (not 500): the login handler saw the peer address.
    assert!(
        response.starts_with("HTTP/1.1 401"),
        "unexpected response:\n{response}"
    );
    assert!(response.contains("auth.invalid_credentials"), "{response}");

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}
