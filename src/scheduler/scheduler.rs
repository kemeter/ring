extern crate job_scheduler;

use job_scheduler::{Job, JobScheduler};
use std::time::Duration;
use crate::runtime::docker;
use crate::models::pods;
use rusqlite::Connection;
use std::sync::{Mutex, Arc};

pub(crate) fn schedule(storage: Arc<Mutex<Connection>>) {
    let mut scheduler = JobScheduler::new();

    scheduler.add(Job::new("1/10 * * * * *".parse().unwrap(), move || {
        println!("Get executed every 10 seconds!");

        let guard = storage.lock().unwrap();

        let list_pods = pods::find_all(guard);
        for pod in list_pods.into_iter() {

            if "docker" == pod.runtime {
                docker::apply(pod.clone());
            }

            println!("{:?}", pod);
        }

    }));

    // Adding a task to scheduler to execute it in every 2 minutes.
    loop {
        scheduler.tick();
        std::thread::sleep(Duration::from_millis(100));
    }
}
