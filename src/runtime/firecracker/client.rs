//! Client for the Firecracker REST API over a Unix control socket.
//!
//! Each microVM is one `firecracker` process with its own API socket
//! (`<socket_dir>/<instance_id>.sock`). Unlike Cloud Hypervisor's monolithic
//! `PUT /api/v1/vm.create`, Firecracker is configured by a *sequence* of small
//! resource PUTs — `/boot-source`, `/drives/<id>`, `/machine-config` — followed
//! by `PUT /actions {InstanceStart}` to boot. State is read back from `GET /`
//! (the instance info endpoint).
//!
//! This mirrors the manual flow proven by `tests/e2e/firecracker/spike_boot.sh`.
//! The HTTP transport (hyper + hyperlocal over a `UnixConnector`) is identical
//! to `cloud_hypervisor::client`; only the endpoints and payload shapes differ.

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use hyperlocal::{UnixConnector, Uri};
use serde::{Deserialize, Serialize};

/// Boot source: the uncompressed kernel image plus its command line. Firecracker
/// boots a kernel directly — there is no firmware step like hypervisor-fw.
#[derive(Debug, Serialize, Clone)]
pub(crate) struct BootSource {
    pub kernel_image_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_args: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initrd_path: Option<String>,
}

/// One block device. The root device is `/dev/vda`; additional drives attach in
/// order. `path_on_host` is a raw image on the host filesystem.
#[derive(Debug, Serialize, Clone)]
pub(crate) struct Drive {
    pub drive_id: String,
    pub path_on_host: String,
    pub is_root_device: bool,
    pub is_read_only: bool,
}

/// CPU + memory sizing. `smt` (hyper-threading) is left at Firecracker's default.
#[derive(Debug, Serialize, Clone)]
pub(crate) struct MachineConfig {
    pub vcpu_count: u32,
    pub mem_size_mib: u32,
}

/// One TAP-backed network interface. `host_dev_name` is the host tap device;
/// the guest MAC is set so the guest gets a deterministic address. Reserved for
/// the networking phase — not sent in the boot-minimal flow.
#[derive(Debug, Serialize, Clone)]
pub(crate) struct NetworkInterface {
    pub iface_id: String,
    pub host_dev_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guest_mac: Option<String>,
}

/// Instance actions. The only one used at boot is `InstanceStart`; graceful
/// shutdown uses `SendCtrlAltDel` (the guest must run an ACPI handler).
#[derive(Debug, Serialize, Clone)]
pub(crate) struct Action {
    pub action_type: String,
}

/// Subset of `GET /` (instance info). `state` is `Not started`, `Running`, or
/// `Paused`. Consumed by the state-verification + health phases.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct InstanceInfo {
    pub state: String,
    #[serde(default)]
    #[allow(dead_code)] // Part of the info response; kept for completeness/diagnostics.
    pub id: Option<String>,
}

#[derive(Debug)]
pub(crate) enum ClientError {
    Http(String),
    Api { status: StatusCode, body: String },
    Json(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Http(e) => write!(f, "HTTP error: {}", e),
            ClientError::Api { status, body } => write!(f, "API error ({}): {}", status, body),
            ClientError::Json(e) => write!(f, "JSON error: {}", e),
        }
    }
}

/// Talks to one Firecracker process over its API socket.
pub(crate) struct FirecrackerClient {
    socket_path: String,
}

impl FirecrackerClient {
    pub fn new(socket_path: &str) -> Self {
        Self {
            socket_path: socket_path.to_string(),
        }
    }

    async fn request(
        &self,
        method: Method,
        endpoint: &str,
        body: Option<String>,
    ) -> Result<String, ClientError> {
        let uri: hyper::Uri = Uri::new(&self.socket_path, endpoint).into();
        let connector = UnixConnector;
        let client: Client<UnixConnector, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).build(connector);

        let req = match body {
            Some(b) => Request::builder()
                .method(method)
                .uri(uri)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json")
                .body(Full::new(Bytes::from(b)))
                .map_err(|e| ClientError::Http(e.to_string()))?,
            None => Request::builder()
                .method(method)
                .uri(uri)
                .header("Accept", "application/json")
                .body(Full::new(Bytes::new()))
                .map_err(|e| ClientError::Http(e.to_string()))?,
        };

        let response = client
            .request(req)
            .await
            .map_err(|e| ClientError::Http(e.to_string()))?;

        let status = response.status();
        let body_bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| ClientError::Http(e.to_string()))?
            .to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();

        if status.is_success() || status == StatusCode::NO_CONTENT {
            Ok(body_str)
        } else {
            Err(ClientError::Api {
                status,
                body: body_str,
            })
        }
    }

    /// `PUT /boot-source` — set the kernel and command line.
    pub async fn put_boot_source(&self, source: &BootSource) -> Result<(), ClientError> {
        let body = serde_json::to_string(source).map_err(|e| ClientError::Json(e.to_string()))?;
        self.request(Method::PUT, "/boot-source", Some(body))
            .await?;
        Ok(())
    }

    /// `PUT /drives/<drive_id>` — attach a block device.
    pub async fn put_drive(&self, drive: &Drive) -> Result<(), ClientError> {
        let endpoint = format!("/drives/{}", drive.drive_id);
        let body = serde_json::to_string(drive).map_err(|e| ClientError::Json(e.to_string()))?;
        self.request(Method::PUT, &endpoint, Some(body)).await?;
        Ok(())
    }

    /// `PUT /machine-config` — set vCPU count and memory.
    pub async fn put_machine_config(&self, config: &MachineConfig) -> Result<(), ClientError> {
        let body = serde_json::to_string(config).map_err(|e| ClientError::Json(e.to_string()))?;
        self.request(Method::PUT, "/machine-config", Some(body))
            .await?;
        Ok(())
    }

    /// `PUT /network-interfaces/<id>` — attach a TAP interface. Reserved for the
    /// networking phase.
    #[allow(dead_code)]
    pub async fn put_network_interface(&self, iface: &NetworkInterface) -> Result<(), ClientError> {
        let endpoint = format!("/network-interfaces/{}", iface.iface_id);
        let body = serde_json::to_string(iface).map_err(|e| ClientError::Json(e.to_string()))?;
        self.request(Method::PUT, &endpoint, Some(body)).await?;
        Ok(())
    }

    /// `PUT /actions` — issue an instance action (`InstanceStart`, `SendCtrlAltDel`).
    pub async fn action(&self, action_type: &str) -> Result<(), ClientError> {
        let action = Action {
            action_type: action_type.to_string(),
        };
        let body = serde_json::to_string(&action).map_err(|e| ClientError::Json(e.to_string()))?;
        self.request(Method::PUT, "/actions", Some(body)).await?;
        Ok(())
    }

    /// Convenience: boot a configured VM.
    pub async fn start(&self) -> Result<(), ClientError> {
        self.action("InstanceStart").await
    }

    /// Convenience: request a graceful guest shutdown via ACPI.
    pub async fn send_ctrl_alt_del(&self) -> Result<(), ClientError> {
        self.action("SendCtrlAltDel").await
    }

    /// `GET /` — read instance info (including `state`). Used by the
    /// state-verification + health phases.
    #[allow(dead_code)]
    pub async fn info(&self) -> Result<InstanceInfo, ClientError> {
        let body = self.request(Method::GET, "/", None).await?;
        serde_json::from_str(&body).map_err(|e| ClientError::Json(e.to_string()))
    }
}
