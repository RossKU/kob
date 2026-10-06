//! Verifier policy: resource bounds, finality depth and the token allowlist.

use kob_protocol::artifacts::{token_template, token_template_by_hash, TemplateId};
use kob_protocol::registry::{Family, Registry, Status};

use crate::wire::{parse_hash32, Network};

/// Resource bounds (`kaspa-exact-v2.md`, "Resource bounds"). Checked as early as possible; every
/// failure is fail-closed.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Bytes of the `payload.transaction` JSON text.
    pub max_tx_json_bytes: usize,
    pub max_inputs: usize,
    pub max_outputs: usize,
    /// Bytes of one signature script (a KCC-20 leader sigscript is about 3.1 KB, the 16/16 program more).
    pub max_signature_script_bytes: usize,
    /// Bytes of the tx payload.
    pub max_payload_bytes: usize,
    /// Largest acceptable fee in sompi (verifier policy; the payer's own limit is client-side).
    pub max_fee_sompi: u64,
    /// Smallest acceptable KAS amount in sompi. Application policy, not consensus (the binding
    /// defines no universal floor); 0 disables it.
    pub min_amount_sompi: u64,
    /// Smallest KAS value of a merchant token output ("carrier") an offer may quote.
    pub min_carrier_sompi: u64,
    /// Largest KAS value of a merchant token output ("carrier") an offer may quote, and of the payer's own token
    /// change output. The carrier is funded by the payer (a token payment moves the merchant's carrier out of
    /// the payer's KAS), so an offer quoting one base unit with a 500 KAS carrier would drain the payer: the
    /// payer's preflight and both builders refuse anything above this bound. Must be at least `min_carrier_sompi`.
    pub max_carrier_sompi: u64,
    /// Bytes of a facilitator request body.
    pub max_body_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_tx_json_bytes: 256 * 1024,
            max_inputs: 32,
            max_outputs: 16,
            max_signature_script_bytes: 32 * 1024,
            max_payload_bytes: 512,
            max_fee_sompi: 50_000_000,
            min_amount_sompi: 0,
            min_carrier_sompi: 100_000_000,
            max_carrier_sompi: 200_000_000,
            max_body_bytes: 1024 * 1024,
        }
    }
}

/// Custody class of a token (the founder's point on forum topic 15 post #20: acceptance is not
/// unconditional custody when an issuer can freeze or seize).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Custody {
    /// The token program was semantically reviewed to have no admin, freeze, seize, mint or burn path
    /// that can touch a holder's balance: once the payment is final, the merchant owns the units.
    Unconditional,
    /// The program (or its issuer through an extension) can reach a holder's balance. Accepting it
    /// is an explicit merchant risk and is never presented as final custody.
    IssuerControlled,
}

impl Custody {
    pub fn as_str(self) -> &'static str {
        match self {
            Custody::Unconditional => "unconditional",
            Custody::IssuerControlled => "issuer-controlled",
        }
    }
    pub fn parse(s: &str) -> Option<Custody> {
        match s {
            "unconditional" => Some(Custody::Unconditional),
            "issuer-controlled" => Some(Custody::IssuerControlled),
            _ => None,
        }
    }
}

/// One token the facilitator accepts as payment.
///
/// Two roles: a **merchant asset** (the `kcc20` profile, and the `kcc20` gain of a swap-and-pay offer)
/// needs [`Family::Kcc20`]; a swap-and-pay **pay asset** may be of either family (a KRON token, a
/// KaspaCom-template KCC-20 token, ...). Nothing else distinguishes tokens: the program is any pinned
/// token program of the family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowedToken {
    /// KIP-20 covenant id (the x402 `asset`).
    pub covenant_id: [u8; 32],
    /// The pinned token program (its template hash is what the verifier requires of every token input).
    pub program: TemplateId,
    /// Token family (must be the program's family; [`Family::Kcc20`] by default, see [`AllowedToken::new`]).
    pub family: Family,
    /// Extension commitment; all zero for a KRON token (it has no such field).
    pub extension_commitment: [u8; 32],
    pub custody: Custody,
    pub ticker: String,
    pub decimals: u8,
}

impl AllowedToken {
    /// A token on `program`; the family is the program's.
    pub fn new(
        covenant_id: [u8; 32],
        program: TemplateId,
        extension_commitment: [u8; 32],
        custody: Custody,
        ticker: impl Into<String>,
        decimals: u8,
    ) -> Self {
        AllowedToken { covenant_id, program, family: program.family(), extension_commitment, custody, ticker: ticker.into(), decimals }
    }

    /// Template hash of the pinned program (either family).
    pub fn template_hash(&self) -> [u8; 32] {
        token_template(self.program).hash
    }

    /// True if the token may be a merchant asset of the `kcc20` profile (KCC-20 family only).
    pub fn is_merchant_capable(&self) -> bool {
        self.family == Family::Kcc20
    }
}

/// Tokens acceptable as payment, keyed by covenant id (one covenant id = one token).
#[derive(Clone, Debug, Default)]
pub struct TokenAllowlist {
    tokens: Vec<AllowedToken>,
}

impl TokenAllowlist {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a token; a covenant id can be listed once.
    pub fn insert(&mut self, t: AllowedToken) -> Result<(), String> {
        if !t.program.is_token() {
            return Err(format!("{} is not a token program", t.program.name()));
        }
        if t.family != t.program.family() {
            return Err(format!("{} is a {} program, not {}", t.program.name(), t.program.family().as_str(), t.family.as_str()));
        }
        if t.family == Family::Kron && t.extension_commitment != [0; 32] {
            return Err("a KRON token has no extension commitment (it must be all zero)".into());
        }
        if self.tokens.iter().any(|x| x.covenant_id == t.covenant_id) {
            return Err("token already listed".into());
        }
        self.tokens.push(t);
        Ok(())
    }

    pub fn find(&self, covenant_id: &[u8; 32]) -> Option<&AllowedToken> {
        self.tokens.iter().find(|t| &t.covenant_id == covenant_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &AllowedToken> {
        self.tokens.iter()
    }

    /// The default allowlist of a registry: every `listed`, `verified` token (either family) whose
    /// program is pinned in this build and on the registry's strict list (`reviewed`). The custody class
    /// is the program's: [`Custody::Unconditional`] only when its registry `capabilities` are empty (the profile's
    /// `unconditional` means no admin, freeze, seize, mint or burn path: freeze, seize and blacklist reach balances, mint authority,
    /// public mint and burn dilute or destroy supply), else [`Custody::IssuerControlled`].
    /// KRON tokens (no extension commitment: all zero) are only usable as a swap-and-pay pay asset,
    /// see [`AllowedToken`]. Tokens on unreviewed programs are not accepted as x402 payment.
    pub fn from_registry(reg: &Registry) -> Self {
        let mut list = TokenAllowlist::new();
        for t in reg.tokens.iter().filter(|t| t.status == Status::Listed && t.verified) {
            let Some(tpl) = reg.template(&t.template_id) else { continue };
            if !tpl.is_strict() || tpl.family != t.family {
                continue;
            }
            let (Some(cov), Some(hash)) = (parse_hash32(&t.covenant_id), parse_hash32(&tpl.template_hash)) else { continue };
            let ext = match t.family {
                Family::Kcc20 => match t.extension_commitment.as_deref().map(parse_hash32) {
                    Some(Some(e)) => e,
                    _ => continue,
                },
                Family::Kron => [0; 32],
            };
            let Some(program) = token_template_by_hash(&hash).filter(|x| x.family == t.family).map(|x| x.id) else { continue };
            let _ = list.insert(AllowedToken {
                covenant_id: cov,
                program,
                family: t.family,
                extension_commitment: ext,
                custody: if tpl.capabilities.is_empty() { Custody::Unconditional } else { Custody::IssuerControlled },
                ticker: t.ticker.clone(),
                decimals: t.decimals,
            });
        }
        list
    }
}

/// Everything a verifier needs besides the chain and the clock.
#[derive(Clone, Debug)]
pub struct Policy {
    pub network: Network,
    pub limits: Limits,
    /// DAA depth below the virtual tip that `confirmed` finality requires (default 100, about 10 s).
    pub confirmations_daa: u64,
    /// Tokens accepted by the `kcc20` profile (KCC-20 family) and as swap-and-pay payer assets (either
    /// family) / merchant assets (KCC-20 family).
    pub tokens: TokenAllowlist,
    /// Accept [`Custody::IssuerControlled`] tokens at all (default off).
    pub allow_issuer_controlled: bool,
    /// The binding requires the `payment-identifier` extension for exact; a deployment that does
    /// not want it (a test facilitator) can turn the requirement off.
    pub require_payment_identifier: bool,
    /// Weakest finality this verifier settles on (default `accepted`; the offer may ask for more).
    pub min_finality: crate::wire::Finality,
    /// Serve swap-and-pay (`extra.route`).
    pub swap_enabled: bool,
}

impl Policy {
    /// Defaults for a network: 100 DAA confirmation depth, no tokens, swap on, payment identifier required.
    pub fn new(network: Network) -> Self {
        Policy {
            network,
            limits: Limits::default(),
            confirmations_daa: 100,
            tokens: TokenAllowlist::new(),
            allow_issuer_controlled: false,
            require_payment_identifier: true,
            min_finality: crate::wire::Finality::Accepted,
            swap_enabled: true,
        }
    }
}
