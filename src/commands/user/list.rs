use clap::App;
use clap::SubCommand;
use clap::ArgMatches;
use cli_table::{format::Justify, print_stdout, Table, WithTitle};
use serde_json::Result;
use serde::{Serialize, Deserialize};
use crate::config::config::Config;
use crate::config::config::load_auth_config;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("user:list")
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

pub(crate) fn execute(_args: &ArgMatches, mut configuration: Config) {
    let mut users = vec![];
    let api_url = configuration.get_api_url();

    let auth_config = load_auth_config();

    let response = ureq::get(&format!("{}/users", api_url))
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .send_json({});

    let response_content = response.unwrap().into_string().unwrap();

    let value: Result<Vec<UserDto>> = serde_json::from_str(&response_content);
    let users_list = value.unwrap();

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

    print_stdout(users.with_title());
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
