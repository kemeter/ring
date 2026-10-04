//! Helpers for the tests that need a live Docker daemon. They self-skip when
//! no daemon is reachable, so the suite stays green without Docker.

use bollard::Docker;
use futures::StreamExt;

/// The image these tests run in. Any image with a shell would do; alpine
/// is the smallest one to pull on a cold CI runner.
pub(crate) const TEST_IMAGE: &str = "alpine:latest";

pub(crate) async fn daemon() -> Option<Docker> {
    let docker = Docker::connect_with_local_defaults().ok()?;
    docker.ping().await.ok()?;
    Some(docker)
}

/// Pull [`TEST_IMAGE`] unless it is already local.
///
/// Not an optimisation: a CI runner has a Docker daemon but an empty image
/// store, so assuming the image is present turns "no image" into a test
/// failure that reads like a broken ownership check. Returns whether the
/// image is usable, so a runner without network skips rather than fails on
/// something these tests are not about.
pub(crate) async fn ensure_image(docker: &Docker) -> bool {
    use bollard::query_parameters::CreateImageOptionsBuilder;

    if docker.inspect_image(TEST_IMAGE).await.is_ok() {
        return true;
    }

    let options = CreateImageOptionsBuilder::new()
        .from_image(TEST_IMAGE)
        .build();

    // The pull only completes once its progress stream is drained.
    let mut pull = docker.create_image(Some(options), None, None);
    while let Some(item) = pull.next().await {
        if item.is_err() {
            return false;
        }
    }

    docker.inspect_image(TEST_IMAGE).await.is_ok()
}

/// Start a plain container, optionally labelled as Ring-managed, and
/// return its id. The caller removes it.
pub(crate) async fn start_container(docker: &Docker, name: &str, ring_managed: bool) -> String {
    use bollard::query_parameters::{
        CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StartContainerOptionsBuilder,
    };
    use std::collections::HashMap;

    // A previous crashed run may have left the name taken.
    let _ = docker
        .remove_container(
            name,
            Some(RemoveContainerOptionsBuilder::new().force(true).build()),
        )
        .await;

    let mut labels = HashMap::new();
    if ring_managed {
        labels.insert(
            super::RING_DEPLOYMENT_LABEL.to_string(),
            "exec-boundary-test".to_string(),
        );
    }

    let config = bollard::models::ContainerCreateBody {
        image: Some(TEST_IMAGE.to_string()),
        cmd: Some(vec!["sleep".to_string(), "60".to_string()]),
        labels: Some(labels),
        ..Default::default()
    };

    let created = docker
        .create_container(
            Some(CreateContainerOptionsBuilder::new().name(name).build()),
            config,
        )
        .await
        .expect("create container");

    docker
        .start_container(
            &created.id,
            Some(StartContainerOptionsBuilder::new().build()),
        )
        .await
        .expect("start container");

    created.id
}

pub(crate) async fn remove(docker: &Docker, id: &str) {
    use bollard::query_parameters::RemoveContainerOptionsBuilder;
    let _ = docker
        .remove_container(
            id,
            Some(RemoveContainerOptionsBuilder::new().force(true).build()),
        )
        .await;
}
