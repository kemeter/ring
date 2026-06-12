use crate::cli::problem_json::http_error_global_list;
use crate::cli::style;
use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::exit_code;
use clap::ArgMatches;
use clap::Command;
use cli_table::{Table, WithTitle, format::Justify};
use serde::{Deserialize, Serialize};

pub(crate) fn command_config() -> Command {
    Command::new("list").about("List users")
}

#[derive(Table)]
struct UserTableItem {
    #[table(title = "ID", justify = "Justify::Right")]
    id: String,

    #[table(title = "Created at (UTC)")]
    created_at: String,

    #[table(title = "Updated at (UTC)")]
    updated_at: String,

    #[table(title = "Status")]
    status: String,

    #[table(title = "Username")]
    username: String,

    #[table(title = "Login at (UTC)")]
    login_at: String,
}

pub(crate) async fn execute(
    _args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let mut users = vec![];
    let api_url = configuration.get_api_url();

    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .get(format!("{}/users", api_url))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status != 200 {
                style::print_error(&http_error_global_list(status.as_u16(), "users"));
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let users_list: Vec<UserDto> = match response.json().await {
                Ok(list) => list,
                Err(e) => {
                    eprintln!("Failed to parse user list: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            for user in users_list {
                users.push(UserTableItem {
                    id: user.id,
                    created_at: style::format_date(&user.created_at),
                    updated_at: style::format_date(user.updated_at.as_deref().unwrap_or_default()),
                    status: user.status,
                    username: user.username,
                    login_at: style::format_date(user.login_at.as_deref().unwrap_or_default()),
                })
            }

            style::print_table(users.with_title());
        }
        Err(err) => {
            eprintln!("Error fetching users: {}", err);
            exit_code::from_reqwest_error(&err).exit();
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct UserDto {
    id: String,
    created_at: String,
    updated_at: Option<String>,
    status: String,
    username: String,
    login_at: Option<String>,
}
