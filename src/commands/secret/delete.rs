use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde::Deserialize;
use std::io::{self, Write};

use crate::config::config::Config;
use crate::config::config::load_auth_config;
use crate::exit_code;

pub(crate) fn command_config() -> Command {
    Command::new("delete")
        .about("Delete a secret")
        .arg(Arg::new("id").required(true).help("Secret ID"))
        .arg(
            Arg::new("force")
                .short('f')
                .long("force")
                .help("Force deletion even if referenced by deployments")
                .action(clap::ArgAction::SetTrue),
        )
}

#[derive(Deserialize)]
struct ConflictResponse {
    #[allow(dead_code)]
    error: String,
    deployments: Vec<String>,
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args.get_one::<String>("id").unwrap();
    let force = args.get_flag("force");
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let url = if force {
        format!("{}/secrets/{}?force=true", api_url, id)
    } else {
        format!("{}/secrets/{}", api_url, id)
    };

    let request = client
        .delete(&url)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            match status.as_u16() {
                204 => println!("Secret {} deleted", id),
                404 => {
                    eprintln!("Secret {} not found", id);
                    exit_code::from_http_status(404).exit();
                }
                409 => {
                    // Secret is referenced by deployments
                    if let Ok(conflict) = response.json::<ConflictResponse>().await {
                        println!("This secret is referenced by the following deployments:");
                        for dep in &conflict.deployments {
                            println!("  - {}", dep);
                        }
                        println!();

                        print!("These deployments will fail on next restart. Continue? [y/N] ");
                        io::stdout().flush().unwrap();

                        let mut input = String::new();
                        if io::stdin().read_line(&mut input).is_ok() {
                            if input.trim().to_lowercase() == "y" {
                                // Retry with force
                                let force_url = format!("{}/secrets/{}?force=true", api_url, id);
                                let retry = client
                                    .delete(&force_url)
                                    .header(
                                        "Authorization",
                                        format!("Bearer {}", auth_config.token),
                                    )
                                    .send()
                                    .await;

                                match retry {
                                    Ok(resp) => {
                                        let retry_status = resp.status();
                                        if retry_status.as_u16() == 204 {
                                            println!("Secret {} deleted", id);
                                        } else {
                                            eprintln!("Failed to delete secret: {}", retry_status);
                                            exit_code::from_http_status(retry_status.as_u16())
                                                .exit();
                                        }
                                    }
                                    Err(err) => {
                                        eprintln!("Failed to delete secret: {}", err);
                                        exit_code::from_reqwest_error(&err).exit();
                                    }
                                }
                            } else {
                                println!("Cancelled");
                            }
                        }
                    } else {
                        eprintln!(
                            "Secret is referenced by deployments. Use -f to force deletion."
                        );
                        exit_code::from_http_status(409).exit();
                    }
                }
                _ => {
                    eprintln!("Failed to delete secret: {}", status);
                    exit_code::from_http_status(status.as_u16()).exit();
                }
            }
        }
        Err(error) => {
            eprintln!("Failed to delete secret: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
