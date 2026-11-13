use crate::cli::problem_json::render_response_error;
use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::exit_code;
use clap::ArgMatches;
use clap::Command;
use clap::{Arg, builder::PossibleValuesParser};
use serde::Deserialize;
use serde_json::json;

pub(crate) fn command_config() -> Command {
    Command::new("update")
        .about("Change a namespace's security level (admin only)")
        .arg(Arg::new("name").required(true).help("Namespace name"))
        .arg(
            Arg::new("security-level")
                .long("security-level")
                .required(true)
                .value_parser(PossibleValuesParser::new([
                    "privileged",
                    "baseline",
                    "restricted",
                    "default",
                ]))
                .help("What its deployments may ask of the host; `default` follows the server's"),
        )
}

#[derive(Deserialize)]
struct NamespaceOutput {
    name: String,
    security_level: String,
}

/// The request body: `default` resets the level, sent as `null`.
fn body(level: &str) -> serde_json::Value {
    match level {
        "default" => json!({ "security_level": null }),
        level => json!({ "security_level": level }),
    }
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let name = args.get_one::<String>("name").expect("name is required");
    let level = args
        .get_one::<String>("security-level")
        .expect("security-level is required");
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .patch(format!("{}/namespaces/{}", api_url, name))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .json(&body(level))
        .send()
        .await;

    match request {
        Ok(response) => {
            if response.status().is_success() {
                match response.json::<NamespaceOutput>().await {
                    Ok(namespace) => println!(
                        "Namespace '{}' is now at the {} security level",
                        namespace.name, namespace.security_level
                    ),
                    Err(_) => println!("Namespace '{}' updated", name),
                }
                return;
            }

            let context = format!("Unable to update namespace '{}'", name);
            let code = render_response_error(&context, response).await;
            exit_code::from_http_status(code).exit();
        }
        Err(error) => {
            eprintln!("Failed to update namespace: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_resets_the_level() {
        assert_eq!(body("default"), json!({ "security_level": null }));
        assert_eq!(
            body("restricted"),
            json!({ "security_level": "restricted" })
        );
    }

    #[test]
    fn only_known_levels_are_accepted() {
        let parse = |level: &str| {
            command_config().try_get_matches_from(["update", "tenant-a", "--security-level", level])
        };
        assert!(parse("privileged").is_ok());
        assert!(parse("root").is_err());
    }
}
