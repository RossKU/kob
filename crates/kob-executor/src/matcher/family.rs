//! Token families behind one abstraction.
//!
//! The planner is family-agnostic: it works on normalised candidates (quotes, base units, classes) and
//! on the per-transaction token limits a family's program imposes. A family adapter supplies those
//! limits and lowers a [`Plan`](super::planner::Plan) to a transaction with that family's builders.
//!
//! * **KCC-20** (`kob_protocol::build`): slot limits of the pinned program (3/3 reference and P2,
//!   4/5, 8/8 for KOB-issued tokens, 16/16) and the covenants' `MAX_TOK_IN = 8`.
//! * **KRON** (`contracts/adapters/kron`): 4 token inputs / 5 outputs, every output amount at most
//!   1e9 (`kob_protocol::registry::KRON_MAX_OUTPUT_AMOUNT`), no extension commitment. The matcher
//!   plans KRON books with the same planner (the limits below enforced) and lowers them with the KRON
//!   builders through [`KronAdapter`].

use kob_protocol::artifacts::token_template_by_hash;
use kob_protocol::registry::KRON_MAX_OUTPUT_AMOUNT;
use kob_protocol::state::MAX_TOK_IN;
use kob_protocol::tx::BuiltTx;

use super::lower::LowerCtx;
use super::planner::Plan;

pub use kob_protocol::registry::Family;

/// Default family of a listed order (JSON field omitted).
pub fn default_family() -> Family {
    Family::Kcc20
}

/// Per-transaction limits of one token program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenLimits {
    /// Token inputs per transaction (leader included), already capped by the orders' own limit.
    pub max_in: usize,
    /// Token outputs per transaction.
    pub max_out: usize,
    /// Largest amount a single token output may carry (KRON: 1e9).
    pub max_output_amount: Option<i64>,
}

/// KRON token program slots (`contracts/adapters/kron/README.md`).
pub const KRON_SLOTS: (usize, usize) = (4, 5);
/// Token inputs a KRON order accepts in one transaction.
pub const KRON_MAX_TOK_IN: usize = 4;

/// Limits of the token program with template hash `tpl` in `family` (None: not a supported program).
pub fn token_limits(family: Family, tpl: &[u8; 32]) -> Option<TokenLimits> {
    match family {
        Family::Kcc20 => {
            let t = token_template_by_hash(tpl).filter(|t| t.family == Family::Kcc20)?;
            let (i, o) = t.slots;
            Some(TokenLimits { max_in: i.min(MAX_TOK_IN), max_out: o, max_output_amount: None })
        }
        Family::Kron => {
            let t = token_template_by_hash(tpl).filter(|t| t.family == Family::Kron)?;
            Some(TokenLimits {
                max_in: t.slots.0.min(KRON_MAX_TOK_IN),
                max_out: t.slots.1,
                max_output_amount: Some(KRON_MAX_OUTPUT_AMOUNT),
            })
        }
    }
}

/// A lowered plan: the builder request (for logs and dry runs) and the unsigned transaction.
#[derive(Clone, Debug)]
pub struct Lowered {
    /// The family's builder request as JSON (KCC-20: a `kob_protocol::build::Action`).
    pub request: serde_json::Value,
    pub built: BuiltTx,
    /// KAS on the operator's own token inputs (0: the matcher trades no tokens of its own).
    pub operator_token_kas_in: u64,
    /// KAS on the operator's own token output (net tokens it receives).
    pub operator_token_kas_out: u64,
}

/// Lowers plans of one family to unsigned transactions with that family's own builders.
pub trait FamilyAdapter: Send + Sync {
    fn family(&self) -> Family;
    fn lower(&self, plan: &Plan, cx: &LowerCtx) -> Result<Lowered, String>;
}

/// Lowers a plan with the `kob_protocol::build` batch builder. The builders follow the token program of
/// each leg, so one lowering serves both families; what differs is enforced there: the slot limits, the
/// KRON output amount range, address-presence authorisation of key-owned KRON tokens (the operator's
/// funding input is the P2PK input that authorises its own tokens).
fn lower_with(plan: &Plan, cx: &LowerCtx, family: Family) -> Result<Lowered, String> {
    // a global batch may trade both families: the adapter of its first book lowers it (the builders follow every leg's
    // token program); the engine only plans families it has adapters for
    if plan.book.family != family {
        return Err(format!("a plan led by a {} book reached the {} adapter", plan.book.family.as_str(), family.as_str()));
    }
    let batch = super::lower::lower_batch(plan, cx)?;
    let (kin, kout) = super::lower::operator_token_kas(plan, &batch);
    let action = kob_protocol::build::Action::Batch(batch);
    let floor = &cx.budget_floor;
    let budgets = move |role: &str| super::lower::budgets(role).map(|b| floor.get(role).map_or(b, |m| b.max(*m)));
    let built = kob_protocol::build::build_with(&action, &budgets).map_err(|e| e.to_string())?;
    Ok(Lowered {
        request: serde_json::to_value(&action).map_err(|e| e.to_string())?,
        built,
        operator_token_kas_in: kin,
        operator_token_kas_out: kout,
    })
}

/// The KCC-20 adapter (`kob_protocol::build`).
pub struct Kcc20Adapter;

impl FamilyAdapter for Kcc20Adapter {
    fn family(&self) -> Family {
        Family::Kcc20
    }
    fn lower(&self, plan: &Plan, cx: &LowerCtx) -> Result<Lowered, String> {
        lower_with(plan, cx, Family::Kcc20)
    }
}

/// The KRON adapter (`kob_protocol::build`, the KRON order kinds and token programs).
pub struct KronAdapter;

impl FamilyAdapter for KronAdapter {
    fn family(&self) -> Family {
        Family::Kron
    }
    fn lower(&self, plan: &Plan, cx: &LowerCtx) -> Result<Lowered, String> {
        lower_with(plan, cx, Family::Kron)
    }
}

/// Registered family adapters (both families in this build).
pub struct Families {
    adapters: Vec<Box<dyn FamilyAdapter>>,
}

impl Default for Families {
    fn default() -> Self {
        Families { adapters: vec![Box::new(Kcc20Adapter), Box::new(KronAdapter)] }
    }
}

impl Families {
    /// Registers an adapter (replacing one of the same family).
    pub fn register(&mut self, a: Box<dyn FamilyAdapter>) {
        self.adapters.retain(|x| x.family() != a.family());
        self.adapters.push(a);
    }
    pub fn get(&self, f: Family) -> Option<&dyn FamilyAdapter> {
        self.adapters.iter().find(|a| a.family() == f).map(|a| a.as_ref())
    }
    pub fn supports(&self, f: Family) -> bool {
        self.get(f).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kob_protocol::artifacts::{template, TemplateId};

    #[test]
    fn limits_follow_the_programs() {
        let l = |id: TemplateId| token_limits(Family::Kcc20, &template(id).hash).unwrap();
        assert_eq!((l(TemplateId::Kcc20Ref).max_in, l(TemplateId::Kcc20Ref).max_out), (3, 3));
        assert_eq!((l(TemplateId::Kcc20Ref4x5).max_in, l(TemplateId::Kcc20Ref4x5).max_out), (4, 5));
        assert_eq!((l(TemplateId::Kcc20Ref8x8).max_in, l(TemplateId::Kcc20Ref8x8).max_out), (8, 8));
        // 16/16 is capped by the orders' MAX_TOK_IN.
        assert_eq!((l(TemplateId::Kcc20Ref16x16).max_in, l(TemplateId::Kcc20Ref16x16).max_out), (8, 16));
        assert!(token_limits(Family::Kcc20, &template(TemplateId::KobAsk).hash).is_none());
        for id in [TemplateId::KronToken2433, TemplateId::KronToken2732] {
            let k = token_limits(Family::Kron, &kob_protocol::artifacts::token_template(id).hash).unwrap();
            assert_eq!((k.max_in, k.max_out, k.max_output_amount), (4, 5, Some(1_000_000_000)));
            assert!(token_limits(Family::Kcc20, &kob_protocol::artifacts::token_template(id).hash).is_none());
        }
        assert!(token_limits(Family::Kron, &[0; 32]).is_none());
        assert!(Families::default().supports(Family::Kcc20));
        assert!(Families::default().supports(Family::Kron));
    }
}
