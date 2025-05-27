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

    let response = ureq::get(&query)
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .set("Content-Type", "application/json")
        .call();

    let response_content = match response {
        Ok(res) => res.into_string().unwrap_or_else(|_| "Invalid response body".to_string()),
        Err(err) => {
            eprintln!("Failed to fetch node info: {}", err);
            return;
        }
    };

    let parsed: serde_json::Result<NodeRootDto> = serde_json::from_str(&response_content);

    match parsed {
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
            eprintln!("Raw response: {}", response_content);
        }
    }
}
