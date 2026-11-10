use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use secrecy::ExposeSecret;
use starr_adapter_musicbrainz::covers::CoverArchive;
use starr_adapter_tmdb::images::ImageCdn;
use starr_db::Database;
use starr_secrets::{CatalogTokenKey, TotpKey};
use starr_server::config::{self, AdminCommand, Command, Config, Mode};
use starr_server::recovery::{RecoverRequest, recover_admin};
use starr_server::{AppState, HttpSettings, ImageProxy};
use starr_worker::catalog::{CatalogAccess, FULL_SHARE, WEB_SHARE, WORKER_SHARE};
use starr_worker::image_cache::ImageCache;
use starr_worker::secrets::CredentialKey;
use starr_worker::{SystemJobs, Worker, WorkerConfig};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

/// Split mode: how often the worker looks for jobs the web process enqueued.
const SPLIT_DUE_CHECK: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Developer tools that print generated artifacts and exit.
    match std::env::args().nth(1).as_deref() {
        // API schema for TS codegen (`just gen-api`).
        Some("openapi") => {
            println!("{}", starr_server::openapi_json());
            return Ok(());
        }
        // Configuration reference (`just gen-config-docs`).
        Some("config-docs") => {
            print!("{}", config::config_docs_markdown());
            return Ok(());
        }
        _ => {}
    }

    let config = Config::parse();
    // `config.log` was validated while parsing.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&config.log))
        .init();
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating data dir {}", config.data_dir.display()))?;

    if let Some(Command::Admin(AdminCommand::Recover {
        username,
        reset_totp,
        create,
    })) = &config.command
    {
        let db = open_database(&config).await?;
        let request = RecoverRequest {
            username: username.clone(),
            reset_totp: *reset_totp,
            create: *create,
        };
        return run_recover(&db, &config, &request).await;
    }

    // Process model (ADR 0003): one process by default, or `web` and
    // `worker` sharing only the database. Key files are loaded before the
    // database is touched, so a bad key file stops the start early.
    let shutdown = CancellationToken::new();
    let mode = config.mode();
    match mode {
        // HTTP server and worker share the database, the in-memory wake
        // signal and the shutdown token.
        Mode::All => {
            let credentials = load_credentials(&config)?;
            let (state, listener) = start_web(&config, open_database(&config).await?, mode).await?;
            let worker = Worker::new(
                state.db.clone(),
                worker_jobs(&config, credentials).with_catalog(Arc::clone(&state.catalog)),
                Arc::clone(&state.wake),
                WorkerConfig::default(),
            );
            log_started(&config, mode, Some(&listener));
            let (served, (), ()) = tokio::join!(
                serve_web(listener, state, &shutdown),
                worker.run(shutdown.clone()),
                cancel_on_signal(&shutdown),
            );
            served?;
        }
        // Enqueues jobs, never runs them, never loads the secret key: service
        // credentials are sealed with the recipient the worker publishes.
        Mode::Web => {
            warn_if_secret_key_visible(&config);
            let (state, listener) = start_web(&config, open_database(&config).await?, mode).await?;
            log_started(&config, mode, Some(&listener));
            let (served, ()) = tokio::join!(
                serve_web(listener, state, &shutdown),
                cancel_on_signal(&shutdown),
            );
            served?;
        }
        // No listener. Nothing wakes it in memory, so it checks the queue
        // for jobs the web process enqueued. Reads the web key only if it
        // is mounted (catalog tokens, ADR 0003 amendment 1).
        Mode::Worker => {
            let credentials = load_credentials(&config)?;
            let mut jobs = worker_jobs(&config, credentials);
            if let Some(key) = CatalogTokenKey::load_existing(&config.web_key_file())
                .context("loading the web key")?
            {
                jobs = jobs.with_catalog(Arc::new(catalog_access(key, WORKER_SHARE)?));
            } else {
                tracing::warn!(
                    path = %config.web_key_file().display(),
                    "no web key: this worker cannot check catalog services (TMDB) or run catalog jobs; mount web.key into it read-only"
                );
            }
            let worker = Worker::new(
                open_database(&config).await?,
                jobs,
                Arc::new(Notify::new()),
                WorkerConfig {
                    due_check: Some(SPLIT_DUE_CHECK),
                    ..WorkerConfig::default()
                },
            );
            log_started(&config, mode, None);
            tokio::join!(worker.run(shutdown.clone()), cancel_on_signal(&shutdown));
        }
    }
    tracing::info!("Starr stopped");
    Ok(())
}

/// The job handlers of a process with a worker: notification links and
/// both image caches' limits from the configuration.
fn worker_jobs(config: &Config, credentials: CredentialKey) -> SystemJobs {
    SystemJobs::new(credentials)
        .with_public_url(config.public_url.clone())
        .with_image_cache(
            ImageCache::new(config.image_cache_dir()),
            config.image_cache_bytes(),
        )
        .with_adult_image_limit(config.adult_image_cache_bytes())
}

async fn open_database(config: &Config) -> anyhow::Result<Database> {
    Database::open(&config.database_path(), &config.backup_dir())
        .await
        .context("opening database")
}

/// The key that opens service credentials; only modes with a worker load it.
/// Its recipient is published once this process becomes the active worker.
fn load_credentials(config: &Config) -> anyhow::Result<CredentialKey> {
    CredentialKey::load(&config.secret_key_file()).context("loading the secret key")
}

/// Catalog access for a process holding `percent` of per-host budgets
/// (spec 0006 Q14: 100 alone, web 70 and worker 30 when split).
fn catalog_access(key: CatalogTokenKey, percent: u32) -> anyhow::Result<CatalogAccess> {
    let access = CatalogAccess::new(key).with_budget_share(percent);
    Ok(match dev_url("STARR_DEV_LISTENBRAINZ_URL")? {
        Some(url) => access.with_listenbrainz_base(&url),
        None => access,
    })
}

/// Development builds only: a fake's URL instead of a public service that
/// is not an operator setting (`ListenBrainz`, the Cover Art Archive), so
/// local runs and end-to-end tests never contact the real one. Release
/// builds ignore these variables.
fn dev_url(name: &str) -> anyhow::Result<Option<url::Url>> {
    if !cfg!(debug_assertions) {
        return Ok(None);
    }
    std::env::var(name)
        .ok()
        .map(|raw| url::Url::parse(&raw).with_context(|| format!("{name} is not a URL")))
        .transpose()
}

/// The web side's start: web key, setup token, listener. A web-only
/// process holds the web share of per-host catalog budgets.
async fn start_web(
    config: &Config,
    db: Database,
    mode: Mode,
) -> anyhow::Result<(AppState, TcpListener)> {
    let catalog_percent = if mode == Mode::Web {
        WEB_SHARE
    } else {
        FULL_SHARE
    };
    warn_about_public_url(config);
    // TOTP secrets and catalog tokens are opened by the web side with its
    // own key (ADR 0003 and its amendment 1).
    let totp_key =
        TotpKey::load_or_create(&config.web_key_file()).context("loading the web key")?;
    AppState::check_totp_key(&db, &totp_key).await?;
    let catalog = catalog_access(
        CatalogTokenKey::load_or_create(&config.web_key_file()).context("loading the web key")?,
        catalog_percent,
    )?;
    catalog
        .publish(&db)
        .await
        .context("publishing the catalog token recipient")?;
    let covers = match dev_url("STARR_DEV_COVERART_URL")? {
        Some(url) => CoverArchive::with_base(url),
        None => CoverArchive::new(),
    }
    .context("building the Cover Art Archive client")?;
    let images = ImageProxy::new(
        ImageCdn::new().context("building the image CDN client")?,
        covers,
        ImageCache::new(config.image_cache_dir()),
    );
    let state = AppState::new(db, http_settings(config))
        .with_totp_key(totp_key)
        .with_catalog(Arc::new(catalog))
        .with_images(images);
    if let Some(token) = state
        .issue_setup_token()
        .await
        .context("checking setup state")?
    {
        // Printed exactly once, on purpose: the operator copies it from the
        // container log into the setup page. Only its hash is kept. Written to
        // stderr directly so no STARR_LOG filter can hide it.
        eprintln!(
            "\n    No admin account exists yet. Open Starr and complete the setup with this one-time token:\n\n        SETUP TOKEN: {}\n\n    The token changes on every restart until setup is complete.\n",
            token.expose_secret()
        );
    }
    let addr = SocketAddr::new(config.bind, config.port);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    Ok((state, listener))
}

fn log_started(config: &Config, mode: Mode, listener: Option<&TcpListener>) {
    let addr = listener.and_then(|l| l.local_addr().ok());
    tracing::info!(
        version = starr_core::VERSION,
        ?mode,
        ?addr,
        data_dir = %config.data_dir.display(),
        "Starr started"
    );
}

fn warn_about_public_url(config: &Config) {
    if config
        .public_url
        .as_ref()
        .is_some_and(|url| url.scheme() == "https")
        && !config.behind_proxy
    {
        tracing::warn!(
            "STARR_PUBLIC_URL is https but STARR_BEHIND_PROXY is false: Starr cannot see that requests arrive over https and will set non-Secure cookies. Enable STARR_BEHIND_PROXY when a TLS proxy is in front"
        );
    }
    if config.public_url.is_none() {
        tracing::warn!(
            "STARR_PUBLIC_URL is not set; links and origin checks need it and setup will require it"
        );
    }
}

/// Web mode never opens the secret key. If the file is even visible to it,
/// running web and worker separately likely protects nothing, so say so.
/// Only the directory entry is looked at, never the file's content.
fn warn_if_secret_key_visible(config: &Config) {
    let path = config.secret_key_file();
    if std::fs::symlink_metadata(&path).is_ok() {
        tracing::warn!(
            path = %path.display(),
            "web mode does not use the secret key, but the file is visible to it; mount it only into the worker container, or the split does not protect service credentials"
        );
    }
}

fn http_settings(config: &Config) -> HttpSettings {
    HttpSettings {
        public_url: config.public_url.clone(),
        additional_origins: config.additional_origins.clone(),
        behind_proxy: config.behind_proxy,
        member_delete_grace_days: config.member_delete_grace_days,
        admin_totp_required: config.admin_totp_required,
    }
}

/// `starr admin recover`: print a one-time set-password link and exit.
async fn run_recover(
    db: &Database,
    config: &Config,
    request: &RecoverRequest,
) -> anyhow::Result<()> {
    let recovered = recover_admin(db, config.public_url.as_ref(), request, chrono::Utc::now())
        .await
        .context("recovering admin account")?;
    // The link is the point of this command: print it once, to stdout.
    println!(
        "Admin `{}` {}{}.\n\nOne-time link to set a new password (valid until {}):\n\n    {}\n",
        recovered.username,
        if recovered.created {
            "created"
        } else {
            "reactivated"
        },
        if request.reset_totp {
            ", TOTP turned off"
        } else {
            ""
        },
        recovered.expires_at.format("%Y-%m-%d %H:%M UTC"),
        recovered.link.expose_secret(),
    );
    Ok(())
}

/// Serve HTTP until `shutdown` fires. A server error stops everything else too.
async fn serve_web(
    listener: TcpListener,
    state: AppState,
    shutdown: &CancellationToken,
) -> anyhow::Result<()> {
    // Connect info provides the socket peer address for throttling.
    let result = starr_server::serve(listener, state, shutdown.clone().cancelled_owned()).await;
    shutdown.cancel();
    result.context("serving HTTP")
}

/// Fire `shutdown` on SIGINT/SIGTERM; returns early if something else fired it.
async fn cancel_on_signal(shutdown: &CancellationToken) {
    tokio::select! {
        () = shutdown_signal() => {
            tracing::info!("shutting down");
            shutdown.cancel();
        }
        () = shutdown.cancelled() => {}
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
