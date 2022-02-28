use rusqlite::Connection;
use rusqlite::named_params;
use serde::{Deserialize, Serialize};
use serde_rusqlite::from_rows;
use serde_rusqlite::from_rows_ref;
use std::sync::MutexGuard;
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Pod {
    pub(crate) id: String,
    pub(crate) created_at: i64,
    pub(crate) status: String,
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) image: String,
    pub(crate) runtime: String,
    pub(crate) replicas: i64,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) instances: Vec<String>,
    pub(crate) labels: String
}

impl Pod {
    pub fn deserialize_labels(serialized: &str) -> HashMap<String, String> {
        let deserialized: HashMap<String, String> = serde_json::from_str(&serialized).unwrap();
        deserialized
    }
}

pub(crate) fn find_all(connection: MutexGuard<Connection>) -> Vec<Pod> {
    println!("find_all");
    let mut statement = connection.prepare("
            SELECT
                id,
                created_at,
                status,
                namespace,
                name,
                image,
                runtime,
                replicas,
                labels
            FROM pod"
    ).expect("Could not fetch pods");

    let mut pods: Vec<Pod> = Vec::new();
    let mut rows_iter = from_rows::<Pod>(statement.query([]).unwrap());

    loop {
        match rows_iter.next() {
            None => { break; },
            Some(pod) => {
                let pod = pod.expect("Could not deserialize Pod item");
                pods.push(pod);
            }
        }
    }

    return pods;
}

pub(crate) fn find_one_by_filters(connection: &Connection, filters: Vec<String>) -> Result<Option<Pod>, serde_rusqlite::Error> {

    println!("find_one_by_filters {:?}", filters);

    let mut statement = connection.prepare("SELECT * FROM pod WHERE namespace = :namespace AND name = :name AND status = :status").unwrap();
    let mut rows = statement.query(named_params!{
        ":namespace": filters[0],
        ":name": filters[1],
        ":status": "running"
    }).unwrap();


    let mut ref_rows = from_rows_ref::<Pod>(&mut rows);
    let result = ref_rows.next();

    result.transpose()
}

pub(crate) fn create(connection: &MutexGuard<Connection>, pod: &Pod) -> Pod {
    println!("create");
    println!("{:?}", pod);
    let mut statement = connection.prepare("
            INSERT INTO pod (
                id,
                created_at,
                status,
                namespace,
                name,
                image,
                runtime,
                replicas,
                labels
            ) VALUES (
                :id,
                :created_at,
                :status,
                :namespace,
                :name,
                :image,
                :runtime,
                :replicas,
                :labels
            )"
    ).expect("Could not create pod");

    statement.execute(named_params!{
        ":id": pod.id,
        ":created_at": pod.created_at,
        ":status": "running",
        ":namespace": pod.namespace,
        ":name": pod.name,
        ":image": pod.image,
        ":runtime": pod.runtime,
        ":labels": pod.labels,
        ":replicas": pod.replicas,
    }).expect("Could not create pod");

    return pod.clone();
}

pub(crate) fn update(connection: &MutexGuard<Connection>, pod: &Pod) {
    println!("update pod");

    let mut statement = connection.prepare("
            UPDATE pod
            SET
                status = :status
            WHERE
                id = :id"
    ).expect("Could not update pod");

    statement.execute(named_params!{
        ":id": pod.id,
        ":status": pod.status
    }).expect("Could not update pod");
}
