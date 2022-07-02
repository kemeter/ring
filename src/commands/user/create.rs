pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("user:list")
        .about("create user")
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

pub(crate) fn execute(args: &ArgMatches, mut configuration: Config) {

    let api_url = format!("{}/users", configuration.get_api_url());
    let request = ureq::post(&api_url)
        .send_json(json!({
            "username": username,
            "password": password
        }));

    match request {
        Ok(response) => {
            if response.status() == 201 {
                println!("user creates")
            }
        }
        Err(err) => {
            println!("Unable to create user");
        }
    }
}