use clap::App;
use clap::Arg;
use clap::SubCommand;
use log::info;
use clap::ArgMatches;
use std::fs::File;
use std::io::prelude::*;
use yaml_rust::YamlLoader;
use std::str;
use std::env;
use ureq::json;
use std::collections::HashMap;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("apply")
        .name("apply")
        .arg(
            Arg::with_name("file")
                .short("f")
                .long("file")
                .value_name("FILE")
                .help("Sets a custom config file")
                .takes_value(true),
        )
        .about("Apply a configuration file")
}

pub(crate) fn apply(args: &ArgMatches) {
    info!("Apply configuration");

    let file = args.value_of("file").unwrap_or("ring.yaml");
    let mut config = File::open(file).expect("Unable to open file");
    let mut contents = String::new();

    config.read_to_string(&mut contents).expect("Unable to read file");

    let docs = YamlLoader::load_from_str(&contents).unwrap();
    let pods = &docs[0]["pod"].as_hash().unwrap();

    for entry in pods.iter() {
        let pod_name = entry.0.as_str().unwrap();

        let configs = &docs[0]["pod"][pod_name].as_hash().unwrap();

        let mut namespace: &str = "";
        let mut runtime: &str = "";
        let mut image: &str = "";
        let mut name: &str = "";
        let mut replicas = 0;
        let mut labels = String::from("{}");
        let mut secrets = String::from("{}");

        for key in configs.iter() {

            let label = key.0.as_str().unwrap();
            let value = &docs[0]["pod"][pod_name][label];

            if "runtime" == label && "docker" != value.as_str().unwrap() {
                println!("Runtime \"{}\" not supported", value.as_str().unwrap());
                continue;
            }

            if "namespace" == label {
                namespace = value.as_str().unwrap();
            }

            if "name" == label {
                name = value.as_str().unwrap();
            }

            if "runtime" == label {
                runtime = value.as_str().unwrap();
            }

            if "image" == label {
                image = value.as_str().unwrap();
            }

            if "replicas" == label {
                replicas = value.as_i64().unwrap();
            }

            if "labels" == label {
                let labels_vec = value.as_vec().unwrap();

                if labels_vec.len() > 0 {
                    let mut map = HashMap::new();

                    for l in labels_vec {
                        for v in l.as_hash().unwrap().iter() {
                            map.insert(v.0.as_str().unwrap(), v.1.as_str().unwrap());
                        }
                    }

                    labels = serde_json::to_string(&map).unwrap();
                }
            }

            if "secrets" == label {
                let secrets_vec = value.as_hash().unwrap();
                let mut map = HashMap::new();

                for v in secrets_vec.iter() {
                    let mut secret_value = String::from(v.1.as_str().unwrap());
                    secret_value.remove(0);

                    let value_format = env::var(&secret_value).unwrap_or(v.1.as_str().unwrap().to_string());
                    map.insert(v.0.as_str().unwrap(), value_format);
                }

                secrets = serde_json::to_string(&map).unwrap();
            }
        }

        let api_url = "http://127.0.0.1:3030/pods";

        info!("push configuration: {}", api_url);
        let _resp = ureq::post(api_url)
            .send_json(json!({
                "image": image,
                "name": name,
                "runtime": runtime,
                "namespace": namespace,
                "replicas": replicas,
                "labels": labels,
                "secrets": secrets
            }));
    }

    println!("deployment created");
}