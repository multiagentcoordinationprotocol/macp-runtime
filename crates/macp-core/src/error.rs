use crate::policy::DenyReason;
use thiserror::Error;

/// Protocol error vocabulary. `#[non_exhaustive]`: downstream matches must
/// carry a wildcard arm so new error variants are not a breaking change.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum MacpError {
    #[error("InvalidMacpVersion")]
    InvalidMacpVersion,
    #[error("InvalidEnvelope")]
    InvalidEnvelope,
    #[error("SessionAlreadyExists")]
    SessionAlreadyExists,
    #[error("UnknownSession")]
    UnknownSession,
    #[error("SessionNotOpen")]
    SessionNotOpen,
    #[error("TtlExpired")]
    TtlExpired,
    #[error("InvalidTtl")]
    InvalidTtl,
    #[error("UnknownMode")]
    UnknownMode,
    #[error("InvalidModeState")]
    InvalidModeState,
    #[error("InvalidPayload")]
    InvalidPayload,
    #[error("Forbidden")]
    Forbidden,
    #[error("Unauthenticated")]
    Unauthenticated,
    /// Kept for RFC error-code completeness.  The runtime currently represents
    /// duplicate detection via `ProcessResult { duplicate: true }` at the Ack
    /// level rather than returning this as an error.
    #[error("DuplicateMessage")]
    DuplicateMessage,
    #[error("PayloadTooLarge")]
    PayloadTooLarge,
    #[error("RateLimited")]
    RateLimited,
    #[error("StorageFailed")]
    StorageFailed,
    #[error("InvalidSessionId")]
    InvalidSessionId,
    #[error("UnknownPolicyVersion")]
    UnknownPolicyVersion,
    /// A governance policy denied a commitment. `reasons` are structured
    /// (RFC-MACP-0012 §6.2): each carries a stable code and keeps participant
    /// identifiers out of its message.
    #[error("PolicyDenied")]
    PolicyDenied { reasons: Vec<DenyReason> },
    #[error("InvalidPolicyDefinition")]
    InvalidPolicyDefinition,
}

impl MacpError {
    /// Returns the RFC error code string for this error variant.
    pub fn error_code(&self) -> &'static str {
        match self {
            MacpError::InvalidMacpVersion => "UNSUPPORTED_PROTOCOL_VERSION",
            MacpError::InvalidEnvelope => "INVALID_ENVELOPE",
            MacpError::SessionAlreadyExists => "SESSION_ALREADY_EXISTS",
            MacpError::UnknownSession => "SESSION_NOT_FOUND",
            MacpError::SessionNotOpen => "SESSION_NOT_OPEN",
            MacpError::TtlExpired => "SESSION_NOT_OPEN",
            MacpError::InvalidTtl => "INVALID_ENVELOPE",
            MacpError::UnknownMode => "MODE_NOT_SUPPORTED",
            MacpError::InvalidModeState => "INVALID_ENVELOPE",
            MacpError::InvalidPayload => "INVALID_ENVELOPE",
            MacpError::Forbidden => "FORBIDDEN",
            MacpError::Unauthenticated => "UNAUTHENTICATED",
            MacpError::DuplicateMessage => "DUPLICATE_MESSAGE",
            MacpError::PayloadTooLarge => "PAYLOAD_TOO_LARGE",
            MacpError::RateLimited => "RATE_LIMITED",
            MacpError::StorageFailed => "INTERNAL_ERROR",
            MacpError::InvalidSessionId => "INVALID_SESSION_ID",
            MacpError::UnknownPolicyVersion => "UNKNOWN_POLICY_VERSION",
            MacpError::PolicyDenied { .. } => "POLICY_DENIED",
            MacpError::InvalidPolicyDefinition => "INVALID_POLICY_DEFINITION",
        }
    }
}

/// A gRPC status code, as the error-codes registry's gRPC column names it
/// (proposal S13; RFC-MACP-0006 §3: "The gRPC binding MUST map error codes to
/// gRPC status codes per the error-codes registry").
///
/// Transport-free on purpose: `macp-core` does not depend on tonic. A gRPC
/// embedder converts with [`TransportStatus::grpc_code`] (the canonical
/// numeric value `tonic::Code::from_i32` accepts); an HTTP embedder uses
/// [`MacpError::http_status`] instead, because the HTTP column is finer
/// (409 vs 412, 413 vs 429) than the gRPC one.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransportStatus {
    InvalidArgument,
    NotFound,
    AlreadyExists,
    PermissionDenied,
    ResourceExhausted,
    FailedPrecondition,
    Internal,
    Unauthenticated,
}

impl TransportStatus {
    /// The canonical gRPC numeric status code.
    pub const fn grpc_code(self) -> i32 {
        match self {
            TransportStatus::InvalidArgument => 3,
            TransportStatus::NotFound => 5,
            TransportStatus::AlreadyExists => 6,
            TransportStatus::PermissionDenied => 7,
            TransportStatus::ResourceExhausted => 8,
            TransportStatus::FailedPrecondition => 9,
            TransportStatus::Internal => 13,
            TransportStatus::Unauthenticated => 16,
        }
    }

    /// The canonical gRPC status name, as the registry's gRPC column spells it.
    pub const fn grpc_name(self) -> &'static str {
        match self {
            TransportStatus::InvalidArgument => "INVALID_ARGUMENT",
            TransportStatus::NotFound => "NOT_FOUND",
            TransportStatus::AlreadyExists => "ALREADY_EXISTS",
            TransportStatus::PermissionDenied => "PERMISSION_DENIED",
            TransportStatus::ResourceExhausted => "RESOURCE_EXHAUSTED",
            TransportStatus::FailedPrecondition => "FAILED_PRECONDITION",
            TransportStatus::Internal => "INTERNAL",
            TransportStatus::Unauthenticated => "UNAUTHENTICATED",
        }
    }
}

/// One row of the error-codes registry: the code, its gRPC status, its HTTP
/// status.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistryEntry {
    pub code: &'static str,
    pub grpc: TransportStatus,
    pub http: u16,
}

const fn entry(code: &'static str, grpc: TransportStatus, http: u16) -> RegistryEntry {
    RegistryEntry { code, grpc, http }
}

/// The error-codes registry (`registries/error-codes.md` in the spec repo):
/// every code with its gRPC column (proposal S13) and HTTP column. The
/// single source for [`MacpError::transport_status`], [`MacpError::http_status`]
/// and the server's status mapping; pinned against the spec by tests.
pub const ERROR_CODE_REGISTRY: &[RegistryEntry] = &[
    entry("UNAUTHENTICATED", TransportStatus::Unauthenticated, 401),
    entry("FORBIDDEN", TransportStatus::PermissionDenied, 403),
    entry("SESSION_NOT_FOUND", TransportStatus::NotFound, 404),
    entry("SESSION_NOT_OPEN", TransportStatus::FailedPrecondition, 409),
    entry("DUPLICATE_MESSAGE", TransportStatus::AlreadyExists, 409),
    entry(
        "SESSION_ALREADY_EXISTS",
        TransportStatus::AlreadyExists,
        409,
    ),
    entry("INVALID_ENVELOPE", TransportStatus::InvalidArgument, 400),
    entry(
        "UNSUPPORTED_PROTOCOL_VERSION",
        TransportStatus::InvalidArgument,
        400,
    ),
    entry("MODE_NOT_SUPPORTED", TransportStatus::InvalidArgument, 400),
    entry("PAYLOAD_TOO_LARGE", TransportStatus::ResourceExhausted, 413),
    entry("RATE_LIMITED", TransportStatus::ResourceExhausted, 429),
    entry("INVALID_SESSION_ID", TransportStatus::InvalidArgument, 400),
    entry("INTERNAL_ERROR", TransportStatus::Internal, 500),
    entry("UNKNOWN_POLICY_VERSION", TransportStatus::NotFound, 404),
    entry("POLICY_DENIED", TransportStatus::PermissionDenied, 403),
    entry(
        "INVALID_POLICY_DEFINITION",
        TransportStatus::InvalidArgument,
        400,
    ),
    // Deprecated alias for FORBIDDEN; never emitted, kept so a lookup of a
    // peer's legacy code still resolves.
    entry("UNAUTHORIZED", TransportStatus::PermissionDenied, 403),
];

/// Look up a registry code (e.g. one received from a peer).
pub fn registry_entry(code: &str) -> Option<&'static RegistryEntry> {
    ERROR_CODE_REGISTRY.iter().find(|e| e.code == code)
}

impl MacpError {
    fn registry_row(&self) -> &'static RegistryEntry {
        registry_entry(self.error_code())
            .expect("every MacpError code is in ERROR_CODE_REGISTRY (pinned by a test)")
    }

    /// The gRPC status the registry assigns this error's code.
    pub fn transport_status(&self) -> TransportStatus {
        self.registry_row().grpc
    }

    /// The HTTP status the registry assigns this error's code.
    pub fn http_status(&self) -> u16 {
        self.registry_row().http
    }

    /// The structured denial reasons of a `PolicyDenied`, empty otherwise.
    pub fn deny_reasons(&self) -> &[DenyReason] {
        match self {
            MacpError::PolicyDenied { reasons } => reasons,
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_mapping_covers_all_variants() {
        let cases: Vec<(MacpError, &str)> = vec![
            (
                MacpError::InvalidMacpVersion,
                "UNSUPPORTED_PROTOCOL_VERSION",
            ),
            (MacpError::InvalidEnvelope, "INVALID_ENVELOPE"),
            (MacpError::SessionAlreadyExists, "SESSION_ALREADY_EXISTS"),
            (MacpError::UnknownSession, "SESSION_NOT_FOUND"),
            (MacpError::SessionNotOpen, "SESSION_NOT_OPEN"),
            (MacpError::TtlExpired, "SESSION_NOT_OPEN"),
            (MacpError::InvalidTtl, "INVALID_ENVELOPE"),
            (MacpError::UnknownMode, "MODE_NOT_SUPPORTED"),
            (MacpError::InvalidModeState, "INVALID_ENVELOPE"),
            (MacpError::InvalidPayload, "INVALID_ENVELOPE"),
            (MacpError::Forbidden, "FORBIDDEN"),
            (MacpError::Unauthenticated, "UNAUTHENTICATED"),
            (MacpError::DuplicateMessage, "DUPLICATE_MESSAGE"),
            (MacpError::PayloadTooLarge, "PAYLOAD_TOO_LARGE"),
            (MacpError::RateLimited, "RATE_LIMITED"),
            (MacpError::StorageFailed, "INTERNAL_ERROR"),
            (MacpError::InvalidSessionId, "INVALID_SESSION_ID"),
            (MacpError::UnknownPolicyVersion, "UNKNOWN_POLICY_VERSION"),
            (
                MacpError::PolicyDenied {
                    reasons: vec!["test".into()],
                },
                "POLICY_DENIED",
            ),
            (
                MacpError::InvalidPolicyDefinition,
                "INVALID_POLICY_DEFINITION",
            ),
        ];

        for (error, expected_code) in cases {
            assert_eq!(
                error.error_code(),
                expected_code,
                "error_code() mismatch for {:?}",
                error
            );
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn display_matches_variant_name() {
        assert_eq!(
            MacpError::InvalidMacpVersion.to_string(),
            "InvalidMacpVersion"
        );
        assert_eq!(MacpError::Forbidden.to_string(), "Forbidden");
        assert_eq!(MacpError::TtlExpired.to_string(), "TtlExpired");
        assert_eq!(MacpError::Unauthenticated.to_string(), "Unauthenticated");
        assert_eq!(MacpError::DuplicateMessage.to_string(), "DuplicateMessage");
        assert_eq!(MacpError::PayloadTooLarge.to_string(), "PayloadTooLarge");
        assert_eq!(MacpError::RateLimited.to_string(), "RateLimited");
        assert_eq!(MacpError::StorageFailed.to_string(), "StorageFailed");
        assert_eq!(MacpError::InvalidSessionId.to_string(), "InvalidSessionId");
        assert_eq!(
            MacpError::UnknownPolicyVersion.to_string(),
            "UnknownPolicyVersion"
        );
        assert_eq!(
            MacpError::PolicyDenied {
                reasons: vec!["test".into()]
            }
            .to_string(),
            "PolicyDenied"
        );
    }

    #[test]
    fn every_error_has_a_registry_status() {
        use TransportStatus::*;
        let cases: Vec<(MacpError, TransportStatus, u16)> = vec![
            (MacpError::InvalidMacpVersion, InvalidArgument, 400),
            (MacpError::InvalidEnvelope, InvalidArgument, 400),
            (MacpError::SessionAlreadyExists, AlreadyExists, 409),
            (MacpError::UnknownSession, NotFound, 404),
            (MacpError::SessionNotOpen, FailedPrecondition, 409),
            (MacpError::TtlExpired, FailedPrecondition, 409),
            (MacpError::InvalidTtl, InvalidArgument, 400),
            (MacpError::UnknownMode, InvalidArgument, 400),
            (MacpError::InvalidModeState, InvalidArgument, 400),
            (MacpError::InvalidPayload, InvalidArgument, 400),
            (MacpError::Forbidden, PermissionDenied, 403),
            (MacpError::Unauthenticated, Unauthenticated, 401),
            (MacpError::DuplicateMessage, AlreadyExists, 409),
            (MacpError::PayloadTooLarge, ResourceExhausted, 413),
            (MacpError::RateLimited, ResourceExhausted, 429),
            (MacpError::StorageFailed, Internal, 500),
            (MacpError::InvalidSessionId, InvalidArgument, 400),
            (MacpError::UnknownPolicyVersion, NotFound, 404),
            (
                MacpError::PolicyDenied { reasons: vec![] },
                PermissionDenied,
                403,
            ),
            (MacpError::InvalidPolicyDefinition, InvalidArgument, 400),
        ];
        for (err, grpc, http) in cases {
            assert_eq!(err.transport_status(), grpc, "{err:?}");
            assert_eq!(err.http_status(), http, "{err:?}");
        }
        assert_eq!(TransportStatus::PermissionDenied.grpc_code(), 7);
        assert!(registry_entry("NOPE").is_none());
        let mut codes: Vec<_> = ERROR_CODE_REGISTRY.iter().map(|e| e.code).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(
            codes.len(),
            ERROR_CODE_REGISTRY.len(),
            "duplicate registry row"
        );
    }

    /// The HTTP column must equal the spec's `registries/error-codes.md`
    /// table. Reads the spec checkout named by `MACP_ERROR_REGISTRY_PATH`
    /// (CI: the `SPEC_REV` export); skipped when unset. The gRPC column is
    /// pinned once the spec ships it (S13) — then this test reads it too.
    #[test]
    fn http_column_matches_the_spec_registry() {
        let Ok(path) = std::env::var("MACP_ERROR_REGISTRY_PATH") else {
            return;
        };
        let text = std::fs::read_to_string(&path).expect("MACP_ERROR_REGISTRY_PATH is readable");
        let mut seen = 0;
        for line in text.lines() {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            if cells.len() < 4
                || cells[1].is_empty()
                || !cells[1].chars().all(|c| c.is_ascii_uppercase() || c == '_')
            {
                continue;
            }
            let Ok(http) = cells[3].parse::<u16>() else {
                continue;
            };
            let row = registry_entry(cells[1]).unwrap_or_else(|| panic!("{} missing", cells[1]));
            assert_eq!(row.http, http, "{}", cells[1]);
            if let Some(grpc) = cells.iter().find(|c| {
                c.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_') && registry_grpc_name(c)
            }) {
                assert_eq!(row.grpc.grpc_name(), *grpc, "{}", cells[1]);
            }
            seen += 1;
        }
        assert_eq!(seen, ERROR_CODE_REGISTRY.len(), "every row compared");
    }

    fn registry_grpc_name(cell: &str) -> bool {
        [
            "INVALID_ARGUMENT",
            "NOT_FOUND",
            "ALREADY_EXISTS",
            "PERMISSION_DENIED",
            "RESOURCE_EXHAUSTED",
            "FAILED_PRECONDITION",
            "INTERNAL",
        ]
        .contains(&cell)
    }
}
