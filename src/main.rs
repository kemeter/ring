#![allow(clippy::module_inception)]

use clap::{Arg, Command};
use std::env;
use std::process::Command as BaseCommand;

#[macro_use]
extern crate tracing;
mod cli;
mod commands;
mod hypervisor;
mod models;
mod runtime;
mod scheduler;

mod api;

mod events;

mod webhook;

mod config;

mod dashboard;
mod database;
mod exit_code;
mod serializer;
mod telemetry;
mod utils;

#[cfg(test)]
mod fixtures;

/// Build a fresh `EnvFilter` from `RUST_LOG` (falling back to `info`). Called
/// per layer so each layer filters independently rather than relying on the
/// `.with()` order — an `EnvFilter` added as a bare layer would otherwise act
/// globally over the whole registry.
fn env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

/// Build the console `fmt` layer, honouring `RING_LOG_FORMAT`. `json` emits one
/// JSON object per event (for log shippers / structured ingestion); anything
/// else (unset, `text`, `pretty`) keeps the default human-readable format. The
/// layer is boxed so both formats share one return type. Each format carries its
/// own `EnvFilter` so it filters independently of layer ordering (see
/// [`env_filter`]).
fn console_fmt_layer<S>() -> Box<dyn tracing_subscriber::Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use tracing_subscriber::layer::Layer;

    match env::var("RING_LOG_FORMAT").as_deref() {
        Ok("json") => tracing_subscriber::fmt::layer()
            .json()
            .with_filter(env_filter())
            .boxed(),
        _ => tracing_subscriber::fmt::layer()
            .with_filter(env_filter())
            .boxed(),
    }
}

/// Initialise `tracing`. Always installs the console `fmt` layer; when
/// `telemetry` is `Some` (server start) it also attaches whichever of the
/// subscriber-layer signals — traces and logs — are enabled, returning a guard
/// that owns their providers (kept alive for the process). Metrics is not a
/// subscriber layer; it is built later, once the stats cache exists, and
/// attached to the same guard via [`telemetry::OtelGuard::attach_meter`].
///
/// Each signal is independent and non-fatal: if an OTLP exporter can't be built
/// the error is logged and that signal is skipped, but the console subscriber
/// (and the other signals) still come up. Called exactly once, after the config
/// is loaded, so every non-server command stays console-only at zero cost.
fn init_tracing(
    telemetry: Option<&config::server::TelemetryConfig>,
) -> Option<telemetry::OtelGuard> {
    use tracing_subscriber::layer::{Layer, SubscriberExt};
    use tracing_subscriber::util::SubscriberInitExt;

    let registry = tracing_subscriber::registry().with(console_fmt_layer());

    let Some(cfg) = telemetry else {
        registry.init();
        return None;
    };

    let mut guard = telemetry::OtelGuard::default();

    // Outcome messages are buffered and emitted *after* `.init()`: logging here
    // would go nowhere because no subscriber is active until the registry is
    // initialised. `(is_error, message)` — errors are rare (a bad exporter) but
    // must still surface once logging is live.
    let mut messages: Vec<(bool, String)> = Vec::new();

    // Traces: an OTel span layer over the registry.
    let trace_layer = if cfg.traces.enabled {
        match telemetry::build_tracer(&cfg.traces) {
            Ok((layer, tracer_guard)) => {
                guard.absorb(tracer_guard);
                messages.push((
                    false,
                    format!(
                        "telemetry: OTLP trace export enabled ({})",
                        cfg.traces.endpoint
                    ),
                ));
                Some(layer.with_filter(env_filter()))
            }
            Err(e) => {
                messages.push((true, format!(
                    "telemetry: failed to initialise trace exporter, continuing without traces: {e}"
                )));
                None
            }
        }
    } else {
        None
    };

    // Logs: a tracing→OTLP bridge layer, in addition to the console.
    let log_layer = if cfg.logs.enabled {
        match telemetry::build_logger(&cfg.logs) {
            Ok((layer, provider)) => {
                guard.attach_logger(provider);
                messages.push((
                    false,
                    format!("telemetry: OTLP log export enabled ({})", cfg.logs.endpoint),
                ));
                Some(layer.with_filter(env_filter()))
            }
            Err(e) => {
                messages.push((true, format!(
                    "telemetry: failed to initialise log exporter, continuing without OTLP logs: {e}"
                )));
                None
            }
        }
    } else {
        None
    };

    registry.with(trace_layer).with(log_layer).init();

    // Now that the subscriber is live, replay the buffered outcomes.
    for (is_error, msg) in messages {
        if is_error {
            error!("{msg}");
        } else {
            info!("{msg}");
        }
    }

    Some(guard)
}

/// Build the CLI tree.
///
/// Kept as its own function (rather than inlined in `main`) because
/// `ring completions` generates its scripts from this exact tree — one
/// definition, so the completions can never drift from the real commands.
fn build_cli() -> Command {
    Command::new("ring")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Mlanawo Mbechezi <mlanawo.mbechezi@kemeter.io>")
        .about("The ring to rule them all")
        .arg(
            Arg::new("context")
                .required(false)
                .help("Sets the context to use (e.g., development, staging, production)")
                .long("context")
                .short('c'),
        )
        .arg(
            Arg::new("config")
                .required(false)
                .help("Path to a config.toml to load (overrides RING_CONFIG_FILE and the default)")
                .long("config"),
        )
        .subcommand(commands::context::command_config())
        .subcommand(commands::init::command_config())
        .subcommand(
            Command::new("server")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(commands::server::command_config()),
        )
        .subcommand(commands::apply::command_config())
        .subcommand(commands::dashboard::command_config())
        .subcommand(commands::doctor::command_config())
        .subcommand(commands::login::command_config())
        .subcommand(commands::logout::command_config())
        .subcommand(
            Command::new("deployment")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                // .args(push_args())
                .subcommand(commands::deployment::list::command_config())
                .subcommand(commands::deployment::inspect::command_config())
                .subcommand(commands::deployment::delete::command_config())
                .subcommand(commands::deployment::logs::command_config())
                .subcommand(commands::deployment::events::command_config())
                .subcommand(commands::deployment::metrics::command_config())
                .subcommand(commands::deployment::health_checks::command_config()),
        )
        .subcommand(
            Command::new("namespace")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(commands::namespace::create::command_config())
                .subcommand(commands::namespace::list::command_config())
                .subcommand(commands::namespace::prune::command_config())
                .subcommand(commands::namespace::audit::command_config())
                .subcommand(commands::namespace::delete::command_config()),
        )
        .subcommand(
            Command::new("node")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(commands::node::get::command_config()),
        )
        .subcommand(
            Command::new("config")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(commands::config::list::command_config())
                .subcommand(commands::config::inspect::command_config())
                .subcommand(commands::config::delete::command_config()),
        )
        .subcommand(
            Command::new("user")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(commands::user::list::command_config())
                .subcommand(commands::user::create::command_config())
                .subcommand(commands::user::update::command_config())
                .subcommand(commands::user::delete::command_config()),
        )
        .subcommand(
            Command::new("secret")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(commands::secret::list::command_config())
                .subcommand(commands::secret::create::command_config())
                .subcommand(commands::secret::delete::command_config()),
        )
        .subcommand(
            Command::new("token")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(commands::token::list::command_config())
                .subcommand(commands::token::create::command_config())
                .subcommand(commands::token::revoke::command_config())
                .subcommand(commands::token::rotate::command_config()),
        )
        .subcommand(
            Command::new("webhook")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(commands::webhook::list::command_config())
                .subcommand(commands::webhook::create::command_config())
                .subcommand(commands::webhook::delete::command_config())
                .subcommand(commands::webhook::inspect::command_config()),
        )
        .subcommand(commands::completions::command_config())
}

#[tokio::main]
async fn main() {
    let app = build_cli();

    // `completions` only needs the CLI tree, never the config or a client, so
    // it is dispatched before `load_config`: generating a script must work on a
    // machine that has never run `ring init`.
    let matches = app.clone().get_matches();
    if let Some(("completions", sub_matches)) = matches.subcommand() {
        commands::completions::execute(sub_matches, app);
        return;
    }

    let context = matches
        .get_one::<String>("context")
        .map(|s| s.as_str())
        .unwrap_or("default");

    let config_file = matches.get_one::<String>("config").map(|s| s.as_str());

    let subcommand_name = matches.subcommand();
    let config = config::config::load_config(context, config_file);

    // OTLP export is a server concern: enable it only for `server start`, where
    // the daemon runs long enough for batching to matter. Every other command
    // initialises console logging only. The guard lives for the whole of `main`
    // so every signal flushes on exit. Traces and logs are installed now (they
    // are subscriber layers); metrics is attached later, once the stats cache
    // exists (see `server::execute`).
    let is_server_start = matches!(
        subcommand_name,
        Some(("server", sub)) if sub.subcommand().map(|(n, _)| n).unwrap_or("start") == "start"
    );
    let mut telemetry_guard = init_tracing(is_server_start.then_some(&config.server.telemetry));

    let client = reqwest::Client::builder()
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("Content-Type", "application/json".parse().unwrap());
            headers
        })
        .build()
        .expect("Failed to build HTTP client");

    match subcommand_name {
        Some(("context", sub_matches)) => {
            commands::context::execute(sub_matches, config);
        }
        Some(("init", sub_matches)) => {
            commands::init::init(sub_matches);
        }
        Some(("server", sub_matches)) => {
            let server_command = sub_matches.subcommand().unwrap_or(("start", sub_matches));
            if let ("start", sub_matches) = server_command {
                commands::server::execute(sub_matches, config, telemetry_guard.as_mut()).await
            }
        }
        Some(("apply", sub_matches)) => {
            commands::apply::apply(sub_matches, config, &client).await;
        }
        Some(("dashboard", sub_matches)) => {
            commands::dashboard::execute(sub_matches, config, context.to_string()).await;
        }
        Some(("doctor", sub_matches)) => {
            commands::doctor::execute(sub_matches, config);
        }
        Some(("deployment", sub_matches)) => {
            let deployment_command = sub_matches.subcommand().unwrap_or(("list", sub_matches));
            match deployment_command {
                ("list", sub_matches) => {
                    commands::deployment::list::execute(sub_matches, config, &client).await;
                }
                ("inspect", sub_matches) => {
                    commands::deployment::inspect::execute(sub_matches, config, &client).await;
                }
                ("delete", sub_matches) => {
                    commands::deployment::delete::execute(sub_matches, config, &client).await;
                }

                ("logs", sub_matches) => {
                    commands::deployment::logs::execute(sub_matches, config, &client).await;
                }
                ("events", sub_matches) => {
                    commands::deployment::events::execute(sub_matches, config, &client).await;
                }
                ("metrics", sub_matches) => {
                    commands::deployment::metrics::execute(sub_matches, config, &client).await;
                }
                ("health-checks", sub_matches) => {
                    commands::deployment::health_checks::execute(sub_matches, config, &client)
                        .await;
                }
                _ => {}
            }
        }
        Some(("namespace", sub_matches)) => {
            let namespace_command = sub_matches.subcommand().unwrap_or(("list", sub_matches));
            match namespace_command {
                ("create", sub_matches) => {
                    commands::namespace::create::execute(sub_matches, config, &client).await;
                }
                ("list", sub_matches) => {
                    commands::namespace::list::execute(sub_matches, config, &client).await;
                }
                ("prune", sub_matches) => {
                    commands::namespace::prune::execute(sub_matches, config, &client).await;
                }
                ("audit", sub_matches) => {
                    commands::namespace::audit::execute(sub_matches, config, &client).await;
                }
                ("delete", sub_matches) => {
                    commands::namespace::delete::execute(sub_matches, config, &client).await;
                }
                _ => {}
            }
        }
        Some(("node", sub_matches)) => {
            let node_command = sub_matches.subcommand().unwrap_or(("get", sub_matches));
            if let ("get", sub_matches) = node_command {
                commands::node::get::execute(sub_matches, config, &client).await;
            }
        }
        Some(("config", sub_matches)) => {
            let config_command = sub_matches.subcommand().unwrap_or(("list", sub_matches));
            match config_command {
                ("list", sub_matches) => {
                    commands::config::list::execute(sub_matches, config, &client).await;
                }
                ("inspect", sub_matches) => {
                    commands::config::inspect::execute(sub_matches, config, &client).await;
                }
                ("delete", sub_matches) => {
                    commands::config::delete::execute(sub_matches, config, &client).await;
                }
                _ => {}
            }
        }
        Some(("login", sub_matches)) => {
            commands::login::execute(sub_matches, config, &client).await;
        }
        Some(("logout", sub_matches)) => {
            commands::logout::execute(sub_matches, config, &client).await;
        }
        Some(("user", sub_matches)) => {
            let user_command = sub_matches.subcommand().unwrap_or(("list", sub_matches));
            match user_command {
                ("list", sub_matches) => {
                    commands::user::list::execute(sub_matches, config, &client).await;
                }
                ("create", sub_matches) => {
                    commands::user::create::execute(sub_matches, config, &client).await;
                }
                ("update", sub_matches) => {
                    commands::user::update::execute(sub_matches, config, &client).await;
                }
                ("delete", sub_matches) => {
                    commands::user::delete::execute(sub_matches, config, &client).await;
                }
                _ => {}
            }
        }
        Some(("secret", sub_matches)) => {
            let secret_command = sub_matches.subcommand().unwrap_or(("list", sub_matches));
            match secret_command {
                ("list", sub_matches) => {
                    commands::secret::list::execute(sub_matches, config, &client).await;
                }
                ("create", sub_matches) => {
                    commands::secret::create::execute(sub_matches, config, &client).await;
                }
                ("delete", sub_matches) => {
                    commands::secret::delete::execute(sub_matches, config, &client).await;
                }
                _ => {}
            }
        }
        Some(("token", sub_matches)) => {
            let token_command = sub_matches.subcommand().unwrap_or(("list", sub_matches));
            match token_command {
                ("list", sub_matches) => {
                    commands::token::list::execute(sub_matches, config, &client).await;
                }
                ("create", sub_matches) => {
                    commands::token::create::execute(sub_matches, config, &client).await;
                }
                ("revoke", sub_matches) => {
                    commands::token::revoke::execute(sub_matches, config, &client).await;
                }
                ("rotate", sub_matches) => {
                    commands::token::rotate::execute(sub_matches, config, &client).await;
                }
                _ => {}
            }
        }
        Some(("webhook", sub_matches)) => {
            let webhook_command = sub_matches.subcommand().unwrap_or(("list", sub_matches));
            match webhook_command {
                ("list", sub_matches) => {
                    commands::webhook::list::execute(sub_matches, config, &client).await;
                }
                ("create", sub_matches) => {
                    commands::webhook::create::execute(sub_matches, config, &client).await;
                }
                ("delete", sub_matches) => {
                    commands::webhook::delete::execute(sub_matches, config, &client).await;
                }
                ("inspect", sub_matches) => {
                    commands::webhook::inspect::execute(sub_matches, config, &client).await;
                }
                _ => {}
            }
        }

        _ => {
            let process_args: Vec<String> = env::args().collect();
            let process_name = process_args[0].as_str().to_owned();

            let mut subprocess = BaseCommand::new(process_name.as_str())
                .arg("--help")
                .spawn()
                .expect("failed to execute process");

            subprocess.wait().expect("failed to wait for process");
        }
    }
}
