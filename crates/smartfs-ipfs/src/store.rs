use async_trait::async_trait;
use smartfs_schema::{Result, SmartFsError, Uuid};
use smartfs_store::BlobStore;
use std::path::PathBuf;
use tokio::fs;
use tokio::io::{AsyncRead, AsyncReadExt};

/// @id: 4562c4fb-5f2d-423d-86f4-add5090ae4ca
/// IPFS storage backend implementing `BlobStore` via the Kubo daemon HTTP API.
#[derive(Debug, Clone)]
pub struct IpfsStore {
    api_endpoint: String,
    gateway_endpoint: String,
    client: reqwest::Client,
}

/// @id: c212eaa5-6808-4055-9f3b-ecf49eb0a341
impl IpfsStore {
    /// @id: 88ebea89-da84-4dde-923e-38d0fddedb46
    /// Creates a new `IpfsStore` with configured API and Gateway endpoints.
    pub fn new(api_endpoint: impl Into<String>, gateway_endpoint: impl Into<String>) -> Self {
        Self {
            api_endpoint: api_endpoint.into(),
            gateway_endpoint: gateway_endpoint.into(),
            client: reqwest::Client::new(),
        }
    }

    /// @id: a260392b-9918-42b9-a283-47c17e5e62cc
    /// Default Kubo endpoints (http://127.0.0.1:5001 for API, http://127.0.0.1:8080 for Gateway).
    pub fn default_local() -> Self {
        Self::new("http://127.0.0.1:5001", "http://127.0.0.1:8080")
    }
}

/// @id: 9a4bef1e-0561-45d5-afbe-173332f4f408
#[async_trait]
impl BlobStore for IpfsStore {
    async fn put(&self, uuid: Uuid, data: &[u8]) -> Result<()> {
        let add_url = format!("{}/api/v0/add", self.api_endpoint);
        let part = reqwest::multipart::Part::bytes(data.to_vec())
            .file_name(uuid.to_string());
        let form = reqwest::multipart::Form::new().part("file", part);

        let res = self.client.post(&add_url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| SmartFsError::Store(format!("IPFS add failed: {e}")))?;

        if !res.status().is_success() {
            return Err(SmartFsError::Store(format!(
                "IPFS add returned status {}",
                res.status()
            )));
        }

        let body: serde_json::Value = res.json().await
            .map_err(|e| SmartFsError::Store(format!("failed to parse IPFS response: {e}")))?;

        if let Some(hash) = body.get("Hash").and_then(|h| h.as_str()) {
            let pin_url = format!("{}/api/v0/pin/add?arg={}", self.api_endpoint, hash);
            self.client.post(&pin_url).send().await
                .map_err(|e| SmartFsError::Store(format!("IPFS pin failed: {e}")))?;
        }

        Ok(())
    }

    async fn get(&self, uuid: Uuid, external_path: Option<&str>) -> Result<Vec<u8>> {
        if let Some(path) = external_path {
            return fs::read(PathBuf::from(path)).await.map_err(SmartFsError::from);
        }

        let url = format!("{}/ipfs/{}", self.gateway_endpoint, uuid);
        let res = self.client.get(&url).send().await
            .map_err(|e| SmartFsError::Store(format!("IPFS gateway fetch failed: {e}")))?;

        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(SmartFsError::NotFound(format!("IPFS blob {uuid} not found")));
        }

        if !res.status().is_success() {
            return Err(SmartFsError::Store(format!("IPFS gateway status: {}", res.status())));
        }

        let bytes = res.bytes().await
            .map_err(|e| SmartFsError::Store(format!("failed to read IPFS body: {e}")))?;

        Ok(bytes.to_vec())
    }

    async fn put_stream(
        &self,
        uuid: Uuid,
        mut stream: Box<dyn AsyncRead + Send + Unpin>,
        _size_hint: Option<u64>,
    ) -> Result<()> {
        let mut buffer = Vec::new();
        stream.read_to_end(&mut buffer).await?;
        self.put(uuid, &buffer).await
    }

    async fn get_stream(
        &self,
        uuid: Uuid,
        external_path: Option<&str>,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        let bytes = self.get(uuid, external_path).await?;
        Ok(Box::new(std::io::Cursor::new(bytes)))
    }

    async fn delete(&self, uuid: Uuid) -> Result<()> {
        let url = format!("{}/api/v0/pin/rm?arg={}", self.api_endpoint, uuid);
        let res = self.client.post(&url).send().await
            .map_err(|e| SmartFsError::Store(format!("IPFS pin/rm failed: {e}")))?;

        if !res.status().is_success() && res.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(SmartFsError::Store(format!("IPFS pin/rm error: {}", res.status())));
        }

        Ok(())
    }

    async fn exists(&self, uuid: Uuid) -> Result<bool> {
        let url = format!("{}/ipfs/{}", self.gateway_endpoint, uuid);
        let res = self.client.head(&url).send().await
            .map_err(|e| SmartFsError::Store(format!("IPFS exists check failed: {e}")))?;

        Ok(res.status().is_success())
    }
}
