//! Instance-level runtime operations: report facts, start and stop instances,
//! make no decisions.
//!
//! Every runtime implements [`InstanceDriver`] and leaves the restart policy,
//! the backoff and the scaling decisions to the scheduler
//! (`scheduler::reconcile`), which runs the same code for all of them.

use crate::hypervisor::error::RuntimeError;
use crate::models::deployments::Deployment;
use crate::models::restart_state::Termination;
use crate::models::volume::ResolvedMount;
use async_trait::async_trait;

/// The instances of one deployment, as the runtime sees them now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Observation {
    /// Live instances.
    pub(crate) running: Vec<String>,
    /// Instances that ended on their own, oldest first. Instances the
    /// scheduler stopped itself are not reported. Each one keeps being
    /// reported until it is [discarded](InstanceDriver::discard_instance).
    pub(crate) terminated: Vec<Termination>,
}

#[async_trait]
pub(crate) trait InstanceDriver: Send + Sync {
    async fn observe(&self, deployment: &Deployment) -> Observation;

    /// Create and start one instance, appending its id to
    /// `deployment.instances`. Never touches the status or the counters.
    async fn start_instance(
        &self,
        deployment: &mut Deployment,
        resolved_mounts: &[ResolvedMount],
    ) -> Result<(), RuntimeError>;

    /// Stop and remove a live instance. Its exit is not reported as a
    /// termination.
    async fn stop_instance(&self, instance_id: &str) -> bool;

    /// Remove a terminated instance once its termination has been handled.
    async fn discard_instance(&self, instance_id: &str);

    /// The terminated instance of a finished job. By default it is kept, and
    /// its logs with it, until the deployment is deleted. A runtime for which
    /// an ended instance still holds real resources (a microVM's disk copy)
    /// removes it instead.
    async fn retire_instance(&self, _instance_id: &str) {}

    /// Remove every instance of a deleted deployment, live or ended, and
    /// whatever the runtime kept for them. Clears `deployment.instances`.
    ///
    /// An error means the runtime could not even be asked: the deployment is
    /// kept, and torn down again on a later pass, rather than forgotten with
    /// its instances still running.
    async fn teardown(&self, deployment: &mut Deployment) -> Result<(), RuntimeError>;
}
