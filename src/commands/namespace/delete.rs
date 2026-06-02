use crate::cli::problem_json::render_response_error;
use crate::config::config::{Config, load_auth_config};
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;

pub(crate) fn command_config() -> Command {
    Command::new("delete")
        .about("Delete an empty namespace (refuses if it still has resources)")
        .arg(Arg::new("name").required(true).help("Namespace name"))
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let name = args.get_one::<String>("name").expect("name is required");
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .delete(format!("{}/namespaces/{}", api_url, name))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status == 204 {
                println!("Namespace '{}' deleted", name);
                return;
            }

            let context = format!("Unable to delete namespace '{}'", name);
            let code = render_response_error(&context, response).await;
            exit_code::from_http_status(code).exit();
        }
        Err(error) => {
            eprintln!("Failed to delete namespace: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
