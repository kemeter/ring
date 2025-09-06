use crate::api::dto::node::NodeRootDto;
use crate::config::config::{load_auth_config, Config};
use clap::{ArgMatches, Command};

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("get")
        .about("Get node information")
}

pub(crate) async fn execute(_args: &ArgMatches, mut configuration: Config) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let query = format!("{}/node/get", api_url);

    let response = reqwest::Client::new()
        .get(&query)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .header("Content-Type", "application/json")
        .send()
        .await;

    match response {
        Ok(res) => {
            match res.json::<NodeRootDto>().await {
                Ok(data) => {
            println!("\n🖧 Node Info");
            println!("──────────────");
            println!("Hostname         : {}", data.hostname);
            println!("OS               : {}", data.os);
            println!("Architecture     : {}", data.arch);
            println!("Uptime           : {}", data.uptime);
            println!("CPU Cores        : {}", data.cpu_count);
            println!("Memory Total     : {:.2} GiB", data.memory_total);
            println!("Memory Available : {:.2} GiB", data.memory_available);
            println!("Load Average     : {:.2}, {:.2}, {:.2}",
                     data.load_average.get(0).unwrap_or(&0.0),
                     data.load_average.get(1).unwrap_or(&0.0),
                     data.load_average.get(2).unwrap_or(&0.0));
                }
                Err(e) => {
                    eprintln!("Failed to parse JSON: {}", e);
                }
            }
        }
        Err(err) => {
            eprintln!("Failed to fetch node info: {}", err);
        }
    }
}
