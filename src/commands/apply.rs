use clap::App;
use clap::SubCommand;
use log::info;
use clap::ArgMatches;
use std::fs::File;
use std::io::prelude::*;
use yaml_rust::YamlLoader;

use ureq::json;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("apply")
        .name("apply")
}

pub(crate) fn apply(_args: &ArgMatches) {
    info!("Apply configuration");

    let file = "ring.yaml";
    let mut file = File::open(file).expect("Unable to open file");
    let mut contents = String::new();
  
    file.read_to_string(&mut contents).expect("Unable to read file");

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
                "labels": "{}"
            }));
    }

    println!("deployment created");
}