use rusqlite::Connection;
use rusqlite::named_params;
use serde::{Deserialize, Serialize};
use serde_rusqlite::from_rows;
use serde_rusqlite::from_rows_ref;
use tokio::sync::MutexGuard;
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Deployment {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) status: String,
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) image: String,
    pub(crate) runtime: String,
    pub(crate) kind: String,
    pub(crate) replicas: u32,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) instances: Vec<String>,
    pub(crate) labels: String,
    pub(crate) secrets: String,
    pub(crate) volumes: String
}

impl Deployment {
    pub fn deserialize_labels(serialized: &str) -> HashMap<String, String> {
        let deserialized: Vec<HashMap<String, String>> = serde_json::from_str(&serialized).unwrap();
        let mut labels = HashMap::new();

        for data in deserialized.into_iter() {
            for (key, value) in data {
                labels.insert(key.clone(), value.clone());
            }
        }

        labels
    }
}

pub(crate) fn find_all(connection: &MutexGuard<Connection>) -> Vec<Deployment> {
    let mut statement = connection.prepare("
            SELECT
                id,
                created_at,
                status,
                namespace,
                name,
                image,
                runtime,
                kind,
                replicas,
                labels,
                secrets,
                volumes
            FROM deployment"
    ).expect("Could not fetch deployments");

    let mut deployments: Vec<Deployment> = Vec::new();
    let mut rows_iter = from_rows::<Deployment>(statement.query([]).unwrap());

    loop {
        match rows_iter.next() {
            None => { break; },
            Some(deployment) => {
                let deployment = deployment.expect("Could not deserialize Deployment item");
                deployments.push(deployment);
            }
        }
    }

    return deployments;
}

pub(crate) fn find_one_by_filters(connection: &Connection, filters: Vec<String>) -> Result<Option<Deployment>, serde_rusqlite::Error> {

    debug!("find_one_by_filters {:?}", filters);

    let mut statement = connection.prepare("SELECT * FROM deployment WHERE namespace = :namespace AND name = :name AND status = :status").unwrap();
    let mut rows = statement.query(named_params!{
        ":namespace": filters[0],
        ":name": filters[1],
        ":status": "running"
    }).unwrap();


    let mut ref_rows = from_rows_ref::<Deployment>(&mut rows);
    let result = ref_rows.next();

    result.transpose()
}

pub(crate) fn find(connection: &MutexGuard<Connection>, id: String) -> Result<Option<Deployment>, serde_rusqlite::Error> {
    let mut statement = connection.prepare("
            SELECT
                id,
                created_at,
                status,
                namespace,
                name,
                image,
                runtime,
                kind,
                replicas,
                labels,
                secrets,
                volumes
            FROM deployment
            WHERE id = :id
            "
    ).expect("Could not fetch deployment");

    let mut rows = statement.query(named_params!{
        ":id": id,
    }).unwrap();

    let mut ref_rows = from_rows_ref::<Deployment>(&mut rows);
    let result = ref_rows.next();

    result.transpose()
}

pub(crate) fn create(connection: &MutexGuard<Connection>, deployment: &Deployment) -> Deployment {

    let labels = serde_json::to_string(&deployment.labels).unwrap();
    let secrets = serde_json::to_string(&deployment.secrets).unwrap();

    let mut statement = connection.prepare("
            INSERT INTO deployment (
                id,
                created_at,
                status,
                namespace,
                name,
                image,
                runtime,
                kind,
                replicas,
                labels,
                secrets,
                volumes
            ) VALUES (
                :id,
                :created_at,
                :status,
                :namespace,
                :name,
                :image,
                :runtime,
                :kind,
                :replicas,
                :labels,
                :secrets,
                :volumes
            )"
    ).expect("Could not create deployment");

    statement.execute(named_params!{
        ":id": deployment.id,
        ":created_at": deployment.created_at,
        ":status": "running",
        ":namespace": deployment.namespace,
        ":name": deployment.name,
        ":image": deployment.image,
        ":runtime": deployment.runtime,
        ":kind": deployment.kind,
        ":labels": labels,
        ":replicas": deployment.replicas,
        ":secrets": secrets,
        ":volumes": deployment.volumes,
    }).expect("Could not create deployment");

    return deployment.clone();
}

pub(crate) fn update(connection: &MutexGuard<Connection>, deployment: &Deployment) {
    let mut statement = connection.prepare("
            UPDATE deployment
            SET
                status = :status
            WHERE
                id = :id"
    ).expect("Could not update deployment");

    statement.execute(named_params!{
        ":id": deployment.id,
        ":status": deployment.status
    }).expect("Could not update deployment");
}


pub(crate) fn delete(connection: &MutexGuard<Connection>, id: String) {
    let mut statement = connection.prepare("
            DELETE FROM deployment
            WHERE
                id = :id"
    ).expect("Could not update deployment");

    statement.execute(named_params!{
        ":id": id
    }).expect("Could not delete deployment");
}

pub(crate) fn delete_batch(connection: &MutexGuard<Connection>, deleted: Vec<String>) {
    for id in deleted {
        let mut statement = connection.prepare("
            DELETE FROM deployment
            WHERE
                id = :id"
        ).expect("Could not delete deployment");

        statement.execute(named_params!{
            ":id": id
        }).expect("Could not delete deployment");
    }
}




