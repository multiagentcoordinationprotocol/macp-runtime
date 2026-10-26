//! The generated `macp.v1` gRPC client and the endpoint URL rules, shared by
//! [`crate::GrpcTarget`] and the `macp` command line.

use std::time::Duration;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

#[allow(clippy::all, clippy::pedantic, missing_docs)]
mod proto {
    tonic::include_proto!("macp.v1");
}

pub use proto::macp_runtime_service_client::MacpRuntimeServiceClient;

/// Connect to `url`: `grpc://host:port` or `http://host:port` in the clear,
/// `grpcs://` or `https://` over TLS (WebPKI roots).
pub async fn connect(url: &str) -> Result<MacpRuntimeServiceClient<Channel>, String> {
    let (endpoint_url, tls) = if let Some(rest) = url.strip_prefix("grpc://") {
        (format!("http://{rest}"), false)
    } else if let Some(rest) = url.strip_prefix("grpcs://") {
        (format!("https://{rest}"), true)
    } else if url.starts_with("https://") {
        (url.to_string(), true)
    } else if url.starts_with("http://") {
        (url.to_string(), false)
    } else {
        return Err(format!(
            "unsupported target URL {url} (use grpc://, grpcs://, http:// or https://)"
        ));
    };
    let mut endpoint = Endpoint::from_shared(endpoint_url)
        .map_err(|e| format!("bad target URL {url}: {e}"))?
        .connect_timeout(Duration::from_secs(10));
    if tls {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .map_err(|e| format!("TLS setup failed: {e}"))?;
    }
    let channel = endpoint
        .connect()
        .await
        .map_err(|e| format!("cannot connect to {url}: {e}"))?;
    Ok(MacpRuntimeServiceClient::new(channel))
}
