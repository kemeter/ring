use std::fs;
use clap::{App, Arg};
use clap::SubCommand;
use clap::ArgMatches;
use cli_table::{format::Justify, print_stdout, Table, WithTitle};
use crate::config::config::{Config, Contexts, get_config_dir, load_auth_config};
use toml::de::Error as TomlError;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("config")
        .name("config")
        .arg(
            Arg::with_name("parameter")
                .required(false)
                .help("show specific parameter")
        )
}

#[derive(Table)]
struct ConfigTableItem {
    #[table(title = "Name", justify = "Justify::Right")]
    name: String,
    #[table(title = "Host")]
    host: String
}

pub(crate) fn execute(args: &ArgMatches, configuration: Config) {
    let parameter = args.value_of("parameter").unwrap_or("configs");

    if parameter == "current-context" {
        println!("{:?}", configuration);
    }

    if parameter == "user-token" {
        let auth = load_auth_config(configuration.name);

        println!("{}", auth.token);
    }

    if parameter == "configs" {
        let home_dir = get_config_dir();
        let file = format!("{}/config.toml", home_dir);
        let mut configs = vec![];

        if fs::metadata(file.clone()).is_ok() {
            let contents = fs::read_to_string(file).unwrap();
            let contexts: Result<Contexts, TomlError> = toml::from_str(&contents);

           for (key, value) in contexts.unwrap().contexts {
               configs.push(ConfigTableItem {
                   name: key,
                   host: value.host,
               })
           }
        }

        print_stdout(configs.with_title()).expect("");
    }
}
