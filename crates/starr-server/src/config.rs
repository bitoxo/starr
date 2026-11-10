//! Operator configuration: the single schema for every `STARR_*` setting.
//!
//! Every option can be set via environment variable (the usual way in Docker)
//! or the equivalent CLI flag. Values are validated at start; an invalid value
//! stops the process with a readable message. `docs/configuration.md` is
//! generated from this schema ([`config_docs_markdown`]).

use std::fmt::Write as _;
use std::net::IpAddr;
use std::path::PathBuf;

use clap::builder::BoolishValueParser;
use clap::{ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;
use url::Url;

#[derive(Debug, Clone, Parser)]
#[command(name = "starr", version, about)]
pub struct Config {
    /// Which parts this process runs: `all` (HTTP server and worker in one
    /// process, the default), `web` (HTTP only: enqueues jobs but never runs
    /// them and never reads `STARR_SECRET_KEY_FILE`) or `worker` (runs jobs,
    /// no HTTP listener). `web` and `worker` share `STARR_DATA_DIR` on a local
    /// volume. The subcommands `starr all`, `starr web` and `starr worker`
    /// take precedence over this variable.
    #[arg(long, env = "STARR_MODE", value_enum, default_value_t = Mode::All)]
    mode: Mode,

    /// Directory for the database, backups and other persistent state (`/config` in Docker).
    #[arg(long, env = "STARR_DATA_DIR", default_value = "./data")]
    pub data_dir: PathBuf,

    /// Address to bind the HTTP server to.
    #[arg(long, env = "STARR_BIND", default_value = "0.0.0.0")]
    pub bind: IpAddr,

    /// HTTP port.
    #[arg(long, env = "STARR_PORT", default_value_t = 7979)]
    pub port: u16,

    /// Log filter in `tracing` syntax, e.g. `info,starr=debug`.
    #[arg(long, env = "STARR_LOG", default_value = "info", value_parser = parse_log_filter)]
    pub log: String,

    /// Public base URL under which users reach Starr, e.g. `https://starr.example.com`.
    /// Absolute http(s) URL without path, query or fragment. Used for links
    /// (also in notification emails and webhooks, so in split mode give it to
    /// the worker too) and CSRF origin checks (mutating requests must send this `Origin` or one of
    /// `STARR_ADDITIONAL_ORIGINS`); setup refuses to finish without it.
    /// Development only: while unset, requests whose `Origin` matches their
    /// `Host` header also pass the CSRF check. Do not run without it in production.
    #[arg(long, env = "STARR_PUBLIC_URL", value_parser = parse_public_url)]
    pub public_url: Option<Url>,

    /// Further origins users reach Starr under, comma-separated, each like
    /// `STARR_PUBLIC_URL` (e.g. `http://192.168.1.10:7979` next to an https
    /// public URL). Accepted by the CSRF origin check.
    #[arg(long, env = "STARR_ADDITIONAL_ORIGINS", value_delimiter = ',', value_parser = parse_public_url)]
    pub additional_origins: Vec<Url>,

    /// Trust `X-Forwarded-*` headers from a reverse proxy in front of Starr.
    /// Enable only when Starr is reachable exclusively through that proxy.
    /// Starr trusts exactly one hop: the client IP (for login throttling) is the
    /// rightmost `X-Forwarded-For` entry, the one your proxy appended; entries
    /// left of it are client-supplied and ignored. The rightmost
    /// `X-Forwarded-Proto` decides the cookies: on `https` Starr uses
    /// `__Host-` prefixed `Secure` cookies, otherwise plain cookies without
    /// `Secure` (Starr itself only speaks http). The headers count only when
    /// the connection comes from a loopback or private address (`127.0.0.0/8`,
    /// `::1`, RFC 1918, link-local, IPv6 ULA `fc00::/7`), i.e. a proxy on the
    /// host or in a Docker network; from any other peer they are ignored.
    #[arg(long, env = "STARR_BEHIND_PROXY", action = ArgAction::SetTrue, default_value_t = false, value_parser = BoolishValueParser::new())]
    pub behind_proxy: bool,

    /// Require a second factor (TOTP) for every admin account. An admin
    /// without TOTP is sent to enrollment right after logging in; until it
    /// is confirmed, no other admin API works.
    #[arg(long, env = "STARR_ADMIN_TOTP_REQUIRED", action = ArgAction::SetTrue, default_value_t = false, value_parser = BoolishValueParser::new())]
    pub admin_totp_required: bool,

    /// Days a deleted member stays restorable before only a tombstone remains
    /// (username reserved, everything else removed). Applies to members who
    /// have used Starr or own an account Starr created in a media server;
    /// members who never did are deleted at once. Their media server accounts
    /// are disabled at once; when this period ends the accounts Starr created
    /// are deleted and linked ones released (never ignored or administrator
    /// accounts). With 0 this happens at the next reconcile, without a
    /// disabled phase.
    #[arg(long, env = "STARR_MEMBER_DELETE_GRACE_DAYS", default_value_t = 30, value_parser = clap::value_parser!(u16).range(0..=365))]
    pub member_delete_grace_days: u16,

    /// Size limit of the disk cache of proxied posters and logos, in MiB.
    /// The worker's hourly housekeeping deletes the least recently used
    /// images beyond it.
    #[arg(long, env = "STARR_IMAGE_CACHE_MB", default_value_t = 1024, value_parser = clap::value_parser!(u32).range(16..=1_048_576))]
    pub image_cache_mb: u32,

    /// Size limit of the separate disk cache of the private category's
    /// images, in MiB. Its images are also deleted 30 days after they were
    /// last shown, and all at once when the category is switched off.
    #[arg(long, env = "STARR_ADULT_IMAGE_CACHE_MB", default_value_t = 256, value_parser = clap::value_parser!(u32).range(16..=1_048_576))]
    pub adult_image_cache_mb: u32,

    /// File holding the key that opens stored service credentials (age X25519).
    /// Created on first start with mode 0600. Any file not writable by group
    /// or others works (e.g. a read-only Docker secret, 0444). Starr refuses to
    /// start if the file is writable by group or others, unreadable or
    /// malformed. If it is missing
    /// or holds another key, Starr starts with that key and asks for the API
    /// key of every service sealed with the old one. Back it up together with
    /// `starr.db`. Defaults to `secret.key` inside `STARR_DATA_DIR`.
    #[arg(long, env = "STARR_SECRET_KEY_FILE")]
    secret_key_file: Option<PathBuf>,

    /// File holding the web process's key for admins' TOTP secrets and
    /// catalog read tokens such as TMDB's (age X25519), created on first
    /// start like `STARR_SECRET_KEY_FILE` and with the same permission
    /// rules. The web side (`all` or `web` mode) creates and reads it; a
    /// `worker` process reads it if present (mount it read-only) to check
    /// catalog services and run catalog jobs, and never creates it. Back it
    /// up too. Defaults to `web.key` inside `STARR_DATA_DIR`.
    #[arg(long, env = "STARR_WEB_KEY_FILE")]
    web_key_file: Option<PathBuf>,

    /// Maintenance command instead of running the server.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Which parts of Starr a process runs (ADR 0003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// HTTP server and worker in one process.
    All,
    /// HTTP server only; jobs are run by a separate `worker` process.
    Web,
    /// Worker only: runs jobs, no HTTP listener.
    Worker,
}

/// What to run instead of `STARR_MODE`, or a maintenance command run next to
/// a running server (`docker exec`).
#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Run HTTP server and worker in one process (the default).
    All,
    /// Run the HTTP server only (split mode); needs a separate `starr worker`.
    Web,
    /// Run the worker only (split mode): executes jobs, opens no port.
    Worker,
    /// Admin account maintenance.
    #[command(subcommand)]
    Admin(AdminCommand),
}

#[derive(Debug, Clone, Subcommand)]
pub enum AdminCommand {
    /// Regain access when locked out: reactivates the admin and prints a
    /// one-time set-password link. Removed admins stay removed.
    Recover {
        /// Username of the admin account.
        username: String,
        /// Also turn TOTP off (lost authenticator and recovery codes).
        #[arg(long)]
        reset_totp: bool,
        /// Create the admin if no account has this username.
        #[arg(long)]
        create: bool,
    },
}

impl Config {
    /// The mode to run: the `all`/`web`/`worker` subcommand if given,
    /// otherwise `STARR_MODE`.
    #[must_use]
    pub fn mode(&self) -> Mode {
        match self.command {
            Some(Command::All) => Mode::All,
            Some(Command::Web) => Mode::Web,
            Some(Command::Worker) => Mode::Worker,
            None | Some(Command::Admin(_)) => self.mode,
        }
    }

    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("starr.db")
    }

    /// Where pre-migration database backups are written.
    #[must_use]
    pub fn backup_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }

    /// Disk cache of proxied images (written by the web side, evicted by
    /// the worker's housekeeping).
    #[must_use]
    pub fn image_cache_dir(&self) -> PathBuf {
        self.data_dir.join("cache").join("images")
    }

    /// Image cache limit in bytes (`STARR_IMAGE_CACHE_MB`).
    #[must_use]
    pub fn image_cache_bytes(&self) -> u64 {
        u64::from(self.image_cache_mb) * 1024 * 1024
    }

    /// Adult image cache limit in bytes (`STARR_ADULT_IMAGE_CACHE_MB`).
    #[must_use]
    pub fn adult_image_cache_bytes(&self) -> u64 {
        u64::from(self.adult_image_cache_mb) * 1024 * 1024
    }

    /// The web process's own key file (TOTP secrets, ADR 0003), defaulting
    /// to `<data_dir>/web.key`. Separate from `STARR_SECRET_KEY_FILE`, which
    /// only the worker opens.
    #[must_use]
    pub fn web_key_file(&self) -> PathBuf {
        self.web_key_file
            .clone()
            .unwrap_or_else(|| self.data_dir.join("web.key"))
    }

    /// Path of the secret key file, defaulting to `<data_dir>/secret.key`.
    #[must_use]
    pub fn secret_key_file(&self) -> PathBuf {
        self.secret_key_file
            .clone()
            .unwrap_or_else(|| self.data_dir.join("secret.key"))
    }
}

fn parse_log_filter(value: &str) -> Result<String, String> {
    EnvFilter::try_new(value)
        .map(|_| value.to_owned())
        .map_err(|e| format!("not a valid log filter ({e}); expected e.g. `info,starr=debug`"))
}

fn parse_public_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|e| {
        format!("not an absolute URL ({e}); expected e.g. `https://starr.example.com`")
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "scheme must be http or https, not `{}`",
            url.scheme()
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("must contain a host".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not contain credentials".to_owned());
    }
    if url.path() != "/" {
        return Err(format!(
            "must not contain a path (`{}`); serving under a sub-path is not supported",
            url.path()
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("must not contain a query or fragment".to_owned());
    }
    Ok(url)
}

const DOCS_HEADER: &str = "\
# Configuration

<!-- Generated from crates/starr-server/src/config.rs by `just gen-config-docs`. Do not edit by hand. -->

Starr is configured through environment variables (the usual way in Docker) or the
equivalent command-line flags (`starr --help`). Invalid values stop the start with an
error message.

| Variable | Default | Description |
|---|---|---|
";

const DOCS_FOOTER: &str = "
## Time zone

Starr doesn't read `TZ`. It stores and logs times in UTC (log timestamps end in `Z`), and the web UI
shows times in each viewer's browser time zone. Setting `TZ` in the container does no harm.

## Reverse proxy and the private category

The private category's search sends its text in a request body (`POST /api/v1/private/search`),
never in a URL. Its pages and images still carry catalog ids in their paths
(`/api/v1/private/scenes/{id}`, `/api/v1/private/performers/{id}`,
`/api/v1/private/studios/{id}`, `/api/v1/images/stashdb/{size}/{id}`), and those ids identify
the titles. Starr never logs them; a reverse proxy in front of Starr may. Disable or filter
its access log for `/api/v1/private` and `/api/v1/images/stashdb` (for example nginx
`access_log off;` in a `location` for each, or a Caddy `log_skip` matcher).
";

/// The configuration reference as Markdown, generated from the clap schema.
#[must_use]
pub fn config_docs_markdown() -> String {
    let command = Config::command();
    let mut out = String::from(DOCS_HEADER);
    for arg in command.get_arguments() {
        let Some(env) = arg.get_env() else {
            continue;
        };
        let defaults: Vec<_> = arg
            .get_default_values()
            .iter()
            .map(|v| v.to_string_lossy())
            .collect();
        let default = if defaults.is_empty() {
            "–".to_owned()
        } else {
            format!("`{}`", defaults.join(", "))
        };
        let help = arg
            .get_long_help()
            .or_else(|| arg.get_help())
            .map(ToString::to_string)
            .unwrap_or_default();
        let help = help
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace('|', "\\|");
        // Writing into a String cannot fail.
        let _ = writeln!(out, "| `{}` | {default} | {help} |", env.to_string_lossy());
    }
    out.push_str(DOCS_FOOTER);
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn parse(args: &[&str]) -> Result<Config, clap::Error> {
        Config::try_parse_from(std::iter::once("starr").chain(args.iter().copied()))
    }

    #[test]
    fn defaults() {
        let config = parse(&[]).unwrap();
        assert_eq!(config.data_dir, Path::new("./data"));
        assert_eq!(config.bind.to_string(), "0.0.0.0");
        assert_eq!(config.port, 7979);
        assert_eq!(config.log, "info");
        assert!(config.public_url.is_none());
        assert_eq!(config.additional_origins, Vec::<Url>::new());
        assert!(!config.behind_proxy);
        assert!(!config.admin_totp_required);
        assert_eq!(config.member_delete_grace_days, 30);
        assert_eq!(config.image_cache_bytes(), 1024 * 1024 * 1024);
        assert_eq!(config.adult_image_cache_bytes(), 256 * 1024 * 1024);
        assert_eq!(config.secret_key_file(), Path::new("./data/secret.key"));
        assert_eq!(config.web_key_file(), Path::new("./data/web.key"));
        assert_eq!(config.backup_dir(), Path::new("./data/backups"));
        assert_eq!(config.mode(), Mode::All);
    }

    #[test]
    fn mode_from_flag_or_subcommand() {
        assert_eq!(parse(&["--mode", "web"]).unwrap().mode(), Mode::Web);
        assert_eq!(parse(&["--mode", "worker"]).unwrap().mode(), Mode::Worker);
        assert_eq!(parse(&["web"]).unwrap().mode(), Mode::Web);
        assert_eq!(parse(&["worker"]).unwrap().mode(), Mode::Worker);
        assert_eq!(parse(&["all"]).unwrap().mode(), Mode::All);
        // The subcommand wins over STARR_MODE / --mode.
        assert_eq!(
            parse(&["--mode", "web", "worker"]).unwrap().mode(),
            Mode::Worker
        );
        let err = parse(&["--mode", "both"]).unwrap_err().to_string();
        assert!(err.contains("possible values: all, web, worker"), "{err}");
    }

    #[test]
    fn web_key_file_follows_data_dir_unless_set() {
        let config = parse(&["--data-dir", "/config"]).unwrap();
        assert_eq!(config.web_key_file(), Path::new("/config/web.key"));
        let config = parse(&["--web-key-file", "/run/secrets/web"]).unwrap();
        assert_eq!(config.web_key_file(), Path::new("/run/secrets/web"));
    }

    #[test]
    fn secret_key_file_follows_data_dir_unless_set() {
        let config = parse(&["--data-dir", "/config"]).unwrap();
        assert_eq!(config.secret_key_file(), Path::new("/config/secret.key"));
        let config = parse(&["--secret-key-file", "/run/secrets/starr"]).unwrap();
        assert_eq!(config.secret_key_file(), Path::new("/run/secrets/starr"));
    }

    #[test]
    fn boolean_flags() {
        let config = parse(&["--behind-proxy", "--admin-totp-required"]).unwrap();
        assert!(config.behind_proxy);
        assert!(config.admin_totp_required);
    }

    #[test]
    fn valid_public_urls_are_accepted() {
        for url in [
            "https://starr.example.com",
            "https://starr.example.com/",
            "http://192.168.1.10:7979",
        ] {
            let config = parse(&["--public-url", url]).unwrap_or_else(|e| panic!("{url}: {e}"));
            assert!(config.public_url.is_some(), "{url}");
        }
    }

    #[test]
    fn invalid_public_urls_are_rejected_with_a_reason() {
        for (url, reason) in [
            ("starr.example.com", "not an absolute URL"),
            ("/starr", "not an absolute URL"),
            ("ftp://starr.example.com", "scheme must be http or https"),
            ("https://starr.example.com/starr", "must not contain a path"),
            ("https://starr.example.com/?a=b", "query or fragment"),
            ("https://starr.example.com/#x", "query or fragment"),
            ("https://user:pw@starr.example.com", "credentials"),
        ] {
            let err = parse(&["--public-url", url]).unwrap_err().to_string();
            assert!(err.contains(reason), "{url}: {err}");
        }
    }

    #[test]
    fn additional_origins_are_a_validated_list() {
        let config = parse(&[
            "--additional-origins",
            "http://192.168.1.10:7979,https://starr.lan",
        ])
        .unwrap();
        assert_eq!(config.additional_origins.len(), 2);
        let err = parse(&["--additional-origins", "http://ok.lan,https://x.lan/path"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("must not contain a path"), "{err}");
    }

    #[test]
    fn member_delete_grace_days_is_bounded() {
        assert_eq!(
            parse(&["--member-delete-grace-days", "7"])
                .unwrap()
                .member_delete_grace_days,
            7
        );
        assert!(parse(&["--member-delete-grace-days", "366"]).is_err());
        assert!(parse(&["--member-delete-grace-days", "-1"]).is_err());
    }

    #[test]
    fn image_cache_size_is_bounded() {
        assert_eq!(
            parse(&["--image-cache-mb", "64"])
                .unwrap()
                .image_cache_bytes(),
            64 * 1024 * 1024
        );
        assert!(parse(&["--image-cache-mb", "0"]).is_err());
        assert!(parse(&["--image-cache-mb", "big"]).is_err());
    }

    #[test]
    fn admin_recover_subcommand() {
        let config = parse(&["admin", "recover", "root", "--reset-totp"]).unwrap();
        let Some(Command::Admin(AdminCommand::Recover {
            username,
            reset_totp,
            create,
        })) = config.command
        else {
            panic!("no recover command");
        };
        assert_eq!(username, "root");
        assert!(reset_totp);
        assert!(!create);
        assert!(parse(&[]).unwrap().command.is_none());
        assert!(parse(&["admin", "recover"]).is_err(), "username required");
    }

    #[test]
    fn invalid_log_filter_is_rejected() {
        let err = parse(&["--log", "starr=loud"]).unwrap_err().to_string();
        assert!(err.contains("not a valid log filter"), "{err}");
    }

    #[test]
    fn docs_list_every_setting() {
        let docs = config_docs_markdown();
        for name in [
            "STARR_DATA_DIR",
            "STARR_BIND",
            "STARR_PORT",
            "STARR_LOG",
            "STARR_PUBLIC_URL",
            "STARR_ADDITIONAL_ORIGINS",
            "STARR_BEHIND_PROXY",
            "STARR_ADMIN_TOTP_REQUIRED",
            "STARR_SECRET_KEY_FILE",
            "STARR_WEB_KEY_FILE",
            "STARR_MODE",
            "STARR_MEMBER_DELETE_GRACE_DAYS",
            "STARR_IMAGE_CACHE_MB",
            "STARR_ADULT_IMAGE_CACHE_MB",
        ] {
            assert!(docs.contains(&format!("| `{name}` |")), "{name} missing");
        }
    }

    #[test]
    fn committed_config_docs_are_up_to_date() {
        let committed = include_str!("../../../docs/configuration.md");
        assert!(
            committed == config_docs_markdown(),
            "docs/configuration.md is out of date with the config schema; run `just gen-config-docs`"
        );
    }
}
