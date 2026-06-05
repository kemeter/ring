use crate::cli::problem_json::render_response_error;
use crate::cli::style;
use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;

pub(crate) fn command_config() -> Command {
    Command::new("delete")
        .about("Delete a webhook subscriber")
        .arg(Arg::new("id").required(true).help("Webhook id"))
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args.get_one::<String>("id").unwrap();

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .delete(format!("{}/webhooks/{}", api_url, id))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                style::print_success(&format!("Webhook '{}' deleted", id));
            } else {
                let context = format!("Failed to delete webhook '{}'", id);
                let code = render_response_error(&context, response).await;
                exit_code::from_http_status(code).exit();
            }
        }
        Err(error) => {
            eprintln!("Failed to delete webhook: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
