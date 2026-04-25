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
pub async fn start_event_listener(tx: mpsc::Sender<DockerEvent>, docker: Docker) {
    info!("Starting Docker event listener");

    loop {
        match listen_events(tx.clone(), docker.clone()).await {
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

async fn listen_events(tx: mpsc::Sender<DockerEvent>, docker: Docker) -> Result<(), Box<dyn std::error::Error>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{EventActor, EventMessage};

    fn make_event(action: &str, attrs: &[(&str, &str)], id: Option<&str>) -> EventMessage {
        let attributes = attrs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>();
        EventMessage {
            action: Some(action.to_string()),
            actor: Some(EventActor {
                id: id.map(String::from),
                attributes: Some(attributes),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn parses_die_with_exit_code() {
        let event = make_event(
            "die",
            &[("ring_deployment", "dep-1"), ("exitCode", "137")],
            Some("container-abc"),
        );
        match parse_docker_event(&event) {
            Some(DockerEvent::ContainerDied { deployment_id, container_id, exit_code }) => {
                assert_eq!(deployment_id, "dep-1");
                assert_eq!(container_id, "container-abc");
                assert_eq!(exit_code, Some(137));
            }
            other => panic!("expected ContainerDied, got {:?}", other),
        }
    }

    #[test]
    fn parses_die_without_exit_code() {
        let event = make_event(
            "die",
            &[("ring_deployment", "dep-1")],
            Some("container-abc"),
        );
        match parse_docker_event(&event) {
            Some(DockerEvent::ContainerDied { exit_code, .. }) => {
                assert_eq!(exit_code, None);
            }
            other => panic!("expected ContainerDied with no exit code, got {:?}", other),
        }
    }

    #[test]
    fn parses_oom() {
        let event = make_event(
            "oom",
            &[("ring_deployment", "dep-1")],
            Some("container-abc"),
        );
        assert!(matches!(
            parse_docker_event(&event),
            Some(DockerEvent::ContainerOom { .. })
        ));
    }

    #[test]
    fn parses_kill_with_signal() {
        let event = make_event(
            "kill",
            &[("ring_deployment", "dep-1"), ("signal", "15")],
            Some("container-abc"),
        );
        match parse_docker_event(&event) {
            Some(DockerEvent::ContainerKilled { signal, .. }) => {
                assert_eq!(signal, Some("15".to_string()));
            }
            other => panic!("expected ContainerKilled, got {:?}", other),
        }
    }

    #[test]
    fn parses_start() {
        let event = make_event(
            "start",
            &[("ring_deployment", "dep-1")],
            Some("container-abc"),
        );
        assert!(matches!(
            parse_docker_event(&event),
            Some(DockerEvent::ContainerStarted { .. })
        ));
    }

    #[test]
    fn ignores_event_without_ring_deployment_label() {
        // Containers managed by something other than Ring must be ignored, even
        // if they emit the same actions on the same Docker daemon.
        let event = make_event("die", &[("exitCode", "1")], Some("container-abc"));
        assert!(parse_docker_event(&event).is_none());
    }

    #[test]
    fn ignores_unknown_action() {
        let event = make_event(
            "rename",
            &[("ring_deployment", "dep-1")],
            Some("container-abc"),
        );
        assert!(parse_docker_event(&event).is_none());
    }

    #[test]
    fn ignores_event_without_actor() {
        let event = EventMessage {
            action: Some("die".to_string()),
            actor: None,
            ..Default::default()
        };
        assert!(parse_docker_event(&event).is_none());
    }
}
