use clap::{App, Arg};
use clap::SubCommand;
use clap::ArgMatches;
use crate::config::config::{Config, load_auth_config};

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("config")
        .name("config")
        .arg(
            Arg::with_name("parameter")
                .required(false)
                .help("show specific parameter")
        )
}

pub(crate) fn execute(args: &ArgMatches, configuration: Config) {
    let parameter = args.value_of("parameter").unwrap_or("");

    if parameter == "current-context" {
        println!("{:?}", configuration);
    }

    if parameter == "user-token" {
        let auth = load_auth_config(configuration.name);

        println!("{}", auth.token);
    }
}
