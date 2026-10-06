//! Errors: the public x402 wire reasons and the local diagnostic codes behind them.
//!
//! The elldeeone binding (`spec/errors.md`) exposes only a closed set of public reasons on the wire
//! (`invalid_payload`, `invalid_transaction_state`, ...). Every failure this crate raises carries one
//! of those reasons plus a finer local [`Diag`] code that is logged, tested and returned in
//! `extensions.kaspa.diagnostic` of a failed response, never used as the wire reason itself.

use std::fmt;

use serde_json::Value;

/// The public x402 error reasons of the binding (`spec/errors.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    InvalidX402Version,
    InvalidScheme,
    InvalidNetwork,
    InvalidPaymentRequirements,
    InvalidPayload,
    InvalidTransactionState,
    UnsupportedScheme,
    UnexpectedSettleError,
}

impl Reason {
    /// Wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::InvalidX402Version => "invalid_x402_version",
            Reason::InvalidScheme => "invalid_scheme",
            Reason::InvalidNetwork => "invalid_network",
            Reason::InvalidPaymentRequirements => "invalid_payment_requirements",
            Reason::InvalidPayload => "invalid_payload",
            Reason::InvalidTransactionState => "invalid_transaction_state",
            Reason::UnsupportedScheme => "unsupported_scheme",
            Reason::UnexpectedSettleError => "unexpected_settle_error",
        }
    }
}

/// Local diagnostic codes. The first block is the binding's own list (`kaspa-exact-v2.md`,
/// `errors.md`); the second block is what the KOB token profile and swap-and-pay add.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Diag {
    // binding diagnostics
    InvalidKaspaX402Amount,
    InvalidKaspaX402Binding,
    InvalidKaspaX402Payload,
    InvalidKaspaX402Asset,
    InvalidKaspaX402Accepted,
    InvalidKaspaPaymentIdentifier,
    MissingKaspaPaymentIdentifier,
    KaspaPaymentIdentifierConflict,
    UnsupportedKaspaExactProfile,
    InvalidKaspaExactTransaction,
    InvalidKaspaExactTransactionId,
    InvalidKaspaExactPaymentOutput,
    InvalidKaspaExactSignature,
    InvalidKaspaExactUtxo,
    InvalidKaspaExactFee,
    InvalidKaspaExactMass,
    InvalidKaspaExactReplay,
    InvalidKaspaExactFinality,
    // authorization / expiry (interop-v1 `expiry.cases`)
    ExpiredAuthorization,
    AuthorizationExceedsMaxTimeout,
    InvalidAuthorization,
    // KOB token profile
    TokenNotAllowlisted,
    TokenTemplateMismatch,
    TokenBorrowEnabled,
    TokenOwnerScheme,
    TokenConservation,
    TokenCustodyPolicy,
    CarrierMismatch,
    Underpayment,
    Overpayment,
    // KOB swap-and-pay
    RouteUnsupported,
    RouteAliasing,
    UnknownOrderTemplate,
    OrderConflict,
    OrderNotSpendable,
    PayAssetNotAccepted,
    // KOB intent-based swap-and-pay and invoices
    IntentNotExecutable,
    IntentExpired,
    IntentSpent,
    InvoiceUnknown,
    InvoiceExpired,
    InvoicePaid,
    InvoicePending,
    InvalidInvoice,
    // runtime
    Replay,
    Expired,
    RateLimited,
    Unauthorized,
    NodeUnavailable,
    SettlementPending,
    Internal,
}

impl Diag {
    /// Diagnostic spelling (snake case).
    pub fn as_str(self) -> &'static str {
        use Diag::*;
        match self {
            InvalidKaspaX402Amount => "invalid_kaspa_x402_amount",
            InvalidKaspaX402Binding => "invalid_kaspa_x402_binding",
            InvalidKaspaX402Payload => "invalid_kaspa_x402_payload",
            InvalidKaspaX402Asset => "invalid_kaspa_x402_asset",
            InvalidKaspaX402Accepted => "invalid_kaspa_x402_accepted",
            InvalidKaspaPaymentIdentifier => "invalid_kaspa_payment_identifier",
            MissingKaspaPaymentIdentifier => "missing_kaspa_payment_identifier",
            KaspaPaymentIdentifierConflict => "kaspa_payment_identifier_conflict",
            UnsupportedKaspaExactProfile => "unsupported_kaspa_exact_profile",
            InvalidKaspaExactTransaction => "invalid_kaspa_exact_transaction",
            InvalidKaspaExactTransactionId => "invalid_kaspa_exact_transaction_id",
            InvalidKaspaExactPaymentOutput => "invalid_kaspa_exact_payment_output",
            InvalidKaspaExactSignature => "invalid_kaspa_exact_signature",
            InvalidKaspaExactUtxo => "invalid_kaspa_exact_utxo",
            InvalidKaspaExactFee => "invalid_kaspa_exact_fee",
            InvalidKaspaExactMass => "invalid_kaspa_exact_mass",
            InvalidKaspaExactReplay => "invalid_kaspa_exact_replay",
            InvalidKaspaExactFinality => "invalid_kaspa_exact_finality",
            ExpiredAuthorization => "expired_authorization",
            AuthorizationExceedsMaxTimeout => "authorization_exceeds_max_timeout",
            InvalidAuthorization => "invalid_authorization",
            TokenNotAllowlisted => "token_not_allowlisted",
            TokenTemplateMismatch => "token_template_mismatch",
            TokenBorrowEnabled => "token_borrow_enabled",
            TokenOwnerScheme => "token_owner_scheme",
            TokenConservation => "token_conservation",
            TokenCustodyPolicy => "token_custody_policy",
            CarrierMismatch => "carrier_mismatch",
            Underpayment => "underpayment",
            Overpayment => "overpayment",
            RouteUnsupported => "route_unsupported",
            RouteAliasing => "route_aliasing",
            UnknownOrderTemplate => "unknown_order_template",
            OrderConflict => "order_conflict",
            OrderNotSpendable => "order_not_spendable",
            PayAssetNotAccepted => "pay_asset_not_accepted",
            IntentNotExecutable => "intent_not_executable",
            IntentExpired => "intent_expired",
            IntentSpent => "intent_spent",
            InvoiceUnknown => "invoice_unknown",
            InvoiceExpired => "invoice_expired",
            InvoicePaid => "invoice_paid",
            InvoicePending => "invoice_pending",
            InvalidInvoice => "invalid_invoice",
            Replay => "replay",
            Expired => "expired",
            RateLimited => "rate_limited",
            Unauthorized => "unauthorized",
            NodeUnavailable => "node_unavailable",
            SettlementPending => "settlement_pending",
            Internal => "internal",
        }
    }
}

/// A verification / settlement failure.
#[derive(Clone, Debug)]
pub struct X402Error {
    /// Public reason for the wire.
    pub reason: Reason,
    /// Local diagnostic.
    pub diag: Diag,
    /// Human-readable detail (never security relevant).
    pub message: String,
    /// True when the payer can retry (with a fresh quote and signature for `order_conflict`).
    pub retryable: bool,
    /// Optional structured detail returned in `extensions.kaspa.details` (for example the spent order outpoints).
    pub details: Option<Value>,
}

impl X402Error {
    /// A failure with the given reason and diagnostic.
    pub fn new(reason: Reason, diag: Diag, message: impl Into<String>) -> Self {
        X402Error { reason, diag, message: message.into(), retryable: false, details: None }
    }
    /// `invalid_payload`.
    pub fn payload(diag: Diag, message: impl Into<String>) -> Self {
        Self::new(Reason::InvalidPayload, diag, message)
    }
    /// `invalid_payment_requirements`.
    pub fn requirements(diag: Diag, message: impl Into<String>) -> Self {
        Self::new(Reason::InvalidPaymentRequirements, diag, message)
    }
    /// `invalid_transaction_state` (replay, stale state, conflict, on-chain mismatch).
    pub fn state(diag: Diag, message: impl Into<String>) -> Self {
        Self::new(Reason::InvalidTransactionState, diag, message)
    }
    /// Marks the failure retryable.
    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }
    /// Attaches structured details.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl fmt::Display for X402Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.reason.as_str(), self.diag.as_str(), self.message)
    }
}

impl std::error::Error for X402Error {}

impl From<kob_protocol::Error> for X402Error {
    fn from(e: kob_protocol::Error) -> Self {
        X402Error::payload(Diag::InvalidKaspaExactTransaction, e.to_string())
    }
}

/// Result alias.
pub type Result<T> = std::result::Result<T, X402Error>;

/// Shorthand: early-return an `invalid_payload` failure.
#[macro_export]
macro_rules! bail_payload {
    ($diag:expr, $($arg:tt)*) => {
        return Err($crate::error::X402Error::payload($diag, format!($($arg)*)))
    };
}
