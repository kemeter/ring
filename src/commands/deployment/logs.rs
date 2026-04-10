use crate::config::config::Config;
use crate::config::config::load_auth_config;
use crate::runtime::runtime::Log;
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::collections::HashSet;

pub(crate) fn command_config() -> Command {
    Command::new("logs")
        .about("Show logs for a deployment")
        .arg(Arg::new("id").required(true).help("Deployment ID"))
        .arg(
            Arg::new("follow")
                .long("follow")
                .short('f')
                .help("Follow log output in real time")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("tail")
                .long("tail")
                .help("Number of lines to show from the end of the logs (default: 100)")
                .value_parser(clap::value_parser!(u64)),
        )
        .arg(
            Arg::new("since")
                .long("since")
                .help("Show logs since a relative duration (e.g. 30s, 10m, 2h) or RFC3339 timestamp"),
        )
        .arg(
            Arg::new("container")
                .long("container")
                .short('c')
                .help("Filter logs by container/instance name"),
        )
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args.get_one::<String>("id").unwrap();
    let follow = args.get_flag("follow");
    let tail = args.get_one::<u64>("tail").copied();
    let since = args.get_one::<String>("since").cloned();
    let container = args.get_one::<String>("container").cloned();

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    if follow {
        follow_logs(
            id,
            tail,
            since,
            container,
            &api_url,
            &auth_config.token,
            client,
        )
        .await;
    } else {
        match fetch_logs(
            id,
            tail,
            since.as_deref(),
            container.as_deref(),
            &api_url,
            &auth_config.token,
            client,
        )
        .await
        {
            Ok(logs) => {
                if logs.is_empty() {
                    println!("No logs available for this deployment");
                } else {
                    for log in logs {
                        print_log(&log);
                    }
                }
            }
            Err(e) => eprintln!("{}", e),
        }
    }
}

fn build_url(
    api_url: &str,
    id: &str,
    tail: Option<u64>,
    since: Option<&str>,
    container: Option<&str>,
) -> String {
    let mut url = format!("{}/deployments/{}/logs", api_url, id);
    let mut params: Vec<String> = Vec::new();
    if let Some(t) = tail {
        params.push(format!("tail={}", t));
    }
    if let Some(s) = since {
        params.push(format!("since={}", encode_query(s)));
    }
    if let Some(c) = container {
        params.push(format!("container={}", encode_query(c)));
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    url
}

async fn fetch_logs(
    id: &str,
    tail: Option<u64>,
    since: Option<&str>,
    container: Option<&str>,
    api_url: &str,
    token: &str,
    client: &reqwest::Client,
) -> Result<Vec<Log>, String> {
    let url = build_url(api_url, id, tail, since, container);

    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| format!("Error fetching deployment logs: {}", e))?;

    if response.status() != 200 {
        return Err(format!(
            "Unable to fetch deployment logs: {}",
            response.status()
        ));
    }

    response
        .json::<Vec<Log>>()
        .await
        .map_err(|e| format!("Error parsing logs response: {}", e))
}

async fn follow_logs(
    id: &str,
    tail: Option<u64>,
    since: Option<String>,
    container: Option<String>,
    api_url: &str,
    token: &str,
    client: &reqwest::Client,
) {
    let initial = fetch_logs(
        id,
        tail,
        since.as_deref(),
        container.as_deref(),
        api_url,
        token,
        client,
    )
    .await;

    let mut seen: HashSet<String> = HashSet::new();

    match initial {
        Ok(logs) => {
            for log in &logs {
                print_log(log);
                seen.insert(log_key(log));
            }
        }
        Err(e) => {
            eprintln!("{}", e);
            return;
        }
    }

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        match fetch_logs(
            id,
            tail,
            since.as_deref(),
            container.as_deref(),
            api_url,
            token,
            client,
        )
        .await
        {
            Ok(logs) => {
                for log in logs {
                    let key = log_key(&log);
                    if seen.insert(key) {
                        print_log(&log);
                    }
                }

                if seen.len() > 10_000 {
                    seen.clear();
                }
            }
            Err(e) => {
                eprintln!("{}", e);
            }
        }
    }
}

fn log_key(log: &Log) -> String {
    format!(
        "{}|{}|{}",
        log.timestamp.clone().unwrap_or_default(),
        log.instance,
        log.message
    )
}

fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

fn print_log(log: &Log) {
    match &log.timestamp {
        Some(ts) => println!("[{}] {} {}", ts, log.instance, log.message),
        None => println!("{} {}", log.instance, log.message),
    }
}
