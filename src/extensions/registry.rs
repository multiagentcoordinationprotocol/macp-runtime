use super::provider::{SessionExtensionProvider, SessionOutcome};
use std::collections::HashMap;

pub struct ExtensionProviderRegistry {
    providers: Vec<Box<dyn SessionExtensionProvider>>,
}

impl ExtensionProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn register(&mut self, provider: Box<dyn SessionExtensionProvider>) {
        tracing::info!(key = provider.key(), "registered extension provider");
        self.providers.push(provider);
    }

    pub async fn on_session_start(&self, session_id: &str, extensions: &HashMap<String, Vec<u8>>) {
        for provider in &self.providers {
            if !extensions.contains_key(provider.key()) {
                continue;
            }
            if let Err(e) = provider.on_session_start(session_id, extensions).await {
                tracing::warn!(
                    key = provider.key(),
                    session_id,
                    error = %e,
                    "extension provider on_session_start failed (non-fatal)"
                );
            }
        }
    }

    pub async fn on_session_terminal(&self, session_id: &str, outcome: SessionOutcome) {
        for provider in &self.providers {
            if let Err(e) = provider
                .on_session_terminal(session_id, outcome_ref(&outcome))
                .await
            {
                tracing::warn!(
                    key = provider.key(),
                    session_id,
                    error = %e,
                    "extension provider on_session_terminal failed (non-fatal)"
                );
            }
        }
    }
}

impl Default for ExtensionProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn outcome_ref(outcome: &SessionOutcome) -> SessionOutcome {
    match outcome {
        SessionOutcome::Resolved => SessionOutcome::Resolved,
        SessionOutcome::Expired => SessionOutcome::Expired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::provider::ExtensionError;
    use std::sync::{Arc, Mutex};

    /// Provider that records every lifecycle callback it receives.
    struct RecordingProvider {
        key: &'static str,
        calls: Arc<Mutex<Vec<String>>>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl SessionExtensionProvider for RecordingProvider {
        fn key(&self) -> &str {
            self.key
        }

        async fn on_session_start(
            &self,
            session_id: &str,
            _extensions: &HashMap<String, Vec<u8>>,
        ) -> Result<(), ExtensionError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}:start:{session_id}", self.key));
            if self.fail {
                Err(ExtensionError::Internal("boom".into()))
            } else {
                Ok(())
            }
        }

        async fn on_session_terminal(
            &self,
            session_id: &str,
            outcome: SessionOutcome,
        ) -> Result<(), ExtensionError> {
            let outcome = match outcome {
                SessionOutcome::Resolved => "resolved",
                SessionOutcome::Expired => "expired",
            };
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}:terminal:{session_id}:{outcome}", self.key));
            if self.fail {
                Err(ExtensionError::Internal("boom".into()))
            } else {
                Ok(())
            }
        }
    }

    fn recording(
        key: &'static str,
        fail: bool,
    ) -> (Box<RecordingProvider>, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        (
            Box::new(RecordingProvider {
                key,
                calls: Arc::clone(&calls),
                fail,
            }),
            calls,
        )
    }

    fn extensions_with_key(key: &str) -> HashMap<String, Vec<u8>> {
        let mut map = HashMap::new();
        map.insert(key.to_string(), b"cfg".to_vec());
        map
    }

    #[tokio::test]
    async fn on_session_start_dispatches_only_to_providers_with_matching_key() {
        let mut registry = ExtensionProviderRegistry::new();
        let (a, a_calls) = recording("ext.a", false);
        let (b, b_calls) = recording("ext.b", false);
        registry.register(a);
        registry.register(b);

        registry
            .on_session_start("s1", &extensions_with_key("ext.a"))
            .await;

        assert_eq!(*a_calls.lock().unwrap(), vec!["ext.a:start:s1"]);
        assert!(
            b_calls.lock().unwrap().is_empty(),
            "provider whose key is absent from the extensions map must not be invoked"
        );
    }

    #[tokio::test]
    async fn on_session_start_with_unknown_key_invokes_no_provider() {
        let mut registry = ExtensionProviderRegistry::new();
        let (a, a_calls) = recording("ext.a", false);
        registry.register(a);

        registry
            .on_session_start("s1", &extensions_with_key("ext.unknown"))
            .await;
        registry.on_session_start("s2", &HashMap::new()).await;

        assert!(a_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn on_session_terminal_notifies_all_registered_providers() {
        let mut registry = ExtensionProviderRegistry::new();
        let (a, a_calls) = recording("ext.a", false);
        let (b, b_calls) = recording("ext.b", false);
        registry.register(a);
        registry.register(b);

        registry
            .on_session_terminal("s1", SessionOutcome::Resolved)
            .await;
        registry
            .on_session_terminal("s2", SessionOutcome::Expired)
            .await;

        assert_eq!(
            *a_calls.lock().unwrap(),
            vec!["ext.a:terminal:s1:resolved", "ext.a:terminal:s2:expired"]
        );
        assert_eq!(
            *b_calls.lock().unwrap(),
            vec!["ext.b:terminal:s1:resolved", "ext.b:terminal:s2:expired"]
        );
    }

    #[tokio::test]
    async fn provider_failure_is_non_fatal_and_later_providers_still_run() {
        let mut registry = ExtensionProviderRegistry::new();
        let (failing, failing_calls) = recording("ext.a", true);
        let (ok, ok_calls) = recording("ext.b", false);
        registry.register(failing);
        registry.register(ok);

        let mut extensions = extensions_with_key("ext.a");
        extensions.insert("ext.b".to_string(), vec![]);
        registry.on_session_start("s1", &extensions).await;
        registry
            .on_session_terminal("s1", SessionOutcome::Resolved)
            .await;

        // The failing provider was invoked and its error swallowed (E-1);
        // the provider registered after it still ran for both callbacks.
        assert_eq!(
            *failing_calls.lock().unwrap(),
            vec!["ext.a:start:s1", "ext.a:terminal:s1:resolved"]
        );
        assert_eq!(
            *ok_calls.lock().unwrap(),
            vec!["ext.b:start:s1", "ext.b:terminal:s1:resolved"]
        );
    }

    #[tokio::test]
    async fn duplicate_key_registration_keeps_both_providers() {
        // The registry is an append-only dispatch list: registering a second
        // provider under the same key does not replace the first — both are
        // invoked, in registration order.
        let mut registry = ExtensionProviderRegistry::new();
        let (first, first_calls) = recording("ext.a", false);
        let (second, second_calls) = recording("ext.a", false);
        registry.register(first);
        registry.register(second);

        registry
            .on_session_start("s1", &extensions_with_key("ext.a"))
            .await;

        assert_eq!(*first_calls.lock().unwrap(), vec!["ext.a:start:s1"]);
        assert_eq!(*second_calls.lock().unwrap(), vec!["ext.a:start:s1"]);
    }

    #[tokio::test]
    async fn empty_registry_dispatch_is_a_no_op() {
        let registry = ExtensionProviderRegistry::default();
        registry
            .on_session_start("s1", &extensions_with_key("ext.a"))
            .await;
        registry
            .on_session_terminal("s1", SessionOutcome::Expired)
            .await;
    }
}
