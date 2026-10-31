use crate::api::dto::deployment::DeploymentOutput;
use crate::cli::output::{output_arg, output_format};
use crate::cli::problem_json::http_error_list;
use crate::cli::style;
use crate::config::auth::load_auth_config;
use crate::config::config::Config;
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
                .help("Filter by status (pending, progressing, running, backing_off, completed, failed, deleted)"),
        )
        .arg(
            Arg::new("reason")
                .action(ArgAction::Append)
                .long("reason")
                .help("Filter by reason, e.g. crash_loop or image_pull"),
        )
        .arg(
            Arg::new("type")
                .long("type")
                .help("Filter by type (worker or job)")
                .value_parser(["worker", "job"]),
        )
        .arg(
            Arg::new("label")
                .action(ArgAction::Append)
                .short('l')
                .long("label")
                .help("Filter by label, e.g. -l env=prod (repeatable; all must match)"),
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
    #[table(title = "Reason")]
    reason: String,
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

    for filter in ["status", "reason"] {
        if let Some(values) = args.get_many::<String>(filter) {
            for value in values {
                params.push(format!("{filter}[]={value}"));
            }
        }
    }

    if args.contains_id("type") {
        let type_filter = args.get_one::<String>("type").unwrap();
        params.push(format!("kind={}", type_filter));
    }

    if args.contains_id("label") {
        let labels = args.get_many::<String>("label").unwrap();
        for label in labels {
            // A selector like `env=prod` carries an `=`; percent-encode it so
            // the server parses one `label[]` value, not `label[]=env` + `prod`.
            let encoded: String = url::form_urlencoded::byte_serialize(label.as_bytes()).collect();
            params.push(format!("label[]={}", encoded));
        }
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

            let body = match args.get_many::<String>("reason") {
                Some(reasons) => {
                    let reasons: Vec<&str> = reasons.map(String::as_str).collect();
                    match keep_reasons(&body, &reasons) {
                        Ok(kept) => kept,
                        Err(e) => {
                            eprintln!("Failed to parse deployment list: {}", e);
                            exit_code::ExitCode::General.exit();
                        }
                    }
                }
                None => body,
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
                    // running/target, where target follows the autoscaler when
                    // the deployment opted in. Showing the declared `replicas`
                    // there would read as "2/2 instances" on a deployment Ring
                    // has scaled to 8.
                    replicas: {
                        let target = match (&deployment.autoscale, deployment.desired_replicas) {
                            (Some(_), Some(desired)) => desired,
                            _ => deployment.replicas,
                        };
                        let marker = if deployment.autoscale.is_some() {
                            "*"
                        } else {
                            ""
                        };
                        format!("{}/{}{}", deployment.instances.len(), target, marker)
                    },
                    status: style::status(&deployment.status),
                    reason: deployment.failure.map(|f| f.reason).unwrap_or_default(),
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

/// Keep the deployments failing for one of `reasons`. The server filters on
/// this too, but one older than the filter ignores it and answers with every
/// deployment.
fn keep_reasons(body: &str, reasons: &[&str]) -> serde_json::Result<String> {
    let mut deployments: Vec<serde_json::Value> = serde_json::from_str(body)?;
    deployments.retain(|deployment| {
        deployment["failure"]["reason"]
            .as_str()
            .is_some_and(|reason| reasons.contains(&reason))
    });
    serde_json::to_string(&deployments)
}

#[cfg(test)]
mod tests {
    use super::keep_reasons;

    #[test]
    fn keep_reasons_keeps_only_the_matching_failures() {
        let body = r#"[
            {"name": "a", "failure": {"reason": "crash_loop", "message": ""}},
            {"name": "b", "failure": {"reason": "image_pull", "message": ""}},
            {"name": "c", "failure": null}
        ]"#;

        let kept: serde_json::Value =
            serde_json::from_str(&keep_reasons(body, &["crash_loop"]).unwrap()).unwrap();

        assert_eq!(kept.as_array().unwrap().len(), 1);
        assert_eq!(kept[0]["name"], "a");
    }

    #[test]
    fn keep_reasons_matches_nothing_on_a_server_without_failures() {
        let body = include_str!("../../fixtures/deployments_v0_12_0.json");

        assert_eq!(keep_reasons(body, &["crash_loop"]).unwrap(), "[]");
    }
}
