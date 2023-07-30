use clap::{App, Arg};
use clap::SubCommand;
use clap::ArgMatches;
use crate::config::config::Config;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("config")
        .name("config")
        .arg(
            Arg::with_name("parameter")
        )
}

pub(crate) fn execute(args: &ArgMatches, mut configuration: Config) {
    let parameter = args.value_of("parameter").unwrap();

    if parameter == "current-context" {
        print!("{:?}", configuration);
    }
}
