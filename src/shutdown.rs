//! One process-wide shutdown signal for every listener.
//!
//! The first SIGINT (ctrl-c) or, on Unix, SIGTERM flips a
//! `tokio::sync::watch` to `true`; the gRPC server, the drain deadline and the
//! Prometheus accept loop each hold a receiver, so none of them can miss the
//! signal or race another listener for it. A second signal during the drain
//! exits the process immediately, with `128 + signo`, skipping the final
//! snapshot.
//!
//! Binary-private (declared from `main.rs`, not `lib.rs`): not public API.

use tokio::sync::watch;

/// A cloneable handle on the shared shutdown state.
#[derive(Clone)]
pub struct ShutdownSignal {
    rx: watch::Receiver<bool>,
}

impl ShutdownSignal {
    /// Install the signal handlers and spawn the task that feeds the watch.
    /// Must be called inside a Tokio runtime. On Unix both handlers are
    /// registered synchronously here, so a signal arriving right after this
    /// returns is never lost to the default disposition.
    pub fn install() -> Self {
        let (tx, rx) = watch::channel(false);
        let mut signals = Signals::new();
        tokio::spawn(async move {
            let (name, _) = signals.next().await;
            tracing::info!(
                signal = name,
                "shutdown signal received; draining in-flight requests"
            );
            let _ = tx.send(true);

            // A second signal during the drain: exit now.
            let (name, code) = signals.next().await;
            tracing::warn!(
                signal = name,
                "second shutdown signal during drain; exiting immediately"
            );
            std::process::exit(code);
        });
        Self { rx }
    }

    /// Resolve once shutdown has been signalled (immediately if it already
    /// was).
    pub async fn wait(mut self) {
        // `wait_for` errors only if the sender dropped, which happens only if
        // the feeding task died; treat that as "never signalled".
        if self.rx.wait_for(|fired| *fired).await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// SIGINT + SIGTERM streams, registered eagerly on Unix.
#[cfg(unix)]
struct Signals {
    int: tokio::signal::unix::Signal,
    term: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    fn new() -> Self {
        use tokio::signal::unix::{signal, SignalKind};
        Self {
            int: signal(SignalKind::interrupt()).expect("install SIGINT handler"),
            term: signal(SignalKind::terminate()).expect("install SIGTERM handler"),
        }
    }

    /// The next signal's name and its conventional `128 + signo` exit code.
    async fn next(&mut self) -> (&'static str, i32) {
        tokio::select! {
            _ = self.int.recv() => ("SIGINT", 130),
            _ = self.term.recv() => ("SIGTERM", 143),
        }
    }
}

/// Non-Unix builds keep ctrl-c only.
#[cfg(not(unix))]
struct Signals;

#[cfg(not(unix))]
impl Signals {
    fn new() -> Self {
        Self
    }

    async fn next(&mut self) -> (&'static str, i32) {
        let _ = tokio::signal::ctrl_c().await;
        ("SIGINT", 130)
    }
}
