use crate::api::dto::config::ConfigOutput;
use crate::config::config::{load_auth_config, Config};
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use cli_table::{print_stdout, Table, WithTitle};
use validator::HasLen;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("list")
        .about("List of config maps")
        .arg(
            Arg::new("namespace")
                .short('n')
                .long("namespace")
                .help("restrict only namespace")
        )
}

#[derive(Table)]
struct ConfigTableItem {
    #[table(title = "Id")]
    id: String,
    #[table(title = "Name")]
    name: String,
    #[table(title = "Created at")]
    created_at: String,
    #[table(title = "Updated at")]
    updated_at: String,
    #[table(title = "Namespace")]
    namespace: String,
    #[table(title = "Data")]
    data: String,
}


pub(crate) fn execute(args: &ArgMatches, mut configuration: Config) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let mut query = format!("{}/configs", api_url);
    let mut params: Vec<String> = Vec::new();

    if args.contains_id("namespace"){
        let namespace = args.get_many::<String>("namespace").unwrap();

        for namespace in namespace {
            params.push(format!("namespace[]={}", namespace));
        }
    }

    let response = ureq::get(&*query)
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .set("Content-Type", "application/json")
        .call();
    let response_content = response.unwrap().into_string().unwrap();

    let value: serde_json::Result<Vec<ConfigOutput>> = serde_json::from_str(&response_content);
    let config_list = value.unwrap();

    let mut configs = vec![];

    for config in  config_list{
        configs.push(ConfigTableItem {
            id: config.id.to_string(),
            created_at: config.created_at.to_string(),
            updated_at: config.updated_at.unwrap_or_default(),
            name: config.name,
            namespace: config.namespace,
            data: config.data.length().to_string(),
        });
    }

    print_stdout(configs.with_title()).expect("");

}