use crate::api::dto::deployment::DeploymentOutput;
use crate::commands::output::{output_arg, output_format};
use crate::commands::problem_json::http_error_list;
use crate::commands::style;
use crate::config::config::Config;
use crate::config::config::load_auth_config;
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::{ArgAction, Command};
use cli_table::{Table, WithTitle, format::Justify};

pub(crate) fn command_config() -> Command {
    Command::new("list")
        .about("List deployments")
        .arg(
            Arg::new("namespace")
                .short('n')
                .long("namespace")
                .help("restrict only namespace"),
        )
        .arg(
            Arg::new("status")
                .action(ArgAction::Append)
                .short('s')
                .long("status")
                .help("Filter by status"),
        )
        .arg(
            Arg::new("type")
                .long("type")
                .help("Filter by type (worker or job)")
                .value_parser(["worker", "job"]),
        )
        .arg(output_arg())
}

#[derive(Table)]
struct DeploymentTableItem {
    #[table(title = "ID", justify = "Justify::Right")]
    id: String,
    #[table(title = "Created at (UTC)")]
    created_at: String,
    #[table(title = "Updated at (UTC)")]
    updated_at: String,
    #[table(title = "Namespace")]
    namespace: String,
    #[table(title = "Name")]
    name: String,
    #[table(title = "Image")]
    image: String,
    #[table(title = "Runtime")]
    runtime: String,
    #[table(title = "Kind")]
    kind: String,
    #[table(title = "Replicas")]
    replicas: String,
    #[table(title = "Status")]
    status: String,
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let mut query = format!("{}/deployments", api_url);
    let mut params: Vec<String> = Vec::new();

    let mut ns_scope: Vec<String> = Vec::new();
    if args.contains_id("namespace") {
        let namespace = args.get_many::<String>("namespace").unwrap();

        for namespace in namespace {
            params.push(format!("namespace[]={}", namespace));
            ns_scope.push(namespace.clone());
        }
    }
    let ns_label = if ns_scope.is_empty() {
        "all".to_string()
    } else {
        ns_scope.join(",")
    };

    if args.contains_id("status") {
        let statuses = args.get_many::<String>("status").unwrap();
        for status in statuses {
            params.push(format!("status[]={}", status));
        }
    }

    if args.contains_id("type") {
        let type_filter = args.get_one::<String>("type").unwrap();
        params.push(format!("kind={}", type_filter));
    }

    if !params.is_empty() {
        query.push('?');
        query.push_str(&params.join("&"));
    }

    let request = client
        .get(&*query)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status != 200 {
                style::print_error(&http_error_list(status.as_u16(), "deployments", &ns_label));
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let body = match response.text().await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("Failed to read deployment list response: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            if output_format(args).is_json() {
                println!("{}", body);
                return;
            }

            let deployments_list: Vec<DeploymentOutput> = match serde_json::from_str(&body) {
                Ok(list) => list,
                Err(e) => {
                    eprintln!("Failed to parse deployment list: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            let mut deployments = vec![];
            for deployment in deployments_list {
                deployments.push(DeploymentTableItem {
                    id: deployment.id,
                    created_at: style::format_date(&deployment.created_at),
                    updated_at: style::format_date(&deployment.updated_at),
                    namespace: deployment.namespace,
                    name: deployment.name,
                    image: deployment.image,
                    runtime: deployment.runtime,
                    kind: deployment.kind,
                    replicas: format!("{}/{}", deployment.instances.len(), deployment.replicas),
                    status: style::status(&deployment.status),
                })
            }

            style::print_table(deployments.with_title());
        }
        Err(error) => {
            eprintln!("Error fetching deployments: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
