//! `ring dashboard` — local dashboard with a reverse proxy to a remote API.
//!
//! Reads the current context from `config.toml` to figure out which Ring API
//! to talk to, pulls the bearer token from `auth.json` (or `RING_TOKEN` env),
//! then boots an axum server on the user's machine that:
//!
//! - serves the embedded SvelteKit build,
//! - proxies `/api/*` to the remote API, injecting the bearer token.
//!
//! Lets one operator monitor *any* Ring cluster from their laptop without
//! exposing the dashboard from the server side.

use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::dashboard::{Mode, UpstreamApi};
use clap::{Arg, ArgMatches, Command};
use std::process::exit;

pub(crate) fn command_config() -> Command {
    Command::new("dashboard")
        .about("Open a local web dashboard that points at the current context's API")
        .arg(
            Arg::new("listen")
                .long("listen")
                .help("Address to bind the local dashboard to")
                .default_value("127.0.0.1:3031"),
        )
        .arg(
            Arg::new("no-open")
                .long("no-open")
                .help("Do not open the browser automatically")
                .action(clap::ArgAction::SetTrue),
        )
}

pub(crate) async fn execute(args: &ArgMatches, mut config: Config, context: String) {
    let listen = args
        .get_one::<String>("listen")
        .cloned()
        .unwrap_or_else(|| "127.0.0.1:3031".to_string());
    let no_open = args.get_flag("no-open");

    let api_url = config.get_api_url();
    let auth = load_auth_config(context);
    if auth.token.is_empty() {
        eprintln!("No auth token available. Run `ring login` first, or set RING_TOKEN.");
        exit(1);
    }

    let upstream = UpstreamApi {
        url: api_url.clone(),
        bearer_token: Some(auth.token),
    };
    let mode = Mode::Local { upstream };

    let url = format!("http://{}", listen);
    println!("Dashboard:  {}", url);
    println!("Upstream:   {}", api_url);

    // Open the browser before serving — Vite-style. We do it on a separate
    // task so a flaky `xdg-open` doesn't keep the server from starting.
    if !no_open {
        let url_to_open = url.clone();
        tokio::spawn(async move {
            // Tiny grace period so the listener is up before the browser hits it.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let _ = open_browser(&url_to_open);
        });
    }

    if let Err(e) = crate::dashboard::server::serve(mode, &listen).await {
        eprintln!("Dashboard failed: {}", e);
        exit(1);
    }
}

/// Best-effort browser opener — silently does nothing if the platform tool
/// is missing. We avoid pulling the `open`/`webbrowser` crate just for this.
fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn()?;
    }
    Ok(())
}
