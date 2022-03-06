use warp::http::StatusCode;
use serde::{Serialize, Deserialize};
use rusqlite::Connection;
use crate::models::deployments;
use crate::models::users as users_model;
use users_model::User;
use uuid::Uuid;
use log::info;
use std::sync::{Mutex, Arc};
use chrono::{NaiveDateTime, DateTime, Utc};
use std::collections::HashMap;
use std::convert::Infallible;
use crate::database;

use warp::{
    reject, Filter, Rejection,
};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct NotFound;

#[tokio::main]
pub(crate) async fn start(storage: Arc<Mutex<Connection>>, server_address: &str)
{
    info!("Starting server on {}", server_address);

    let conn = Arc::clone(&storage);
    let conn2 = Arc::clone(&storage);
    let auth_arc = Arc::clone(&storage);

    let login = warp::post()
        .and(warp::path("login"))
        .and(warp::body::json())
        .map(move |login_input: LoginInput | {

            debug!("Login with {:?}", login_input.username);

            let guard = auth_arc.lock().unwrap();

            let option = users_model::find_by_username(&guard, &login_input.username);
            let user = option.as_ref().unwrap();
            println!("{:?}", !user.is_some());

            if !!!user.is_some() {
                let not_found = NotFound;
                return warp::reply::json(&not_found);
            }

            let member = user.clone().unwrap();

            //@todo generate token
            member.token = Uuid::new_v4().to_string();
            users_model::login(&guard, &member);

            let login_output = LoginOutput {
                token: member.token.to_string(),
            };

            warp::reply::json(&login_output)
        });

    let list = warp::get()
        .and(warp::path("deployments"))
        .and(auth_validation())
        .map(move |_input| {
            println!("List deployments");
            let mut deployments: Vec<DeploymentOutput> = Vec::new();
            let guard = conn.lock().unwrap();

            let list_deployments = deployments::find_all(guard);
            for deployment in list_deployments.into_iter() {
                let output = hydrate_deployment_output(deployment);

                deployments.push(output);
            }

            warp::reply::json(&deployments)
        });

    let post = warp::post()
        .and(warp::path("deployments"))
        .and(warp::body::json())
        .and(auth_validation())
        .map(move |deployment_input: DeploymentInput, _auth| {

            let mut filters = Vec::new();
            filters.push(deployment_input.namespace.clone());
            filters.push(deployment_input.name.clone());

            let guard = conn2.lock().unwrap();
            let option = deployments::find_one_by_filters(&guard, filters);
            let config = option.as_ref().unwrap();

            // deployment found
            if config.is_some() {
                info!("Found deployment");
                let mut deployment = config.clone().unwrap();

                //@todo: implement reel deployment diff
                if deployment_input.image.clone() != deployment.image {
                    info!("Image changed");
                    println!("Image changed");

                    deployment.status = "delete".to_string();
                    deployments::update(&guard, &deployment);

                    deployment.image = deployment_input.image.clone();
                    deployments::create(&guard, &deployment);

                    debug!("{:?}", deployment);
                }

                let deployment_output = hydrate_deployment_output(deployment);

                return warp::reply::with_status(warp::reply::json(&deployment_output), StatusCode::OK);

            }  else {
                info!("Deployment not found, create a new one");

                let utc: DateTime<Utc> = Utc::now();
                let deployment = deployments::Deployment {
                    id: Uuid::new_v4().to_string(),
                    name: deployment_input.name.clone(),
                    runtime: deployment_input.runtime.clone(),
                    namespace: deployment_input.namespace.clone(),
                    image: deployment_input.image.clone(),
                    status: "running".to_string(),
                    created_at: utc.timestamp(),
                    labels: deployment_input.labels,
                    instances: [].to_vec(),
                    replicas: deployment_input.replicas,
                };

                deployments::create(&guard, &deployment);

                let deployment_output = hydrate_deployment_output(deployment);

                return warp::reply::with_status(warp::reply::json(&deployment_output), StatusCode::CREATED);
            }
        });

    let routes = login.or(list).or(post);

    warp::serve(routes).run(([0,0,0,0], 3030)).await
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct DeploymentInput {
    name: String,
    runtime: String,
    namespace: String,
    image: String,
    replicas: i64,
    labels: String
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct DeploymentOutput {
    id: String,
    created_at: String,
    status: String,
    name: String,
    runtime: String,
    namespace: String,
    image: String,
    replicas: u8,
    ports: Vec<String>,
    labels: HashMap<String, String>
}

fn hydrate_deployment_output(deployment: deployments::Deployment) -> DeploymentOutput {
    let labels: HashMap<String, String> = deployments::Deployment::deserialize_labels(&deployment.labels);

    return DeploymentOutput {
        id: deployment.id,
        created_at: NaiveDateTime::from_timestamp(deployment.created_at, 0).to_string(),
        status: deployment.status,
        name: deployment.name,
        namespace: deployment.namespace,
        runtime: deployment.runtime,
        image: deployment.image,
        replicas: 0,
        ports: [].to_vec(),
        labels: labels
    };
}


#[derive(Deserialize, Serialize, Debug, Clone)]
struct LoginInput {
    username: String
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct LoginOutput {
    token: String
}

#[derive(Debug, Clone)]
struct Unauthorized;

impl reject::Reject for Unauthorized {}

fn verify_token(token: &str) -> bool {

    //@todo: inject database
    let storage = database::get_database_connection();

    let option = users_model::find_by_token(&storage, token);
    let config = option.as_ref().unwrap();

    if config.is_some() {
        return true;
    }

    return false;
}

fn auth_validation() -> impl Filter<Extract = ((),), Error = Rejection> + Copy {
    warp::header::header("Authorization").and_then(|token: String| async move {

        if verify_token(&token.trim_start_matches("Bearer ")) {
            Ok(())
        } else {
            Err(warp::reject::custom(Unauthorized))
        }
    })
}
