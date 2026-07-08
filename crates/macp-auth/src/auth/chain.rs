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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::resolver::{AuthError, ResolvedIdentity};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    enum Outcome {
        Claim(&'static str),
        Decline,
        Fail,
    }

    struct StubResolver {
        name: &'static str,
        outcome: Outcome,
        calls: Arc<AtomicUsize>,
    }

    impl StubResolver {
        fn boxed(
            name: &'static str,
            outcome: Outcome,
        ) -> (Box<dyn AuthResolver>, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            (
                Box::new(Self {
                    name,
                    outcome,
                    calls: calls.clone(),
                }),
                calls,
            )
        }

        fn identity(&self, sender: &str) -> ResolvedIdentity {
            ResolvedIdentity {
                sender: sender.to_string(),
                allowed_modes: None,
                can_start_sessions: true,
                max_open_sessions: None,
                can_manage_mode_registry: false,
                is_observer: false,
                resolver: self.name.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl AuthResolver for StubResolver {
        fn name(&self) -> &str {
            self.name
        }

        async fn resolve(
            &self,
            _metadata: &MetadataMap,
        ) -> Result<Option<ResolvedIdentity>, AuthError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.outcome {
                Outcome::Claim(sender) => Ok(Some(self.identity(sender))),
                Outcome::Decline => Ok(None),
                Outcome::Fail => Err(AuthError::InvalidCredential("stub rejection".to_string())),
            }
        }
    }

    #[tokio::test]
    async fn first_resolver_that_claims_the_credential_wins() {
        let (first, first_calls) = StubResolver::boxed("first", Outcome::Claim("agent://first"));
        let (second, second_calls) =
            StubResolver::boxed("second", Outcome::Claim("agent://second"));
        let chain = AuthResolverChain::new(vec![first, second]);

        let identity = chain.authenticate(&MetadataMap::new()).await.expect("ok");
        assert_eq!(identity.sender, "agent://first");
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            second_calls.load(Ordering::SeqCst),
            0,
            "chain must stop at the first positive verification"
        );
    }

    #[tokio::test]
    async fn declining_resolver_passes_to_the_next() {
        let (first, first_calls) = StubResolver::boxed("first", Outcome::Decline);
        let (second, second_calls) =
            StubResolver::boxed("second", Outcome::Claim("agent://second"));
        let chain = AuthResolverChain::new(vec![first, second]);

        let identity = chain.authenticate(&MetadataMap::new()).await.expect("ok");
        assert_eq!(identity.sender, "agent://second");
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn all_resolvers_declining_yields_unauthenticated() {
        let (first, first_calls) = StubResolver::boxed("first", Outcome::Decline);
        let (second, second_calls) = StubResolver::boxed("second", Outcome::Decline);
        let chain = AuthResolverChain::new(vec![first, second]);

        let err = chain.authenticate(&MetadataMap::new()).await.unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated), "got {err:?}");
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
    }

    /// A resolver that claims the credential type but finds it invalid stops
    /// the chain with Unauthenticated — later resolvers must not get a second
    /// chance at a credential that positively failed verification.
    #[tokio::test]
    async fn resolver_error_stops_the_chain_as_unauthenticated() {
        let (first, first_calls) = StubResolver::boxed("first", Outcome::Fail);
        let (second, second_calls) =
            StubResolver::boxed("second", Outcome::Claim("agent://second"));
        let chain = AuthResolverChain::new(vec![first, second]);

        let err = chain.authenticate(&MetadataMap::new()).await.unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated), "got {err:?}");
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            second_calls.load(Ordering::SeqCst),
            0,
            "a hard resolver error must not fall through to later resolvers"
        );
    }

    #[tokio::test]
    async fn empty_chain_is_unauthenticated() {
        let chain = AuthResolverChain::new(vec![]);
        let err = chain.authenticate(&MetadataMap::new()).await.unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated), "got {err:?}");
    }
}
