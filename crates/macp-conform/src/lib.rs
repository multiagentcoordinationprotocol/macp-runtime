//! # macp-conform
//!
//! The MACP conformance runner. It drives the spec's canonical conformance
//! fixtures (`schemas/conformance/*.json`) and commitment-hash vectors
//! (`schemas/conformance/cmt-hash/*.json`) against an implementation, and
//! reports which conformance level (spec `docs/conformance.md`) the
//! implementation reaches, as a machine-readable [`Report`].
//!
//! - **Fixtures** load from a directory ([`FixtureSet::from_dir`]) or from a
//!   pinned spec release bundle `macp-conformance-<version>.tar.gz`, checked
//!   against its `SHA256SUMS` ([`FixtureSet::from_bundle`]).
//! - **Targets** implement [`ConformanceTarget`]. [`InProcessTarget`] runs the
//!   Embedded profile over `macp_modes::step` and `macp_modes::replay`, with
//!   no transport. With the `grpc` feature, `GrpcTarget` runs the Server
//!   profile against any `MACPRuntimeService` endpoint.
//! - **Levels**: Core, Core + Decision, Standard modes, plus the optional
//!   multi_round group; each result is reported per [`Level`].
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use macp_conform::{run, FixtureSet, InProcessTarget, Level, RunOptions};
//! let set = FixtureSet::from_dir("schemas/conformance")?;
//! let mut target = InProcessTarget::new();
//! let report = run(&mut target, &set, &RunOptions::default().with_level(Level::Standard)).await;
//! println!("{}", serde_json::to_string_pretty(&report)?);
//! assert!(!report.is_failure());
//! # Ok(()) }
//! ```
//!
//! Without the `grpc` feature this crate depends on no transport (no tonic).

mod bundle;
pub mod encode;
pub mod fixture;
mod in_process;
pub mod level;
pub mod report;
pub mod runner;
pub mod target;

#[cfg(feature = "grpc")]
mod grpc;

pub use fixture::{FixtureSet, FixtureSource, LoadError};
pub use in_process::{state_name, InProcessTarget};
pub use level::{Group, Level};
pub use report::{Outcome, Report, REPORT_SCHEMA};
pub use runner::{run, RunOptions};
pub use target::{
    Capabilities, ConformanceTarget, HistoryEntry, Observable, Observation, Profile, Submission,
    TargetError,
};

#[cfg(feature = "grpc")]
pub use grpc::GrpcTarget;
