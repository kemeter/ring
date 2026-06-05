use crate::cli::problem_json::render_response_error;
use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::exit_code;
use clap::Arg;
use clap::ArgAction;
use clap::ArgMatches;
use clap::Command;
use serde::{Deserialize, Serialize};

pub(crate) fn command_config() -> Command {
    Command::new("create")
        .about("Register a webhook subscriber")
        .arg(
            Arg::new("url")
                .required(true)
                .help("Target URL that receives signed POSTs"),
        )
        .arg(
            Arg::new("event")
                .short('e')
                .long("event")
                .action(ArgAction::Append)
                .help(
                    "Event kind to subscribe to, repeatable. Accepts an exact kind \
                     (deployment.scaled), a family wildcard (deployment.*) or '*'. \
                     Omit for all events",
                ),
        )
        .arg(
            Arg::new("secret")
                .short('s')
                .long("secret")
                .help("HMAC secret (omit to let Ring generate one)"),
        )
}

#[derive(Serialize)]
struct WebhookInput {
    url: String,
    events: Vec<String>,
    secret: Option<String>,
}

#[derive(Deserialize)]
struct WebhookCreated {
    id: String,
    url: String,
    events: Vec<String>,
    secret: Option<String>,
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let url = args.get_one::<String>("url").unwrap();
    let events: Vec<String> = args
        .get_many::<String>("event")
        .map(|v| v.cloned().collect())
        .unwrap_or_default();
    let secret = args.get_one::<String>("secret").cloned();

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let input = WebhookInput {
        url: url.clone(),
        events,
        secret,
    };

    let request = client
        .post(format!("{}/webhooks", api_url))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .json(&input)
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                let created: WebhookCreated = response.json().await.unwrap();
                // Human summary + secret on stderr (the secret won't be shown
                // again); the id on stdout so it can be captured.
                eprintln!("Webhook registered for {}", created.url);
                eprintln!(
                    "  events: {}",
                    if created.events.is_empty() {
                        "all".to_string()
                    } else {
                        created.events.join(", ")
                    }
                );
                if let Some(secret) = created.secret {
                    eprintln!("  secret: {} (copy it now — not shown again)", secret);
                }
                println!("{}", created.id);
            } else {
                let context = format!("Failed to create webhook for '{}'", url);
                let code = render_response_error(&context, response).await;
                exit_code::from_http_status(code).exit();
            }
        }
        Err(error) => {
            eprintln!("Failed to create webhook: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
