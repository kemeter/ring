use std::fs;
use clap::{ Arg, Command};
use clap::ArgMatches;
use cli_table::{format::Justify, print_stdout, Table, WithTitle};
use crate::config::config::{Config, Contexts, get_config_dir, load_auth_config};
use toml::de::Error as TomlError;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("context")
        .name("context")
        .about("Print contexts configuration")
        .arg(
            Arg::new("parameter")
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
    let binding = "configs".to_string();
    let parameter = args.get_one::<String>("parameter").unwrap_or(&binding);

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
