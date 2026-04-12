use crate::api::dto::stats::DeploymentStatsOutput;
use crate::config::config::{Config, load_auth_config};
use clap::{Arg, ArgMatches, Command};

pub(crate) fn command_config() -> Command {
    Command::new("metrics")
        .about("Show real-time resource usage metrics for a deployment")
        .arg(Arg::new("id").help("Deployment ID").required(true))
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.2} GiB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.2} MiB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.2} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args.get_one::<String>("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let response = client
        .get(format!("{}/deployments/{}/metrics", api_url, id))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match response {
        Ok(res) => {
            if res.status() != 200 {
                eprintln!("Unable to fetch metrics: {}", res.status());
                return;
            }

            match res.json::<DeploymentStatsOutput>().await {
                Ok(stats) => {
                    println!("DEPLOYMENT METRICS: {}", stats.deployment_name);
                    println!("===================");
                    println!("Instances     : {}", stats.instance_count);
                    println!("Total CPU     : {:.2}%", stats.total_cpu_usage_percent);
                    println!(
                        "Total Memory  : {} / {} ({:.1}%)",
                        format_bytes(stats.total_memory.usage_bytes),
                        format_bytes(stats.total_memory.limit_bytes),
                        stats.total_memory.usage_percent
                    );
                    println!(
                        "Total Net I/O : {} rx / {} tx",
                        format_bytes(stats.total_network.rx_bytes),
                        format_bytes(stats.total_network.tx_bytes)
                    );
                    println!(
                        "Total Disk I/O: {} read / {} write",
                        format_bytes(stats.total_disk_io.read_bytes),
                        format_bytes(stats.total_disk_io.write_bytes)
                    );
                    println!("Total PIDs    : {}", stats.total_pids);
                    println!();

                    for c in &stats.instances {
                        println!("  Instance: {} ({})", c.instance_name, c.instance_id);
                        println!("    CPU       : {:.2}%", c.cpu_usage_percent);
                        println!(
                            "    Memory    : {} / {} ({:.1}%)",
                            format_bytes(c.memory.usage_bytes),
                            format_bytes(c.memory.limit_bytes),
                            c.memory.usage_percent
                        );
                        println!(
                            "    Net I/O   : {} rx / {} tx",
                            format_bytes(c.network.rx_bytes),
                            format_bytes(c.network.tx_bytes)
                        );
                        println!(
                            "    Disk I/O  : {} read / {} write",
                            format_bytes(c.disk_io.read_bytes),
                            format_bytes(c.disk_io.write_bytes)
                        );
                        println!("    PIDs      : {} / {}", c.pids.current, c.pids.limit);
                        println!("    Restarts  : {}", c.restart_count);
                        println!();
                    }
                }
                Err(e) => eprintln!("Failed to parse metrics: {}", e),
            }
        }
        Err(e) => eprintln!("Failed to fetch metrics: {}", e),
    }
}
