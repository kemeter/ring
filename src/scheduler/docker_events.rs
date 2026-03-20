use bollard::Docker;
use bollard::query_parameters::EventsOptionsBuilder;
use futures::StreamExt;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Events sent from Docker Event Listener to Scheduler
#[derive(Debug, Clone)]
pub enum DockerEvent {
    /// Container died (crashed, stopped, killed)
    ContainerDied {
        deployment_id: String,
        container_id: String,
        exit_code: Option<i64>,
    },
    /// Container started successfully
    ContainerStarted {
        deployment_id: String,
        container_id: String,
    },
    /// Container was killed (SIGKILL, SIGTERM)
    ContainerKilled {
        deployment_id: String,
        container_id: String,
        signal: Option<String>,
    },
    /// Container ran out of memory
    ContainerOom {
        deployment_id: String,
        container_id: String,
    },
}

/// Start listening to Docker events and send them to the scheduler via channel
pub async fn start_event_listener(tx: mpsc::Sender<DockerEvent>) {
    info!("Starting Docker event listener");

    loop {
        match listen_events(tx.clone()).await {
            Ok(_) => {
                warn!("Docker event stream ended, reconnecting in 5s...");
            }
            Err(e) => {
                error!("Docker event listener error: {}, reconnecting in 5s...", e);
            }
        }

        // Wait before reconnecting
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}

async fn listen_events(tx: mpsc::Sender<DockerEvent>) -> Result<(), Box<dyn std::error::Error>> {
    let docker = Docker::connect_with_local_defaults()?;

    // Filter for container events with ring_deployment label
    let filters = HashMap::from([
        ("type".to_string(), vec!["container".to_string()]),
        ("event".to_string(), vec![
            "die".to_string(),
            "start".to_string(),
            "kill".to_string(),
            "oom".to_string(),
        ]),
    ]);

    let options = EventsOptionsBuilder::new()
        .filters(&filters)
        .build();

    let mut events = docker.events(Some(options));

    info!("Connected to Docker event stream");

    while let Some(event_result) = events.next().await {
        match event_result {
            Ok(event) => {
                if let Some(docker_event) = parse_docker_event(&event) {
                    debug!("Docker event: {:?}", docker_event);
                    if let Err(e) = tx.send(docker_event).await {
                        error!("Failed to send event to scheduler: {}", e);
                        // Channel closed, exit the loop
                        return Err("Channel closed".into());
                    }
                }
            }
            Err(e) => {
                error!("Error receiving Docker event: {}", e);
                return Err(e.into());
            }
        }
    }

    Ok(())
}

fn parse_docker_event(event: &bollard::models::EventMessage) -> Option<DockerEvent> {
    let action = event.action.as_ref()?;
    let actor = event.actor.as_ref()?;
    let attributes = actor.attributes.as_ref()?;

    // Only process containers with ring_deployment label
    let deployment_id = attributes.get("ring_deployment")?.clone();
    let container_id = actor.id.as_ref()?.clone();

    match action.as_str() {
        "die" => {
            let exit_code = attributes
                .get("exitCode")
                .and_then(|s| s.parse::<i64>().ok());

            Some(DockerEvent::ContainerDied {
                deployment_id,
                container_id,
                exit_code,
            })
        }
        "start" => Some(DockerEvent::ContainerStarted {
            deployment_id,
            container_id,
        }),
        "kill" => {
            let signal = attributes.get("signal").cloned();

            Some(DockerEvent::ContainerKilled {
                deployment_id,
                container_id,
                signal,
            })
        }
        "oom" => Some(DockerEvent::ContainerOom {
            deployment_id,
            container_id,
        }),
        _ => None,
    }
}
