use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use hyperlocal::{UnixConnector, Uri};
use serde::{Deserialize, Serialize};

/// Client for the Cloud Hypervisor REST API over Unix socket.
///
/// Each VM instance runs its own cloud-hypervisor process with its own socket.
/// The socket path is typically: `<socket_dir>/<vm_id>.sock`
pub(crate) struct CloudHypervisorClient {
    socket_path: String,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct VmConfig {
    pub payload: PayloadConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<CpuConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disks: Option<Vec<DiskConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net: Option<Vec<NetConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<ConsoleConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub console: Option<ConsoleConfig>,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct PayloadConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initramfs: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct CpuConfig {
    pub boot_vcpus: u32,
    pub max_vcpus: u32,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct MemoryConfig {
    pub size: u64,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct DiskConfig {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readonly: Option<bool>,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct NetConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tap: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct ConsoleConfig {
    pub mode: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VmInfo {
    pub state: String,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
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

impl CloudHypervisorClient {
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
                .body(Full::new(Bytes::from(b)))
                .map_err(|e| ClientError::Http(e.to_string()))?,
            None => Request::builder()
                .method(method)
                .uri(uri)
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

    /// Create a VM with the given configuration.
    pub async fn create_vm(&self, config: &VmConfig) -> Result<(), ClientError> {
        let body = serde_json::to_string(config).map_err(|e| ClientError::Json(e.to_string()))?;
        self.request(Method::PUT, "/api/v1/vm.create", Some(body))
            .await?;
        Ok(())
    }

    /// Boot a previously created VM.
    pub async fn boot_vm(&self) -> Result<(), ClientError> {
        self.request(Method::PUT, "/api/v1/vm.boot", None).await?;
        Ok(())
    }

    /// Shut down a running VM.
    pub async fn shutdown_vm(&self) -> Result<(), ClientError> {
        self.request(Method::PUT, "/api/v1/vm.shutdown", None)
            .await?;
        Ok(())
    }

    /// Delete a VM instance.
    pub async fn delete_vm(&self) -> Result<(), ClientError> {
        self.request(Method::PUT, "/api/v1/vm.delete", None).await?;
        Ok(())
    }

    /// Get VM info and state.
    pub async fn info(&self) -> Result<VmInfo, ClientError> {
        let body = self
            .request(Method::GET, "/api/v1/vm.info", None)
            .await?;
        serde_json::from_str(&body).map_err(|e| ClientError::Json(e.to_string()))
    }

    /// Ping the VMM to check if it's alive.
    pub async fn ping(&self) -> Result<(), ClientError> {
        self.request(Method::PUT, "/api/v1/vmm.ping", None).await?;
        Ok(())
    }
}
