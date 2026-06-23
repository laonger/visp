//! TraceContext — lightweight W3C Trace Context data type.
//!
//! This module defines a minimal `TraceContext` value type that carries
//! W3C Trace Context fields (trace_id, span_id, trace_flags, trace_state)
//! through the agent system. It deliberately does **not** depend on
//! `tracing` or `opentelemetry` crates.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur when constructing or parsing a `TraceContext`.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum TraceContextError {
    #[error("trace_id must be exactly 32 hex characters")]
    InvalidTraceIdLength,

    #[error("span_id must be exactly 16 hex characters")]
    InvalidSpanIdLength,

    #[error("invalid W3C traceparent header format")]
    InvalidTraceparent,

    #[error("value contains non-hex characters")]
    InvalidHex,
}

/// Lightweight W3C Trace Context value type.
///
/// Carries trace_id, span_id, trace_flags, and optional trace_state
/// through the agent system without depending on `tracing` or `opentelemetry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceContext {
    /// 32 hex character trace ID (lowercase)
    pub trace_id: String,
    /// 16 hex character span ID (lowercase)
    pub span_id: String,
    /// W3C trace flags (e.g. 0x01 = sampled)
    pub trace_flags: u8,
    /// Optional vendor-specific trace state
    pub trace_state: Option<String>,
}

impl TraceContext {
    /// Create a new `TraceContext`, validating hex length and character set.
    pub fn new(
        trace_id: String,
        span_id: String,
        trace_flags: u8,
        trace_state: Option<String>,
    ) -> Result<Self, TraceContextError> {
        // Validate trace_id length (32 hex chars)
        if trace_id.len() != 32 {
            return Err(TraceContextError::InvalidTraceIdLength);
        }
        // Validate span_id length (16 hex chars)
        if span_id.len() != 16 {
            return Err(TraceContextError::InvalidSpanIdLength);
        }
        // Validate hex characters for both
        if !trace_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(TraceContextError::InvalidHex);
        }
        if !span_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(TraceContextError::InvalidHex);
        }
        // Normalize to lowercase
        let trace_id = trace_id.to_ascii_lowercase();
        let span_id = span_id.to_ascii_lowercase();

        Ok(Self {
            trace_id,
            span_id,
            trace_flags,
            trace_state,
        })
    }

    /// Parse a W3C `traceparent` header value.
    ///
    /// Format: `{version}-{trace_id}-{span_id}-{trace_flags}`
    ///
    /// Only version `00` is supported.
    pub fn from_traceparent(s: &str) -> Result<Self, TraceContextError> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 4 {
            return Err(TraceContextError::InvalidTraceparent);
        }
        let version = parts[0];
        let trace_id = parts[1];
        let span_id = parts[2];
        let flags_str = parts[3];

        // Only support version 00
        if version != "00" {
            return Err(TraceContextError::InvalidTraceparent);
        }

        // Validate flags is 2 hex chars
        if flags_str.len() != 2 || !flags_str.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(TraceContextError::InvalidTraceparent);
        }
        let trace_flags =
            u8::from_str_radix(flags_str, 16).map_err(|_| TraceContextError::InvalidTraceparent)?;

        // Use the existing validation for trace_id and span_id
        // (also checks hex character validity and length)
        Self::new(trace_id.to_string(), span_id.to_string(), trace_flags, None)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── W1-S1-1: basic construction & validation ──────────────────────────

    #[test]
    fn test_trace_context_w3c_format() {
        let trace_id = "0af7651916cd43dd8448eb211c80319c".to_string();
        let span_id = "b7ad6b7169203331".to_string();
        let tc = TraceContext::new(trace_id.clone(), span_id.clone(), 1, None).unwrap();
        assert_eq!(tc.trace_id, trace_id);
        assert_eq!(tc.span_id, span_id);
        assert_eq!(tc.trace_flags, 1);
        assert_eq!(tc.trace_state, None);
        assert_eq!(tc.trace_id.len(), 32);
        assert_eq!(tc.span_id.len(), 16);
    }

    #[test]
    fn test_trace_context_invalid_length_rejected() {
        // trace_id too short (30 hex chars)
        let err = TraceContext::new(
            "0af7651916cd43dd8448eb211c8031".to_string(), // 30 chars
            "b7ad6b7169203331".to_string(),
            0,
            None,
        )
        .unwrap_err();
        assert_eq!(err, TraceContextError::InvalidTraceIdLength);

        // trace_id too long (34 hex chars)
        let err = TraceContext::new(
            "0af7651916cd43dd8448eb211c80319cde".to_string(), // 34 chars
            "b7ad6b7169203331".to_string(),
            0,
            None,
        )
        .unwrap_err();
        assert_eq!(err, TraceContextError::InvalidTraceIdLength);

        // span_id too short (14 hex chars)
        let err = TraceContext::new(
            "0af7651916cd43dd8448eb211c80319c".to_string(),
            "b7ad6b71692033".to_string(), // 14 chars
            0,
            None,
        )
        .unwrap_err();
        assert_eq!(err, TraceContextError::InvalidSpanIdLength);

        // span_id too long (18 hex chars)
        let err = TraceContext::new(
            "0af7651916cd43dd8448eb211c80319c".to_string(),
            "b7ad6b7169203331de".to_string(), // 18 chars
            0,
            None,
        )
        .unwrap_err();
        assert_eq!(err, TraceContextError::InvalidSpanIdLength);
    }

    #[test]
    fn test_trace_context_invalid_hex_rejected() {
        // Non-hex character in trace_id (z is not hex)
        let err = TraceContext::new(
            "zaf7651916cd43dd8448eb211c80319c".to_string(),
            "b7ad6b7169203331".to_string(),
            0,
            None,
        )
        .unwrap_err();
        assert_eq!(err, TraceContextError::InvalidHex);

        // Non-hex character in span_id (g is not hex)
        let err = TraceContext::new(
            "0af7651916cd43dd8448eb211c80319c".to_string(),
            "g7ad6b7169203331".to_string(),
            0,
            None,
        )
        .unwrap_err();
        assert_eq!(err, TraceContextError::InvalidHex);
    }

    // ── Clone + PartialEq + Debug ─────────────────────────────────────────

    #[test]
    fn test_trace_context_clone_eq() {
        let tc = TraceContext::new(
            "0af7651916cd43dd8448eb211c80319c".to_string(),
            "b7ad6b7169203331".to_string(),
            1,
            Some("congo=toto".to_string()),
        )
        .unwrap();
        let cloned = tc.clone();
        assert_eq!(tc, cloned);
        // Debug output contains key fields
        let debug_str = format!("{tc:?}");
        assert!(debug_str.contains("0af7651916cd43dd8448eb211c80319c"));
    }

    // ── Serde roundtrip ───────────────────────────────────────────────────

    #[test]
    fn test_trace_context_serde_roundtrip() {
        let tc = TraceContext::new(
            "0af7651916cd43dd8448eb211c80319c".to_string(),
            "b7ad6b7169203331".to_string(),
            1,
            None,
        )
        .unwrap();
        let json = serde_json::to_string(&tc).unwrap();
        let deser: TraceContext = serde_json::from_str(&json).unwrap();
        assert_eq!(tc, deser);

        // With trace_state
        let tc2 = TraceContext::new(
            "0af7651916cd43dd8448eb211c80319c".to_string(),
            "b7ad6b7169203331".to_string(),
            1,
            Some("congo=toto".to_string()),
        )
        .unwrap();
        let json2 = serde_json::to_string(&tc2).unwrap();
        let deser2: TraceContext = serde_json::from_str(&json2).unwrap();
        assert_eq!(tc2, deser2);
    }

    // ── W3C traceparent header parsing ────────────────────────────────────

    #[test]
    fn test_trace_context_from_w3c_traceparent_header() {
        // Valid traceparent
        let s = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
        let tc = TraceContext::from_traceparent(s).unwrap();
        assert_eq!(tc.trace_id, "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(tc.span_id, "b7ad6b7169203331");
        assert_eq!(tc.trace_flags, 1);
        assert_eq!(tc.trace_state, None);

        // Valid traceparent with flags=00
        let s2 = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-00";
        let tc2 = TraceContext::from_traceparent(s2).unwrap();
        assert_eq!(tc2.trace_flags, 0);
    }

    #[test]
    fn test_trace_context_from_traceparent_invalid() {
        // Wrong number of fields (only 3 parts)
        let err =
            TraceContext::from_traceparent("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331")
                .unwrap_err();
        assert_eq!(err, TraceContextError::InvalidTraceparent);

        // Wrong number of fields (5 parts)
        let err = TraceContext::from_traceparent(
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-extra",
        )
        .unwrap_err();
        assert_eq!(err, TraceContextError::InvalidTraceparent);

        // Version not 00
        let err = TraceContext::from_traceparent(
            "01-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        )
        .unwrap_err();
        assert_eq!(err, TraceContextError::InvalidTraceparent);

        // Bad hex in trace_id (32 chars, invalid hex digit 'x')
        let err = TraceContext::from_traceparent(
            "00-xxxx1916cd43dd8448eb211c80319cAB-b7ad6b7169203331-01",
        )
        .unwrap_err();
        assert_eq!(err, TraceContextError::InvalidHex);

        // Empty string
        let err = TraceContext::from_traceparent("").unwrap_err();
        assert_eq!(err, TraceContextError::InvalidTraceparent);
    }
}
