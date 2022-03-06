use clap::App;
use clap::Arg;
use clap::SubCommand;
use clap::ArgMatches;
use std::fs;
use ureq::json;
use crate::config::config::Config;
use crate::config::config::get_config_dir;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("login")
        .about("Login to your account")
        .arg(
            Arg::with_name("username")
                .short("u")
                .long("username")
                .help("Your username")
                .takes_value(true)
                .required(true)
        )
        .arg(
            Arg::with_name("password")
                .short("p")
                .long("password")
                .help("Your password")
                .takes_value(true)
                .required(true)
        )
}

pub(crate) fn apply(args: &ArgMatches, configuration: Config) {
    let username = args.value_of("username").unwrap();
    let password = args.value_of("password").unwrap();

    let config_directory = get_config_dir();
    let config_file = format!("{}/auth.json", config_directory);

    let api_url = format!("{}://{}:{}/login", configuration.api.scheme, configuration.ip, configuration.api.port);
    let request = ureq::post(&api_url)
        .send_json(json!({
            "username": username,
            "password": password
        }));

    let response = request.unwrap();
    println!("{:?}", response);

    if response.status() == 200 {

        let content = response.into_string().unwrap();
        fs::create_dir_all(&config_directory).unwrap();
        fs::write(config_file, content).expect("Unable to write file");
        return println!("Logging in as {}", username);
    }

    println!("Unable to login");
}
