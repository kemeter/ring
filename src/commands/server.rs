use crate::api::server as ApiServer;
use crate::commands::style;
use crate::runtime::cloud_hypervisor::CloudHypervisorLifecycle;
use crate::runtime::docker;
use crate::runtime::docker::docker_lifecycle::DockerLifecycle;
use crate::runtime::lifecycle_trait::RuntimeLifecycle;
use clap::ArgMatches;
use clap::{Arg, ArgAction, Command};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task;

use crate::config::config::Config;
use crate::database::{get_database_pool, migrate_from_refinery_if_needed};
use crate::scheduler::docker_events::{DockerEvent, start_event_listener};
use crate::scheduler::intentional_shutdowns::IntentionalShutdowns;
use crate::scheduler::scheduler::schedule;

pub(crate) fn command_config() -> Command {
    Command::new("start").arg(
        Arg::new("dashboard")
            .long("dashboard")
            .help("Enable the embedded web dashboard for this run (overrides config.toml)")
            .action(ArgAction::SetTrue),
    )
}

pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config) {
    // Dashboard activation precedence, strongest first:
    //   1. `--dashboard` CLI flag (explicit one-off)
    //   2. `RING_DASHBOARD=true|1|yes` env var (systemd / docker / CI)
    //   3. `[dashboard] enabled = true` in config.toml (persistent)
    // The env var lets operators flip the dashboard on without rewriting
    // the on-disk config — same spirit as `RING_TOKEN` / `RING_SECRET_KEY`.
    if args.get_flag("dashboard") {
        configuration.dashboard.enabled = true;
    } else if let Ok(val) = std::env::var("RING_DASHBOARD")
        && matches!(
            val.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    {
        configuration.dashboard.enabled = true;
    }
    // Optional override of the bind address; useful in containers where
    // the default `127.0.0.1:3031` is unreachable from the host.
    if let Ok(addr) = std::env::var("RING_DASHBOARD_LISTEN")
        && !addr.trim().is_empty()
    {
        configuration.dashboard.listen_address = addr;
    }

    // Validate the encryption key up front. Anything that touches a
    // secret (deployment env vars with `secretRef`, `POST /secrets`, ...)
    // would panic later on a missing or malformed key; failing here gives
    // operators a single, clear log line and a non-zero exit, instead of
    // a 500 the first time someone applies a manifest.
    if let Err(e) = crate::models::secret::try_load_encryption_key() {
        error!("Refusing to start: {}", e);
        std::process::exit(1);
    }

    let pool = get_database_pool().await;

    migrate_from_refinery_if_needed(&pool).await;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Could not execute database migrations.");

    let docker = docker::connect().expect("Failed to connect to Docker");
    info!("Connected to Docker");

    let intentional_shutdowns = IntentionalShutdowns::new();

    let mut runtimes_map: HashMap<String, Arc<dyn RuntimeLifecycle>> = HashMap::new();
    runtimes_map.insert(
        "docker".to_string(),
        Arc::new(DockerLifecycle::new(docker, intentional_shutdowns.clone())),
    );

    let ch_runtime_config =
        crate::runtime::cloud_hypervisor::CloudHypervisorRuntimeConfig::from_user_config(
            &configuration.runtime.cloud_hypervisor,
        );
    info!(
        "Cloud Hypervisor runtime: binary={}, firmware={}, socket_dir={}, seccomp={:?}",
        ch_runtime_config.binary_path,
        ch_runtime_config.firmware_path,
        ch_runtime_config.socket_dir,
        ch_runtime_config.seccomp,
    );
    let ch_lifecycle = CloudHypervisorLifecycle::new(ch_runtime_config);
    let _ch_log_rotator = ch_lifecycle.spawn_console_log_rotator();
    runtimes_map.insert("cloud-hypervisor".to_string(), Arc::new(ch_lifecycle));
    info!(
        "Registered runtimes: {:?}",
        runtimes_map.keys().collect::<Vec<_>>()
    );

    let runtimes = std::sync::Arc::new(runtimes_map);

    let (event_tx, event_rx) = mpsc::channel::<DockerEvent>(1024);
    let docker_for_events =
        docker::connect().expect("Failed to connect to Docker for event listener");
    let event_listener_handler = task::spawn(start_event_listener(event_tx, docker_for_events));

    let api_server_handler = task::spawn(ApiServer::start(
        pool.clone(),
        configuration.clone(),
        runtimes.clone(),
    ));

    // Embedded dashboard — only spawned when explicitly enabled in config,
    // so the default surface stays unchanged for existing users. Proxies
    // to its own API over loopback so the browser sees a single origin.
    if configuration.dashboard.enabled {
        let listen = configuration.dashboard.listen_address.clone();
        let api_port = configuration.api.port;
        task::spawn(async move {
            let mode = crate::dashboard::Mode::Embedded { api_port };
            if let Err(e) = crate::dashboard::server::serve(mode, &listen).await {
                eprintln!("Dashboard task failed: {}", e);
            }
        });
    }

    print_startup_banner(&configuration, runtimes.as_ref());

    let scheduler_handler = task::spawn(schedule(
        pool,
        configuration,
        runtimes,
        event_rx,
        intentional_shutdowns,
    ));

    if let Err(e) = api_server_handler.await {
        eprintln!("API server task failed: {}", e);
    }
    if let Err(e) = scheduler_handler.await {
        eprintln!("Scheduler task failed: {}", e);
    }
    if let Err(e) = event_listener_handler.await {
        eprintln!("Docker event listener task failed: {}", e);
    }
}

/// Print a concise, human-readable summary of where the server is reachable
/// once it has started: API endpoint, embedded dashboard (if enabled), and the
/// registered runtimes. Goes to stdout (not the logger) so it shows regardless
/// of `RUST_LOG`, and uses the shared `style` palette so colour is dropped in
/// pipes / under `NO_COLOR`.
fn print_startup_banner(
    configuration: &Config,
    runtimes: &std::collections::HashMap<String, Arc<dyn RuntimeLifecycle>>,
) {
    let version = env!("CARGO_PKG_VERSION");
    let scheme = &configuration.api.scheme;
    let port = configuration.api.port;
    let host = configuration.host.as_str();

    // Vite-style Local / Network lines. When bound to all interfaces we show
    // both loopback and the machine's LAN IP (the actually-reachable address);
    // a specific bind host shows only that one, on the matching line.
    let url = |h: &str| format!("{}://{}:{}", scheme, h, port);
    let lan_ip = local_ip_address::local_ip().ok().map(|ip| ip.to_string());

    let (local, network): (Option<String>, Option<String>) = match host {
        "0.0.0.0" => (Some(url("127.0.0.1")), lan_ip.map(|ip| url(&ip))),
        "127.0.0.1" | "localhost" => (Some(url(host)), None),
        other => (None, Some(url(other))),
    };

    let mut names: Vec<&String> = runtimes.keys().collect();
    names.sort();
    let runtime_list = names
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let arrow = style::success("➜");
    println!();
    println!("  {} {}  ready", style::success("Ring"), version);
    println!();
    if let Some(u) = &local {
        println!("  {}  Local:     {}", arrow, u);
    }
    if let Some(u) = &network {
        println!("  {}  Network:   {}", arrow, u);
    } else if host == "0.0.0.0" {
        println!("  {}  Network:   (no LAN address detected)", arrow);
    }
    if configuration.dashboard.enabled {
        println!(
            "  {}  Dashboard: http://{}",
            arrow, configuration.dashboard.listen_address
        );
    } else {
        println!("  {}  Dashboard: disabled (enable with --dashboard)", arrow);
    }
    println!("  {}  Runtimes:  {}", arrow, runtime_list);
    println!();
}
