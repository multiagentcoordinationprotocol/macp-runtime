use super::resolver::AuthResolver;
use crate::security::AuthIdentity;
use macp_core::error::MacpError;
use tonic::metadata::MetadataMap;

pub struct AuthResolverChain {
    resolvers: Vec<Box<dyn AuthResolver>>,
}

impl AuthResolverChain {
    pub fn new(resolvers: Vec<Box<dyn AuthResolver>>) -> Self {
        let names: Vec<&str> = resolvers.iter().map(|r| r.name()).collect();
        tracing::info!(chain = ?names, "auth resolver chain initialized");
        Self { resolvers }
    }

    pub async fn authenticate(&self, metadata: &MetadataMap) -> Result<AuthIdentity, MacpError> {
        for resolver in &self.resolvers {
            match resolver.resolve(metadata).await {
                Ok(Some(identity)) => {
                    tracing::debug!(
                        resolver = resolver.name(),
                        sender = %identity.sender,
                        "authenticated"
                    );
                    return Ok(identity.into());
                }
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(
                        resolver = resolver.name(),
                        error = %e,
                        "auth resolver rejected credential"
                    );
                    return Err(MacpError::Unauthenticated);
                }
            }
        }
        Err(MacpError::Unauthenticated)
    }
}
