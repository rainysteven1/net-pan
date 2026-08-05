use std::{
    collections::BTreeMap,
    env,
    io::{self, IsTerminal},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use pan_tui::{
    client::{DaemonPanClient, EmbeddedPanClient, PanClient},
    config::{AppConfig, default_config_root},
    daemon::{DaemonClient, DaemonService},
    download::DownloadOptions,
    ipc::default_socket_path,
    job::DownloadManager,
    notifier::{
        DingTalkNotifierFactory, FeishuNotifierFactory, NotifierConfigFile, NotifierRegistry,
    },
    provider::{
        ProfileId, ProviderAuthStatus, ProviderContext, ProviderId, ProviderIdentity,
        ProviderLoginRequest, ProviderRegistry, ProviderRuntime, ProviderStatus, RemoteItem,
        RemoteObjectId, RemoteRef, SourceId,
        aliyun::AliyunProviderFactory,
        google::{GoogleProviderConfig, GoogleProviderFactory},
    },
    state::{StateStore, default_state_db_path},
    tui::{BrowserOutcome, BrowserProvider, BrowserRuntime, run_browser},
};

#[derive(Debug, Parser)]
#[command(
    name = "pan-tui",
    version,
    about = "Browse cloud-drive folders in a TUI and download selected items locally"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Provider to open first, for example google or aliyun
    #[arg(long, global = true)]
    provider: Option<String>,

    /// Service-account JSON; defaults to the environment or standard config path
    #[arg(short, long, global = true)]
    credentials: Option<PathBuf>,

    /// JSON file containing saved cloud-drive folder links or IDs
    #[arg(long, global = true)]
    sources: Option<PathBuf>,

    /// List saved folder links, then exit without the TUI or downloads
    #[arg(long, global = true)]
    list: bool,

    /// Local directory under which the selected cloud-drive item is created
    #[arg(short, long, global = true)]
    destination: Option<PathBuf>,

    /// Replace completed destination files instead of skipping them
    #[arg(long, global = true)]
    overwrite: bool,

    /// Restart partial files instead of resuming their .part files
    #[arg(long, global = true)]
    no_resume: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the long-lived download daemon
    Daemon {
        /// Override the local Unix socket path
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Show persisted and active daemon Jobs
    Status {
        /// Override the local Unix socket path
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// List registered Providers and their readiness as JSON
    Providers {
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// List Sources exposed by a Provider as JSON
    Sources {
        provider: String,
        #[arg(long, default_value = "default")]
        profile: String,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// List children of an opaque remote folder reference as JSON
    Ls {
        provider: String,
        source: String,
        object: String,
        #[arg(long, default_value = "default")]
        profile: String,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Refresh one opaque remote item and print its metadata as JSON
    Stat {
        provider: String,
        source: String,
        object: String,
        #[arg(long, default_value = "default")]
        profile: String,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Resolve a remote item and submit it to the daemon
    Download {
        provider: String,
        source: String,
        object: String,
        destination: PathBuf,
        #[arg(long, default_value = "default")]
        profile: String,
        #[arg(long, default_value_t = 2)]
        workers: usize,
        #[arg(long)]
        request_id: Option<u64>,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Show one persisted or active Job as JSON
    Job {
        job_id: u64,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Stream daemon Job snapshots as JSON Lines
    Watch {
        #[arg(long, default_value_t = 0)]
        after_revision: u64,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Cancel an active daemon Job
    Cancel {
        job_id: u64,
        /// Override the local Unix socket path
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Submit a serialized DownloadJobRequest to the daemon
    Submit {
        request: PathBuf,
        /// Stable idempotency request ID; defaults to a hash of the request JSON
        #[arg(long)]
        request_id: Option<u64>,
        /// Override the local Unix socket path
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Run the interactive TUI
    Tui,
    /// Manage Provider authentication independently from the TUI
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Start a Provider-owned interactive login flow
    Login { provider: String },
    /// Show authentication state without exposing credentials
    Status { provider: Option<String> },
    /// Remove local Provider authentication state
    Revoke { provider: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Some(Command::Daemon { socket }) => return run_daemon(&cli, socket.clone()).await,
        Some(Command::Status { socket }) => {
            let result = DaemonClient::local(socket.clone())?.status().await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        Some(Command::Providers { socket }) => {
            let providers = DaemonClient::local(socket.clone())?.providers().await?;
            println!("{}", serde_json::to_string_pretty(&providers)?);
            return Ok(());
        }
        Some(Command::Sources {
            provider,
            profile,
            socket,
        }) => {
            let sources = DaemonClient::local(socket.clone())?
                .sources(provider_identity(provider, profile)?)
                .await?;
            println!("{}", serde_json::to_string_pretty(&sources)?);
            return Ok(());
        }
        Some(Command::Ls {
            provider,
            source,
            object,
            profile,
            socket,
        }) => {
            let items = DaemonClient::local(socket.clone())?
                .list_children(remote_ref(provider, profile, source, object)?)
                .await?;
            println!("{}", serde_json::to_string_pretty(&items)?);
            return Ok(());
        }
        Some(Command::Stat {
            provider,
            source,
            object,
            profile,
            socket,
        }) => {
            let item = DaemonClient::local(socket.clone())?
                .refresh_item(remote_ref(provider, profile, source, object)?)
                .await?;
            println!("{}", serde_json::to_string_pretty(&item)?);
            return Ok(());
        }
        Some(Command::Download {
            provider,
            source,
            object,
            destination,
            profile,
            workers,
            request_id,
            socket,
        }) => {
            let daemon = DaemonClient::local(socket.clone())?;
            let item = daemon
                .refresh_item(remote_ref(provider, profile, source, object)?)
                .await?;
            let request = pan_tui::job::DownloadJobRequest::new(
                item,
                destination.clone(),
                DownloadOptions {
                    overwrite: cli.overwrite,
                    resume: !cli.no_resume,
                    concurrency: (*workers).max(1),
                },
            );
            let result = daemon
                .submit(
                    request_id.unwrap_or(DaemonClient::new_request_id()?),
                    request,
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        Some(Command::Job { job_id, socket }) => {
            let result = DaemonClient::local(socket.clone())?.status().await?;
            let pan_tui::ipc::IpcResult::Status { jobs, .. } = result else {
                unreachable!("DaemonClient::status validates the response variant");
            };
            let job = jobs
                .into_iter()
                .find(|job| job.id.0 == *job_id)
                .with_context(|| format!("job {job_id} was not found"))?;
            println!("{}", serde_json::to_string_pretty(&job)?);
            return Ok(());
        }
        Some(Command::Watch {
            after_revision,
            socket,
        }) => {
            let daemon = DaemonClient::local(socket.clone())?;
            let mut updates = daemon.subscribe(*after_revision).await?;
            while let Some(update) = updates.recv().await {
                let response = update.map_err(anyhow::Error::msg)?;
                println!("{}", serde_json::to_string(&response)?);
            }
            return Ok(());
        }
        Some(Command::Cancel { job_id, socket }) => {
            let result = DaemonClient::local(socket.clone())?
                .cancel(pan_tui::download::JobId(*job_id))
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        Some(Command::Submit {
            request,
            request_id,
            socket,
        }) => {
            let contents = std::fs::read(request)
                .with_context(|| format!("failed to read Job request: {}", request.display()))?;
            let request = serde_json::from_slice(&contents).context("invalid Job request JSON")?;
            let result = DaemonClient::local(socket.clone())?
                .submit(
                    request_id.unwrap_or_else(|| request_hash_id(&contents)),
                    request,
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        Some(Command::Auth { command }) => return run_auth(&cli, command).await,
        Some(Command::Tui) | None => {}
    }
    run_tui(cli).await
}

async fn run_auth(cli: &Cli, command: &AuthCommand) -> Result<()> {
    let config_root = default_config_root()?;
    let app_config = AppConfig::load(&config_root.join("config.toml"))?;
    let context = ProviderContext {
        config_root,
        network: app_config.network,
        config_overrides: google_runtime_overrides(cli)?,
    };
    let registry = provider_registry()?;
    match command {
        AuthCommand::Login { provider } => {
            let id = ProviderId::new(provider.trim())?;
            let factory = registry
                .get(&id)
                .with_context(|| format!("provider {id} is not registered"))?;
            let auth = factory
                .login(&context, ProviderLoginRequest::Interactive)
                .await?;
            if !auth.authenticated {
                bail!("provider {id} did not complete authentication");
            }
            let instance = factory.create(context.clone()).await?;
            let sources = instance.sources().await?;
            println!(
                "{} authentication is ready; {} source(s) available",
                factory.display_name(),
                sources.len()
            );
        }
        AuthCommand::Status { provider } => {
            let selected = provider
                .as_deref()
                .map(|value| ProviderId::new(value.trim()))
                .transpose()?;
            for (id, factory) in registry.iter() {
                if selected.as_ref().is_some_and(|selected| selected != id) {
                    continue;
                }
                let status = factory.auth_status(&context)?;
                println!("{}\t{}", id, auth_status_label(status));
            }
        }
        AuthCommand::Revoke { provider } => {
            let id = ProviderId::new(provider.trim())?;
            let factory = registry
                .get(&id)
                .with_context(|| format!("provider {id} is not registered"))?;
            factory.revoke(&context).await?;
            println!(
                "{} local authentication state removed",
                factory.display_name()
            );
        }
    }
    Ok(())
}

fn auth_status_label(status: ProviderAuthStatus) -> &'static str {
    match status {
        ProviderAuthStatus::SignedOut => "SignedOut",
        ProviderAuthStatus::Ready => "Ready",
        ProviderAuthStatus::ReauthRequired => "ReauthRequired",
        ProviderAuthStatus::EnterpriseAppRequired => "EnterpriseAppRequired",
        ProviderAuthStatus::Unsupported => "Unsupported",
    }
}

async fn run_tui(cli: Cli) -> Result<()> {
    let config_root = default_config_root()?;
    let settings_path = config_root.join("config.toml");
    let app_config = AppConfig::load(&settings_path)?;
    let config_overrides = google_runtime_overrides(&cli)?;
    let context = ProviderContext {
        config_root,
        network: app_config.network.clone(),
        config_overrides,
    };
    let registry = provider_registry()?;
    let daemon = DaemonClient::local(None)?;
    let client: Arc<dyn PanClient> = if daemon.socket().exists() {
        daemon
            .status()
            .await
            .context("pan-tui daemon socket exists but is not usable")?;
        Arc::new(DaemonPanClient::new(daemon).await?)
    } else {
        let provider_runtime = ProviderRuntime::from_registry(&registry, context).await;
        let state = StateStore::open(&default_state_db_path()?)?;
        let recovered = state.recover_interrupted()?;
        let recovery = state.load_interrupted_requests()?;
        if recovered > 0 {
            eprintln!("Recovered {recovered} interrupted download Job(s)");
        }
        let embedded = Arc::new(EmbeddedPanClient::new(
            provider_runtime,
            DownloadManager::persistent(state)?,
        ));
        for warning in embedded.resume(recovery).await {
            eprintln!("{warning}");
        }
        embedded
    };
    let providers: Vec<_> = client
        .provider_catalog()
        .await?
        .into_iter()
        .map(BrowserProvider::new)
        .collect();
    let active_provider = select_provider(&providers, cli.provider.as_deref())?;

    if cli.list {
        if !providers[active_provider].is_ready() {
            bail!(provider_status_message(
                providers[active_provider].id().as_str(),
                &providers[active_provider].status()
            ));
        }
        let sources = client
            .sources(&providers[active_provider].identity())
            .await?;
        let roots: Vec<_> = sources.into_iter().map(|source| source.root).collect();
        print_shared_items(&roots);
        return Ok(());
    }

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("interactive TUI requires a terminal; use --list for non-interactive access");
    }
    match run_browser(
        providers,
        active_provider,
        BrowserRuntime::new(
            client,
            settings_path,
            app_config.network,
            resolve_destination(cli.destination.or(app_config.download.destination))?,
            DownloadOptions {
                overwrite: cli.overwrite || app_config.download.overwrite,
                resume: !cli.no_resume && app_config.download.resume,
                concurrency: app_config.download.concurrency,
            },
        ),
    )
    .await?
    {
        BrowserOutcome::Quit => Ok(()),
    }
}

async fn run_daemon(cli: &Cli, socket: Option<PathBuf>) -> Result<()> {
    let config_root = default_config_root()?;
    let app_config = AppConfig::load(&config_root.join("config.toml"))?;
    let notifier_config = NotifierConfigFile::load(&config_root.join("notifiers.toml"))?;
    let mut notifier_registry = NotifierRegistry::default();
    notifier_registry.register(DingTalkNotifierFactory)?;
    notifier_registry.register(FeishuNotifierFactory)?;
    let notifier_load =
        notifier_registry.create_enabled_isolated(&notifier_config.notifiers, &app_config.network);
    for id in &notifier_load.failed_ids {
        eprintln!("Notifier {id} is disabled because its configuration is invalid");
    }
    let notifiers = notifier_load.notifiers;
    let context = ProviderContext {
        config_root,
        network: app_config.network.clone(),
        config_overrides: google_runtime_overrides(cli)?,
    };
    let registry = provider_registry()?;
    let providers = ProviderRuntime::from_registry(&registry, context).await;
    let state = StateStore::open(&default_state_db_path()?)?;
    let service = DaemonService::prepare(
        socket.unwrap_or(default_socket_path()?),
        state,
        providers,
        notifiers,
    )
    .await?;
    if service.recovered_jobs() > 0 {
        eprintln!(
            "Recovered {} interrupted download Job(s)",
            service.recovered_jobs()
        );
    }
    eprintln!("pan-tui daemon listening on {}", service.socket().display());
    service
        .run(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}

fn request_hash_id(contents: &[u8]) -> u64 {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(contents);
    u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix has 8 bytes"))
}

fn provider_identity(provider: &str, profile: &str) -> Result<ProviderIdentity> {
    Ok(ProviderIdentity::new(
        ProviderId::new(provider.trim())?,
        ProfileId::new(profile.trim())?,
    ))
}

fn remote_ref(provider: &str, profile: &str, source: &str, object: &str) -> Result<RemoteRef> {
    Ok(RemoteRef {
        identity: provider_identity(provider, profile)?,
        source_id: SourceId::new(source.trim())?,
        object_id: RemoteObjectId::new(object.trim())?,
    })
}

fn provider_registry() -> Result<ProviderRegistry> {
    let mut registry = ProviderRegistry::new();
    registry.register(GoogleProviderFactory)?;
    registry.register(AliyunProviderFactory)?;
    Ok(registry)
}

fn google_runtime_overrides(cli: &Cli) -> Result<BTreeMap<ProviderId, Vec<u8>>> {
    let credentials_requested =
        cli.credentials.is_some() || env::var_os("GOOGLE_APPLICATION_CREDENTIALS").is_some();
    let sources_requested = cli.sources.is_some() || env::var_os("PAN_TUI_SOURCES").is_some();
    if !credentials_requested && !sources_requested {
        return Ok(BTreeMap::new());
    }
    let config = GoogleProviderConfig {
        enabled: true,
        auth_mode: pan_tui::provider::google::GoogleAuthMode::ServiceAccount,
        credentials_file: resolve_credentials(cli.credentials.clone())?,
        token_file: PathBuf::from("google/oauth-token.json"),
        sources_file: Some(resolve_sources(cli.sources.clone())?),
        sources: Vec::new(),
        proxy: None,
    };
    let contents =
        toml::to_string_pretty(&config).context("failed to build Google CLI override")?;
    Ok(BTreeMap::from([(
        ProviderId::new("google").expect("static provider id is valid"),
        contents.into_bytes(),
    )]))
}

fn select_provider(providers: &[BrowserProvider], requested: Option<&str>) -> Result<usize> {
    if let Some(requested) = requested {
        let requested = ProviderId::new(requested.trim())?;
        if let Some(index) = providers
            .iter()
            .position(|provider| provider.id() == &requested)
        {
            return Ok(index);
        }
        bail!("provider {requested} is not registered");
    }
    Ok(providers
        .iter()
        .position(|provider| provider.id().as_str() == "google" && provider.is_ready())
        .or_else(|| providers.iter().position(BrowserProvider::is_ready))
        .or_else(|| {
            providers
                .iter()
                .position(|provider| provider.id().as_str() == "google")
        })
        .unwrap_or(0))
}

fn provider_status_message(name: &str, status: &ProviderStatus) -> String {
    match status {
        ProviderStatus::Disabled => format!("{name} is disabled"),
        ProviderStatus::NeedsConfig => {
            format!("{name} needs configuration in ~/.config/pan-tui/providers")
        }
        ProviderStatus::Connecting => format!("{name} is still connecting"),
        ProviderStatus::Ready => format!("{name} did not expose an instance"),
        ProviderStatus::ConfigError(message) | ProviderStatus::ConnectionError(message) => {
            message.clone()
        }
    }
}

fn resolve_destination(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home).join("Downloads"));
    }
    Ok(std::env::current_dir()
        .context("failed to resolve current directory for default destination")?
        .join("downloads"))
}

fn resolve_credentials(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let path = explicit
        .or_else(|| env::var_os("GOOGLE_APPLICATION_CREDENTIALS").map(PathBuf::from))
        .or_else(|| {
            env::var_os("HOME").map(|home| google_private_file(&PathBuf::from(home), "service-account.json"))
        })
        .context("service-account credentials not found; use --credentials or GOOGLE_APPLICATION_CREDENTIALS")?;
    if !path.is_file() {
        bail!(
            "service-account credentials do not exist: {}",
            path.display()
        );
    }
    Ok(path)
}

fn resolve_sources(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let path = explicit
        .or_else(|| env::var_os("PAN_TUI_SOURCES").map(PathBuf::from))
        .or_else(|| {
            env::var_os("HOME")
                .map(|home| google_private_file(&PathBuf::from(home), "sources.json"))
        })
        .context("sources file not found; use --sources or PAN_TUI_SOURCES")?;
    if !path.is_file() {
        bail!("sources file does not exist: {}", path.display());
    }
    Ok(path)
}

fn google_private_file(home: &std::path::Path, file: &str) -> PathBuf {
    home.join(".config/pan-tui/providers/google").join(file)
}

fn print_shared_items(items: &[RemoteItem]) {
    println!("Shared links/");
    for item in items {
        let kind = if item.is_folder() {
            "DIR "
        } else if !item.can_download() {
            "SKIP"
        } else {
            "FILE"
        };
        let suffix = item
            .size
            .map(|size| format!("  {size} bytes"))
            .unwrap_or_default();
        println!(
            "  [{kind}] {}{}{suffix}",
            item.name,
            if item.is_folder() { "/" } else { "" }
        );
    }
    println!("\n{} saved links (nothing downloaded)", items.len());
}

#[cfg(test)]
mod tests {
    use super::google_private_file;
    use std::path::Path;

    #[test]
    fn google_cli_defaults_use_provider_private_paths() {
        let home = Path::new("/tmp/pan-tui-home");
        assert_eq!(
            google_private_file(home, "service-account.json"),
            Path::new("/tmp/pan-tui-home/.config/pan-tui/providers/google/service-account.json")
        );
        assert_eq!(
            google_private_file(home, "sources.json"),
            Path::new("/tmp/pan-tui-home/.config/pan-tui/providers/google/sources.json")
        );
    }
}
