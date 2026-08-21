//! Interactive `exec` sessions over the Docker Engine API.
//!
//! The health-check path next door already runs commands in a container, but
//! fire-and-forget: no TTY, no stdin, output drained only to read an exit
//! code. This module is the interactive counterpart — it keeps the stream
//! open, wires stdin back, and exposes terminal resize.
//!
//! Three Docker behaviours drive the shape of the code:
//!
//! 1. **The output stream is multiplexed only without a TTY.** With
//!    `tty: true` the daemon hands back a raw byte stream tagged
//!    `LogOutput::Console`, because a real terminal has a single output
//!    channel. Callers that need stdout and stderr apart must ask for
//!    `tty: false`.
//! 2. **The exit code appears only after the stream is drained.** Inspecting
//!    early returns `running: true` and no code, which is why `wait` polls
//!    rather than reading once (see [`wait_for_exit`]).
//! 3. **A PTY can only be resized once the exec has started.** The daemon
//!    rejects a resize against a created-but-not-started exec, so the initial
//!    size is applied after `start_exec` rather than before it.

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::{self, Stream};
use std::pin::Pin;
use std::time::Duration;

use crate::hypervisor::lifecycle_trait::{
    ExecError, ExecOutput, ExecRequest, ExecSession, TerminalSize,
};

/// How long to keep polling for an exit code after the output stream ends.
///
/// The exec process is already finished by then — this only covers the gap
/// until the daemon marks it as such. Bounded so a daemon that never updates
/// the record cannot hang the session's `wait` forever.
const EXIT_POLL_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Open an interactive exec session in `instance_id`.
///
/// Fails rather than guessing when the container is not running: Docker's
/// error for an exec on a stopped container is opaque, and callers deserve
/// to know the difference between "no such instance" and "the command was
/// rejected".
pub(crate) async fn exec(
    docker: &Docker,
    instance_id: &str,
    request: ExecRequest,
) -> Result<ExecSession, ExecError> {
    if request.command.is_empty() {
        return Err(ExecError::Failed("empty command".to_string()));
    }

    admit(docker, instance_id).await?;

    // `privileged` and `user` stay unset on purpose: the exec inherits the
    // image's user and the container's existing privileges. Escalating here
    // would hand every exec caller more power than the workload itself has.
    let options = CreateExecOptions {
        cmd: Some(request.command.clone()),
        tty: Some(request.tty),
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        ..Default::default()
    };

    let created = docker
        .create_exec(instance_id, options)
        .await
        .map_err(|e| ExecError::Failed(format!("could not create exec: {e}")))?;
    let exec_id = created.id;

    let started = docker
        .start_exec(
            &exec_id,
            Some(StartExecOptions {
                detach: false,
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| ExecError::Failed(format!("could not start exec: {e}")))?;

    let (output, input) = match started {
        StartExecResults::Attached { output, input } => (output, input),
        // Only reachable if `detach: true` above, which we never set.
        StartExecResults::Detached => {
            return Err(ExecError::Failed(
                "docker detached the exec session".to_string(),
            ));
        }
    };

    // Size the PTY as early as we can, which is *after* start: Docker rejects
    // a resize on an exec that has not started yet, so doing this before the
    // call above silently left every session at the default 80x24 and made
    // full-screen programs redraw over themselves until the first SIGWINCH.
    if request.tty
        && let Some(size) = request.size
    {
        resize(docker, &exec_id, size).await;
    }

    Ok(ExecSession {
        output: map_output(output),
        input,
        resize: build_resizer(docker.clone(), exec_id.clone()),
        wait: wait_for_exit(docker.clone(), exec_id),
    })
}

/// Gate an exec request on the container being *ours* and running.
///
/// The ownership half is the security boundary. Everything else in this
/// runtime reaches containers through `list_instances*`, which only ever
/// yields containers carrying the [`RING_DEPLOYMENT_LABEL`]; exec is the one
/// path that takes an id straight from its caller, so it has to re-establish
/// that invariant itself. Without this check any id that reaches the handler
/// addresses every container on the daemon — a database Ring does not manage,
/// another tenant's workload, or Ring's own container — turning exec into a
/// way out of the deployment it was scoped to.
///
/// An unlabelled container is reported as `InstanceUnavailable`, exactly like
/// one that does not exist: a caller poking at ids has no business learning
/// which of them are real.
///
/// Inspect failures are *not* folded into "unavailable". A daemon that is
/// down or a socket we lack permission on is an operational fault, and
/// reporting it as a missing instance sends operators hunting for the wrong
/// problem.
async fn admit(docker: &Docker, instance_id: &str) -> Result<(), ExecError> {
    let container = docker
        .inspect_container(
            instance_id,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .map_err(|e| {
            let message = e.to_string();
            // Docker answers 404 for an unknown container. That is a genuine
            // "no such instance", not a daemon fault.
            if message.contains("404") || message.contains("No such container") {
                ExecError::InstanceUnavailable(format!("{instance_id} does not exist"))
            } else {
                ExecError::Failed(format!("could not inspect {instance_id}: {message}"))
            }
        })?;

    let owned = container
        .config
        .as_ref()
        .and_then(|c| c.labels.as_ref())
        .is_some_and(|labels| labels.contains_key(super::RING_DEPLOYMENT_LABEL));

    if !owned {
        return Err(ExecError::InstanceUnavailable(format!(
            "{instance_id} is not a Ring-managed instance"
        )));
    }

    let running = container
        .state
        .as_ref()
        .and_then(|s| s.running)
        .unwrap_or(false);

    if !running {
        return Err(ExecError::InstanceUnavailable(format!(
            "{instance_id} is not running"
        )));
    }

    Ok(())
}

/// Adapt Docker's stream to the runtime-neutral [`ExecOutput`].
///
/// `Console` is what a TTY session produces and maps to stdout: a terminal
/// has one output channel, and splitting it would invent a distinction the
/// PTY never made. `StdIn` frames are echoes of our own input and are
/// dropped — forwarding them would double every keystroke on screen.
///
/// A transport error is forwarded and *then* ends the stream. Dropping it
/// instead would make a daemon that died mid-command look exactly like a
/// command that finished with nothing left to say, and the caller would
/// report a truncated session as a successful one.
fn map_output(
    output: Pin<Box<dyn Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>>,
) -> Pin<Box<dyn Stream<Item = Result<ExecOutput, ExecError>> + Send>> {
    // `take_while` on a flag that the error arm sets: `filter_map` alone
    // would skip the error and keep polling a stream that has already failed.
    let mut failed = false;
    Box::pin(
        output
            .map(|chunk| match chunk {
                Ok(LogOutput::StdOut { message }) | Ok(LogOutput::Console { message }) => {
                    Some(Ok(ExecOutput::Stdout(message.to_vec())))
                }
                Ok(LogOutput::StdErr { message }) => Some(Ok(ExecOutput::Stderr(message.to_vec()))),
                Ok(LogOutput::StdIn { .. }) => None,
                Err(e) => Some(Err(ExecError::Failed(format!("exec stream failed: {e}")))),
            })
            .take_while(move |item| {
                let keep = !failed;
                if matches!(item, Some(Err(_))) {
                    failed = true;
                }
                async move { keep }
            })
            .filter_map(|item| async move { item }),
    )
}

/// Build the resize hook handed to the caller.
///
/// Resize failures are swallowed: a session whose window cannot be adjusted
/// is degraded, not broken, and tearing it down over a cosmetic problem
/// would be worse than the problem.
fn build_resizer(
    docker: Docker,
    exec_id: String,
) -> Box<dyn Fn(TerminalSize) -> BoxFuture<'static, ()> + Send + Sync> {
    Box::new(move |size: TerminalSize| {
        let docker = docker.clone();
        let exec_id = exec_id.clone();
        Box::pin(async move {
            resize(&docker, &exec_id, size).await;
        })
    })
}

async fn resize(docker: &Docker, exec_id: &str, size: TerminalSize) {
    let _ = docker
        .resize_exec(
            exec_id,
            ResizeExecOptions {
                height: size.rows,
                width: size.cols,
            },
        )
        .await;
}

/// Poll `inspect_exec` until the process reports an exit code.
///
/// Docker only fills in `exit_code` once the exec has finished *and* its
/// output stream has been consumed, so a single inspect right after the
/// stream ends often still shows `running: true`. Polling briefly closes
/// that race; the timeout keeps a stuck daemon from blocking forever, in
/// which case `None` tells the caller the outcome is unknown rather than
/// implying success.
fn wait_for_exit(docker: Docker, exec_id: String) -> BoxFuture<'static, Option<i64>> {
    Box::pin(async move {
        let deadline = tokio::time::Instant::now() + EXIT_POLL_TIMEOUT;

        loop {
            match docker.inspect_exec(&exec_id).await {
                Ok(details) => {
                    if details.running != Some(true) {
                        return details.exit_code;
                    }
                }
                Err(_) => return None,
            }

            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(EXIT_POLL_INTERVAL).await;
        }
    })
}

/// An already-finished session, for runtimes and paths that need to hand
/// back a valid `ExecSession` without a live process behind it.
#[allow(dead_code)]
pub(crate) fn empty_session(exit_code: Option<i64>) -> ExecSession {
    ExecSession {
        output: Box::pin(stream::empty()),
        input: Box::pin(tokio::io::sink()),
        resize: Box::new(|_| Box::pin(async {})),
        wait: Box::pin(async move { exit_code }),
    }
}

/// Tests that need a live Docker daemon.
///
/// The security boundary this module enforces is a property of the *daemon's*
/// view of a container, not of our own types: only a real `inspect_container`
/// can tell us whether the label survived, and a hand-rolled fake would
/// assert against a daemon we invented. These self-skip when Docker is
/// unreachable so the suite stays green on machines without it.
#[cfg(test)]
mod docker_daemon_tests {
    use super::*;
    use crate::hypervisor::lifecycle_trait::ExecRequest;

    /// The image these tests run in. Any image with a shell would do; alpine
    /// is the smallest one to pull on a cold CI runner.
    const TEST_IMAGE: &str = "alpine:latest";

    async fn daemon() -> Option<Docker> {
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
    async fn ensure_image(docker: &Docker) -> bool {
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
    async fn start_container(docker: &Docker, name: &str, ring_managed: bool) -> String {
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder,
            StartContainerOptionsBuilder,
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
                super::super::RING_DEPLOYMENT_LABEL.to_string(),
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

    async fn remove(docker: &Docker, id: &str) {
        use bollard::query_parameters::RemoveContainerOptionsBuilder;
        let _ = docker
            .remove_container(
                id,
                Some(RemoveContainerOptionsBuilder::new().force(true).build()),
            )
            .await;
    }

    /// The boundary: a container Ring did not create must be unreachable even
    /// though it exists, is running, and its id is perfectly valid. Before
    /// the ownership check this exec succeeded and handed back a shell in a
    /// container belonging to something else on the host.
    #[tokio::test]
    async fn exec_refuses_a_container_ring_does_not_manage() {
        let Some(docker) = daemon().await else {
            eprintln!("skipping: no Docker daemon");
            return;
        };
        if !ensure_image(&docker).await {
            eprintln!("skipping: could not obtain {TEST_IMAGE}");
            return;
        }

        let id = start_container(&docker, "ring-exec-unmanaged", false).await;

        let result = exec(
            &docker,
            &id,
            ExecRequest {
                command: vec!["/bin/sh".to_string()],
                tty: false,
                size: None,
            },
        )
        .await;

        remove(&docker, &id).await;

        match result {
            Err(ExecError::InstanceUnavailable(_)) => {}
            Err(other) => panic!("expected InstanceUnavailable, got {other:?}"),
            Ok(_) => panic!("exec entered a container Ring does not manage"),
        }
    }

    /// The same container, labelled, must be reachable — otherwise the check
    /// above would pass simply by refusing everything.
    #[tokio::test]
    async fn exec_runs_in_a_ring_managed_container() {
        let Some(docker) = daemon().await else {
            eprintln!("skipping: no Docker daemon");
            return;
        };
        if !ensure_image(&docker).await {
            eprintln!("skipping: could not obtain {TEST_IMAGE}");
            return;
        }

        let id = start_container(&docker, "ring-exec-managed", true).await;

        let session = exec(
            &docker,
            &id,
            ExecRequest {
                command: vec!["/bin/echo".to_string(), "hello".to_string()],
                tty: false,
                size: None,
            },
        )
        .await;

        let outcome = match session {
            Ok(session) => {
                let chunks: Vec<_> = session.output.collect().await;
                let exit = session.wait.await;
                Some((chunks, exit))
            }
            Err(e) => {
                remove(&docker, &id).await;
                panic!("exec refused a Ring-managed container: {e}");
            }
        };

        remove(&docker, &id).await;

        let (chunks, exit) = outcome.expect("session");
        let stdout: Vec<u8> = chunks
            .into_iter()
            .filter_map(|c| match c {
                Ok(ExecOutput::Stdout(bytes)) => Some(bytes),
                _ => None,
            })
            .flatten()
            .collect();

        assert_eq!(String::from_utf8_lossy(&stdout).trim(), "hello");
        assert_eq!(exit, Some(0), "a successful command must report exit 0");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn collect(
        chunks: Vec<Result<LogOutput, bollard::errors::Error>>,
    ) -> Vec<Result<ExecOutput, ExecError>> {
        futures::executor::block_on(async {
            map_output(Box::pin(stream::iter(chunks)))
                .collect::<Vec<_>>()
                .await
        })
    }

    /// The happy-path frames, unwrapped — most assertions do not care that
    /// the stream is fallible, only that the bytes came through.
    fn collect_ok(chunks: Vec<Result<LogOutput, bollard::errors::Error>>) -> Vec<ExecOutput> {
        collect(chunks)
            .into_iter()
            .map(|item| item.expect("unexpected stream error"))
            .collect()
    }

    #[test]
    fn console_frames_map_to_stdout() {
        // A TTY session tags everything as Console; it must not vanish.
        let out = collect_ok(vec![Ok(LogOutput::Console {
            message: Bytes::from_static(b"hello"),
        })]);
        assert_eq!(out, vec![ExecOutput::Stdout(b"hello".to_vec())]);
    }

    #[test]
    fn stdout_and_stderr_stay_distinct_without_tty() {
        let out = collect_ok(vec![
            Ok(LogOutput::StdOut {
                message: Bytes::from_static(b"out"),
            }),
            Ok(LogOutput::StdErr {
                message: Bytes::from_static(b"err"),
            }),
        ]);
        assert_eq!(
            out,
            vec![
                ExecOutput::Stdout(b"out".to_vec()),
                ExecOutput::Stderr(b"err".to_vec()),
            ]
        );
    }

    #[test]
    fn stdin_echoes_are_dropped() {
        // Forwarding these would double every keystroke back to the client.
        let out = collect_ok(vec![
            Ok(LogOutput::StdIn {
                message: Bytes::from_static(b"typed"),
            }),
            Ok(LogOutput::StdOut {
                message: Bytes::from_static(b"real"),
            }),
        ]);
        assert_eq!(out, vec![ExecOutput::Stdout(b"real".to_vec())]);
    }

    #[test]
    fn stream_error_is_surfaced_and_ends_the_session() {
        // Regression: this used to be dropped by a `filter_map`, so a daemon
        // that died mid-command was indistinguishable from a clean EOF and
        // the frames after the failure were still delivered as if the stream
        // were healthy.
        let out = collect(vec![
            Ok(LogOutput::StdOut {
                message: Bytes::from_static(b"before"),
            }),
            Err(bollard::errors::Error::MissingSessionBuildkitError {}),
            Ok(LogOutput::StdOut {
                message: Bytes::from_static(b"after"),
            }),
        ]);

        assert_eq!(out.len(), 2, "stream must stop at the error: {out:?}");
        assert_eq!(out[0], Ok(ExecOutput::Stdout(b"before".to_vec())));
        assert!(
            matches!(out[1], Err(ExecError::Failed(_))),
            "the error must reach the caller, got {:?}",
            out[1]
        );
    }

    #[test]
    fn empty_session_reports_its_exit_code() {
        let session = empty_session(Some(0));
        assert_eq!(futures::executor::block_on(session.wait), Some(0));
    }
}
