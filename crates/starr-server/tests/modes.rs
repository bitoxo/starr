//! Process modes (ADR 0003), run as the real binary: `starr web` never
//! touches the secret key, `starr worker` never touches the web key and
//! opens no port, and a bad secret key stops the worker.
#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("starr-modes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A running `starr <mode>` whose log lines arrive on a channel.
struct Starr {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Starr {
    fn spawn(data_dir: &Path, args: &[&str]) -> Self {
        Self::spawn_with(data_dir, args, &[])
    }

    fn spawn_with(data_dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_starr"))
            .args(args)
            .env_clear()
            .envs(env.iter().copied())
            .env("STARR_DATA_DIR", data_dir)
            .env("STARR_BIND", "127.0.0.1")
            .env("STARR_PORT", "0")
            .env("STARR_LOG", "info")
            .env("NO_COLOR", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, lines) = mpsc::channel();
        for out in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    let _ = tx.send(line);
                }
            });
        }
        Self { child, lines }
    }

    /// Wait for a log line containing `needle`; panics with the log so far.
    fn wait_for(&self, needle: &str) -> String {
        let mut seen = Vec::new();
        while let Ok(line) = self.lines.recv_timeout(Duration::from_secs(20)) {
            if line.contains(needle) {
                return line;
            }
            seen.push(line);
        }
        panic!("no log line with `{needle}`; log:\n{}", seen.join("\n"));
    }
}

impl Drop for Starr {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn web_mode_runs_without_the_secret_key_and_never_creates_it() {
    let dir = TempDir::new("web");
    let web = Starr::spawn(&dir.0, &["web"]);
    let started = web.wait_for("Starr started");
    assert!(started.contains("Web"), "{started}");
    drop(web);

    assert!(
        dir.0.join("web.key").exists(),
        "web mode creates its web key"
    );
    assert!(
        !dir.0.join("secret.key").exists(),
        "web mode must not create or need the secret key"
    );
}

#[test]
fn starr_mode_env_selects_the_mode() {
    let dir = TempDir::new("env");
    let worker = Starr::spawn_with(&dir.0, &[], &[("STARR_MODE", "worker")]);
    let started = worker.wait_for("Starr started");
    assert!(started.contains("Worker"), "{started}");
}

#[test]
fn worker_mode_opens_no_port_and_never_creates_the_web_key() {
    let dir = TempDir::new("worker");
    let worker = Starr::spawn(&dir.0, &["worker"]);
    let started = worker.wait_for("Starr started");
    assert!(
        started.contains("Worker") && started.contains("addr=None"),
        "{started}"
    );
    worker.wait_for("worker lease acquired");
    drop(worker);

    assert!(
        dir.0.join("secret.key").exists(),
        "the worker creates its key"
    );
    assert!(
        !dir.0.join("web.key").exists(),
        "worker mode must not create the web key"
    );
}

#[cfg(unix)]
#[test]
fn worker_mode_refuses_to_start_with_an_unusable_secret_key() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("worker-bad-key");
    let key = dir.0.join("secret.key");
    std::fs::write(&key, "not a key\n").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();

    let mut worker = Starr::spawn(&dir.0, &["worker"]);
    let status = worker.child.wait().unwrap();
    assert!(!status.success());
    worker.wait_for("loading the secret key");
}
