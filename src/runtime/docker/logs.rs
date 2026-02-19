use bollard::Docker;
use bollard::query_parameters::{LogsOptionsBuilder, InspectContainerOptions};
use bollard::container::LogOutput;
use futures::StreamExt;
use futures::stream::Stream;
use std::pin::Pin;

fn format_log_output(output: LogOutput) -> String {
    match output {
        LogOutput::StdOut { message }
        | LogOutput::StdErr { message }
        | LogOutput::StdIn { message }
        | LogOutput::Console { message } => {
            String::from_utf8_lossy(&message).to_string()
        }
    }
}

fn process_log_chunk(chunk: LogOutput) -> Option<String> {
    let line = format_log_output(chunk).replace('\n', "");
    if line.trim().is_empty() { None } else { Some(line) }
}

pub(crate) async fn logs(docker: &Docker, container_id: String, tail: Option<&str>, since: Option<i32>) -> Vec<String> {
    match docker.inspect_container(&container_id, None::<InspectContainerOptions>).await {
        Ok(_) => {}
        Err(e) => {
            debug!("Container {} not found or not accessible: {}", container_id, e);
            return Vec::new();
        }
    }

    let mut builder = LogsOptionsBuilder::new()
        .stdout(true)
        .stderr(true);

    if let Some(tail_value) = tail {
        builder = builder.tail(tail_value);
    }

    if let Some(since_value) = since {
        builder = builder.since(since_value);
    }

    let options = builder.build();
    let mut logs_stream = docker.logs(&container_id, Some(options));
    let mut logs = vec![];

    while let Some(log_result) = logs_stream.next().await {
        match log_result {
            Ok(chunk) => {
                if let Some(line) = process_log_chunk(chunk) {
                    logs.push(line);
                }
            }
            Err(e) => {
                debug!("Docker get logs errors for container {}: {}", container_id, e);
                break;
            }
        }
    }

    logs
}

pub(crate) async fn logs_stream(
    docker: Docker,
    container_id: String,
    tail: Option<&str>,
    since: Option<i32>,
) -> Pin<Box<dyn Stream<Item = String> + Send>> {
    match docker.inspect_container(&container_id, None::<InspectContainerOptions>).await {
        Ok(_) => {}
        Err(e) => {
            debug!("Container {} not found or not accessible: {}", container_id, e);
            return Box::pin(futures::stream::empty());
        }
    }

    let mut builder = LogsOptionsBuilder::new()
        .stdout(true)
        .stderr(true)
        .follow(true);

    if let Some(tail_value) = tail {
        builder = builder.tail(tail_value);
    }

    if let Some(since_value) = since {
        builder = builder.since(since_value);
    }

    let options = builder.build();

    let stream = docker.logs(&container_id, Some(options))
        .filter_map(|result| async {
            match result {
                Ok(chunk) => process_log_chunk(chunk),
                Err(e) => {
                    debug!("Docker stream logs error: {}", e);
                    None
                }
            }
        });

    Box::pin(stream)
}
