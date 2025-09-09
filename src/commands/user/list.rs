use crate::config::config::load_auth_config;
use crate::config::config::Config;
use clap::ArgMatches;
use clap::Command;
use cli_table::{format::Justify, print_stdout, Table, WithTitle};
use serde::{Deserialize, Serialize};

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("list")
        .about("List users")
}

#[derive(Table)]
struct UserTableItem {
    #[table(title = "ID", justify = "Justify::Right")]
    id: String,

    #[table(title = "Created at")]
    created_at: String,

    #[table(title = "Updated at")]
    updated_at: String,

    #[table(title = "Status")]
    status: String,

    #[table(title = "Username")]
    username: String,

    #[table(title = "Login at")]
    login_at: String,
}

pub(crate) async fn execute(_args: &ArgMatches, mut configuration: Config, client: &reqwest::Client) {
    let mut users = vec![];
    let api_url = configuration.get_api_url();

    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .get(&format!("{}/users", api_url))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            if response.status() != 200 {
                return eprintln!("Unable to fetch users: {}", response.status());
            }

            let users_list: Vec<UserDto> = response.json().await.unwrap_or(vec![]);

            for user in users_list {
                users.push(
                    UserTableItem {
                        id: user.id,
                        created_at: user.created_at,
                        updated_at: user.updated_at,
                        status: user.status,
                        username: user.username,
                        login_at: user.login_at
                    },
                )
            }

            print_stdout(users.with_title()).expect("");
        }
        Err(err) => {
            eprintln!("Error fetching users: {}", err);
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct UserDto {
    id: String,
    created_at: String,
    updated_at: String,
    status: String,
    username: String,
    login_at: String,
}
