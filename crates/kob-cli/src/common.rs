//! Shared pieces of the order commands: templates (pinned or retired), addresses, node lookups, the maker's funding and the
//! sign / engine-validate / write / submit pipeline.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kaspa_addresses::Prefix;
use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_protocol::artifacts::{template, template_by_hash, TemplateId};
use kob_protocol::build::{pair_programs, token_program};
use kob_protocol::issue::rpc_transaction_json;
use kob_protocol::json::to_hex;
use kob_protocol::retired::lot::LotState;
use kob_protocol::retired::nolot::CrossState as NoLotCross;
use kob_protocol::retired::{self, Retired, RetiredState};
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::AnyState;
use kob_protocol::tx::{finalize, pubkey_of, sign_locally, BuiltTx, FinalizeOptions, KeyUtxo, SignedTx, Utxo};
use kob_protocol::verify::validate_signed;
use kob_protocol::Family;
use serde_json::{json, Value};

use crate::wrpc::{self, NodeUtxo};

/// Timeout of one node request.
pub const NODE_TIMEOUT: Duration = Duration::from_secs(60);

/// Most funding UTXOs tried (smallest first) before giving up.
const FUNDING_TRIES: usize = 16;

/// Address prefix of a network name (`mainnet`, `testnet-10`, `simnet`, `devnet`).
pub fn prefix_for_network(network: &str) -> Result<Prefix, String> {
    if network.starts_with("mainnet") {
        Ok(Prefix::Mainnet)
    } else if network.starts_with("testnet") {
        Ok(Prefix::Testnet)
    } else if network.starts_with("simnet") {
        Ok(Prefix::Simnet)
    } else if network.starts_with("devnet") {
        Ok(Prefix::Devnet)
    } else {
        Err(format!("unknown network `{network}` (mainnet, testnet-10, simnet, devnet)"))
    }
}

/// The address of a script public key.
pub fn address_of(spk: &ScriptPublicKey, network: &str) -> Result<String, String> {
    kaspa_txscript::extract_script_pub_key_address(spk, prefix_for_network(network)?)
        .map(|a| a.to_string())
        .map_err(|e| format!("script has no address: {e}"))
}

/// `version (u16 BE) ‖ script`, the node's form.
pub fn spk_bytes(spk: &ScriptPublicKey) -> Vec<u8> {
    let mut b = spk.version().to_be_bytes().to_vec();
    b.extend_from_slice(spk.script());
    b
}

/// The state of an order: of a template pinned by this build, or in the lot layout of a retired template (an older
/// contract version: the maker's cancel only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderState {
    /// A pinned template's state (amounts in base units, prices per whole token).
    Current(AnyState),
    /// A retired template's state in its lot layout (the v2.6 and earlier layouts), with the family of its token (a cross
    /// limit: of token A).
    Lot(LotState, Family),
    /// The retired protocol v3 cross limit (no lots: base units, token A of either family by `aFamily`).
    NoLot(NoLotCross),
}

impl OrderState {
    /// Whether this is the state of a retired (older contract version) template.
    pub fn is_retired(&self) -> bool {
        matches!(self, OrderState::Lot(..) | OrderState::NoLot(_))
    }

    /// Name of the kind in its family as it was pinned (`KobAsk`, `KobAskKron`, ...).
    pub fn kind_name(&self) -> String {
        match self {
            OrderState::Current(s) => s.template_id().name().to_string(),
            OrderState::Lot(l, fam) => fam.kind_name(l.kind_name()),
            OrderState::NoLot(_) => "KobCross".to_string(),
        }
    }

    /// Maker key.
    pub fn maker(&self) -> [u8; 32] {
        match self {
            OrderState::Current(s) => s.maker(),
            OrderState::Lot(l, _) => l.maker(),
            OrderState::NoLot(x) => x.maker,
        }
    }

    /// The order's token (a pair order and a retired cross limit: token A) covenant id.
    pub fn token_cov_id(&self) -> [u8; 32] {
        match self {
            OrderState::Current(s) => s.token_cov_id(),
            OrderState::Lot(l, _) => l.token().0,
            OrderState::NoLot(x) => x.token_cov_id,
        }
    }

    /// The order's token program template hash.
    pub fn token_tpl_hash(&self) -> Option<[u8; 32]> {
        match self {
            OrderState::Current(s) => s.token_tpl_hash(),
            OrderState::Lot(l, _) => Some(l.token().1),
            OrderState::NoLot(x) => Some(x.token_tpl_hash),
        }
    }

    /// The exact custody amount (base units) of a token-holding order (`None`: the order holds no tokens). A retired
    /// order's custody is what its lot layout says (`lotsLeft x lotUnits x unit`).
    pub fn custody_amount(&self) -> Option<i64> {
        match self {
            OrderState::Current(s) => s.custody_amount(),
            OrderState::Lot(l, _) => l.custody_amount(),
            OrderState::NoLot(x) => RetiredState::NoLotCross(x.clone()).custody_amount(),
        }
    }

    /// Base units still open: `amountLeft` of a current order, the custody amount of a retired token-holding order;
    /// `None` for a bid (its quantity is its escrow, see [`OrderState::amount_left_in`]).
    pub fn amount_left(&self) -> Option<i64> {
        match self {
            OrderState::Current(s) => s.amount_left(),
            OrderState::Lot(l, _) => l.custody_amount(),
            OrderState::NoLot(x) => RetiredState::NoLotCross(x.clone()).custody_amount(),
        }
    }

    /// Like [`OrderState::amount_left`], and for a current bid its remaining buying power in an escrow of `value` sompi.
    pub fn amount_left_in(&self, value: u64) -> Option<i64> {
        match self {
            OrderState::Current(AnyState::KobBid(b) | AnyState::KobBidKron(b)) => {
                Some(b.buying_power(i64::try_from(value).unwrap_or(i64::MAX)))
            }
            other => other.amount_left(),
        }
    }

    /// Base units per whole token (`scale`) of a current order.
    pub fn scale(&self) -> Option<i64> {
        match self {
            OrderState::Current(s) => Some(s.scale()),
            OrderState::Lot(..) => None,
            OrderState::NoLot(x) => Some(x.scale),
        }
    }

    /// Smallest fill (`minFill`, base units) of a current order.
    pub fn min_fill(&self) -> Option<i64> {
        match self {
            OrderState::Current(s) => Some(s.min_fill()),
            OrderState::Lot(..) => None,
            OrderState::NoLot(x) => Some(x.min_fill),
        }
    }

    /// The second token of an order and its program: a pair order's quote token B, a retired cross limit's token B
    /// (`None` for the KAS kinds).
    pub fn b_token(&self) -> Result<Option<([u8; 32], TemplateId)>, String> {
        match self {
            OrderState::Current(s) if s.is_pair() => pair_programs(s).map(|(_, b)| Some(b)).map_err(|e| e.to_string()),
            OrderState::Current(_) => Ok(None),
            OrderState::Lot(l, _) => match l.as_cross() {
                Some(x) => {
                    let fam = l.b_family().ok_or_else(|| "cross limit: bFamily must be 1 (KCC-20) or 2 (KRON)".to_string())?;
                    let program = token_program(&x.b_tpl_hash, x.b_prefix_len, x.b_suffix_len).map_err(|e| e.to_string())?;
                    if program.family() != fam {
                        return Err(format!("cross limit: token B program {} is not of the bFamily {fam:?}", program.name()));
                    }
                    Ok(Some((x.b_cov_id, program)))
                }
                None => Ok(None),
            },
            OrderState::NoLot(x) => {
                let fam = kob_protocol::state::family_of_code(x.b_family)
                    .ok_or_else(|| "cross limit: bFamily must be 1 (KCC-20) or 2 (KRON)".to_string())?;
                let program = token_program(&x.b_tpl_hash, x.b_prefix_len, x.b_suffix_len).map_err(|e| e.to_string())?;
                if program.family() != fam {
                    return Err(format!("cross limit: token B program {} is not of the bFamily {fam:?}", program.name()));
                }
                Ok(Some((x.b_cov_id, program)))
            }
        }
    }
}

/// The template of an order: pinned by this build, or retired (spend-only: the maker's cancel).
#[derive(Clone, Copy)]
pub enum Tpl {
    /// A pinned order template.
    Pinned(TemplateId),
    /// A retired order template.
    Retired(&'static Retired),
}

impl std::fmt::Debug for Tpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.label(), to_hex(&self.hash()))
    }
}

impl Tpl {
    /// The order template with this hash (`None`: neither pinned nor retired, or not an order template).
    pub fn resolve(hash: &[u8; 32]) -> Option<Tpl> {
        if let Some(t) = template_by_hash(hash).filter(|t| t.id.kind_code().is_some()) {
            return Some(Tpl::Pinned(t.id));
        }
        retired::by_hash(hash).map(Tpl::Retired)
    }

    /// `pinned` or `retired`.
    pub fn label(&self) -> &'static str {
        match self {
            Tpl::Pinned(_) => "pinned",
            Tpl::Retired(_) => "retired",
        }
    }

    /// Template hash.
    pub fn hash(&self) -> [u8; 32] {
        match self {
            Tpl::Pinned(id) => template(*id).hash,
            Tpl::Retired(r) => r.template.hash,
        }
    }

    /// Decode a state span (canonical encodings only): a pinned template's span into a current state, a retired
    /// template's into its lot layout.
    pub fn decode(&self, span: &[u8]) -> Result<OrderState, String> {
        match self {
            Tpl::Pinned(id) => {
                let s = AnyState::decode(*id, span).map_err(|e| e.to_string())?;
                if s.try_encode().map_err(|e| e.to_string())? != span {
                    return Err("state is not canonical for its template".into());
                }
                Ok(OrderState::Current(s))
            }
            Tpl::Retired(r) => match retired::decode_any(r, span).map_err(|e| e.to_string())? {
                RetiredState::Lot(l) => Ok(OrderState::Lot(l, r.family)),
                RetiredState::NoLotCross(x) => Ok(OrderState::NoLot(x)),
            },
        }
    }

    /// The state span of a decoded state under this template.
    pub fn encode(&self, s: &OrderState) -> Result<Vec<u8>, String> {
        match (self, s) {
            (Tpl::Pinned(id), OrderState::Current(s)) => {
                if s.template_id() != *id {
                    return Err(format!("a {} state is not a {} state", s.template_id().name(), id.name()));
                }
                s.try_encode().map_err(|e| e.to_string())
            }
            (Tpl::Retired(r), OrderState::Lot(l, _)) => retired::encode(r, l).map_err(|e| e.to_string()),
            (Tpl::Retired(r), OrderState::NoLot(x)) => {
                retired::encode_any(r, &RetiredState::NoLotCross(x.clone())).map_err(|e| e.to_string())
            }
            (Tpl::Pinned(id), OrderState::Lot(..) | OrderState::NoLot(_)) => {
                Err(format!("a retired (lot layout) state is not a state of the pinned {} template", id.name()))
            }
            (Tpl::Retired(_), OrderState::Current(_)) => Err("a current state is not a state of a retired template".into()),
        }
    }

    /// The order's P2SH script public key for a state.
    pub fn spk(&self, s: &OrderState) -> Result<ScriptPublicKey, String> {
        let span = self.encode(s)?;
        Ok(match self {
            Tpl::Pinned(id) => template(*id).spk(&span),
            Tpl::Retired(r) => r.template.spk(&span),
        })
    }
}

/// The same state with another `amountLeft` (base units; `None` for a bid, whose quantity is its escrow, and for a
/// retired state, which counts lots: see [`with_lots_left`]).
pub fn with_amount_left(s: &OrderState, n: i64) -> Option<OrderState> {
    if let OrderState::NoLot(x) = s {
        return Some(OrderState::NoLot(NoLotCross { amount_left: n, ..x.clone() }));
    }
    let OrderState::Current(s) = s else { return None };
    let mut s = s.clone();
    match &mut s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.amount_left = n,
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => x.amount_left = n,
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => x.amount_left = n,
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => x.amount_left = n,
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => x.amount_left = n,
        // a pair order: an ask (and a sell-first entry) holds exactly its amount left; a bid's escrow is not guessed
        AnyState::KobPair(x) => {
            if x.is_ask() {
                x.custody = n;
            }
            x.amount_left = n;
        }
        AnyState::KobCondPair(x) => {
            if x.is_ask() {
                x.custody = n;
            }
            x.amount_left = n;
        }
        AnyState::KobIfdPair(x) => x.amount_left = n,
        AnyState::KobBid(_) | AnyState::KobBidKron(_) => return None,
    }
    Some(OrderState::Current(s))
}

/// A retired order's `lotsLeft` (the retired v2.6 lot layouts count lots; `None` for a bid, which has none, and for a
/// current state).
pub fn lots_left(s: &OrderState) -> Option<i64> {
    let OrderState::Lot(l, _) = s else { return None };
    Some(match l {
        LotState::KobAsk(x) => x.lots_left,
        LotState::KobCondAsk(x) => x.lots_left,
        LotState::KobCondBid(x) => x.lots_left,
        LotState::KobIfdBid(x) => x.lots_left,
        LotState::KobIfdAsk(x) => x.lots_left,
        LotState::KobCross(x) => x.lots_left,
        LotState::KobBid(_) => return None,
    })
}

/// The same retired state with another `lotsLeft` (`None` for a bid or a current state).
pub fn with_lots_left(s: &OrderState, n: i64) -> Option<OrderState> {
    let OrderState::Lot(l, fam) = s else { return None };
    let mut l = l.clone();
    match &mut l {
        LotState::KobAsk(x) => x.lots_left = n,
        LotState::KobCondAsk(x) => x.lots_left = n,
        LotState::KobCondBid(x) => x.lots_left = n,
        LotState::KobIfdBid(x) => x.lots_left = n,
        LotState::KobIfdAsk(x) => x.lots_left = n,
        LotState::KobCross(x) => x.lots_left = n,
        LotState::KobBid(_) => return None,
    }
    Some(OrderState::Lot(l, *fam))
}

/// The base units one retired lot holds (`lotUnits x unit`; `None` for a bid, a current state or an overflow).
pub fn lot_size(s: &OrderState) -> Option<i64> {
    let OrderState::Lot(l, _) = s else { return None };
    let (units, unit) = match l {
        LotState::KobAsk(x) => (x.lot_units, x.unit),
        LotState::KobCondAsk(x) => (x.lot_units, x.unit),
        LotState::KobCondBid(x) => (x.lot_units, x.unit),
        LotState::KobIfdBid(x) => (x.lot_units, x.unit),
        LotState::KobIfdAsk(x) => (x.lot_units, x.unit),
        LotState::KobCross(x) => (x.lot_units, x.unit),
        LotState::KobBid(_) => return None,
    };
    units.checked_mul(unit).filter(|s| *s > 0)
}

/// Whether a current kind's script changes only in `amountLeft` (or never): its states differ in that one 8-byte window
/// and no other field needs guessing. The search over amounts is still NOT exhaustive for such a kind: any amount at
/// least `minFill` may have been filled.
pub fn amount_is_the_only_mutable_field(id: TemplateId) -> bool {
    kob_protocol::state::mutable_windows(id).iter().all(|(name, ..)| *name == "amountLeft")
}

/// Whether a state's script never changes after placement, so one candidate is the whole search (a bid: its quantity is
/// its escrow; a retired bid likewise).
pub fn script_is_fixed(s: &OrderState) -> bool {
    match s {
        OrderState::Current(a) => kob_protocol::state::mutable_windows(a.template_id()).is_empty(),
        OrderState::Lot(l, _) => matches!(l, LotState::KobBid(_)),
        // the v3 cross limit moves its amountLeft
        OrderState::NoLot(_) => false,
    }
}

/// Whether a retired kind's script changes only in `lotsLeft` (asks, bids and cross limits): the search over every
/// `lotsLeft` is then exhaustive. The other retired kinds also move a stop, an armed band or repeat fields.
pub fn lots_is_the_only_mutable_field(s: &OrderState) -> bool {
    matches!(s, OrderState::Lot(LotState::KobAsk(_) | LotState::KobBid(_) | LotState::KobCross(_), _))
}

/// A token amount in base units: a plain integer is base units; a decimal such as `1.5` is whole tokens and needs the
/// token's `scale` (`10^decimals`, base units per whole token) and at most `decimals` fractional digits. Positive only.
pub fn parse_amount(s: &str, scale: Option<i64>) -> Result<i64, String> {
    let bad = || format!("`{s}` is not an amount (base units, or a decimal amount of whole tokens)");
    if s.is_empty() || s.matches('.').count() > 1 || !s.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(bad());
    }
    let n: i128 = match s.split_once('.') {
        None => s.parse::<i128>().map_err(|_| bad())?,
        Some((int, frac)) => {
            if int.is_empty() && frac.is_empty() {
                return Err(bad());
            }
            let scale = scale.filter(|x| *x > 0).ok_or_else(|| {
                format!("`{s}`: a decimal amount needs the token's decimals, which are not known here: give base units")
            })?;
            let decimals = scale.to_string().len() - 1;
            if 10i128.pow(decimals as u32) != scale as i128 {
                return Err(format!("`{s}`: the scale {scale} is not a power of ten: give base units"));
            }
            if frac.len() > decimals {
                return Err(format!("`{s}` has more than the token's {decimals} decimals"));
            }
            let whole: i128 = if int.is_empty() { 0 } else { int.parse().map_err(|_| bad())? };
            let part: i128 = if frac.is_empty() { 0 } else { frac.parse().map_err(|_| bad())? };
            let part = part * 10i128.pow((decimals - frac.len()) as u32);
            whole.checked_mul(scale as i128).and_then(|w| w.checked_add(part)).ok_or_else(bad)?
        }
    };
    let n = i64::try_from(n).map_err(|_| format!("`{s}` is out of range"))?;
    if n <= 0 {
        return Err(format!("`{s}` must be positive"));
    }
    Ok(n)
}

/// A [`Utxo`] from a node entry.
pub fn utxo_of(u: &NodeUtxo) -> Utxo {
    Utxo { transaction_id: u.txid, index: u.index, amount: u.amount, block_daa_score: u.daa, covenant_id: u.covenant_id }
}

/// `txid:index`.
pub fn outpoint_str(txid: &[u8; 32], index: u32) -> String {
    format!("{}:{index}", to_hex(txid))
}

/// The node's unspent outputs at a set of scripts: script bytes -> entries.
pub fn lookup(node: &str, network: &str, spks: &[ScriptPublicKey]) -> Result<BTreeMap<Vec<u8>, Vec<NodeUtxo>>, String> {
    let mut addrs: BTreeSet<String> = BTreeSet::new();
    let mut wanted: BTreeSet<Vec<u8>> = BTreeSet::new();
    for spk in spks {
        addrs.insert(address_of(spk, network)?);
        wanted.insert(spk_bytes(spk));
    }
    let addrs: Vec<String> = addrs.into_iter().collect();
    let entries = wrpc::utxos_by_addresses(node, &addrs, NODE_TIMEOUT).map_err(|e| format!("node {node}: {e}"))?;
    let mut out: BTreeMap<Vec<u8>, Vec<NodeUtxo>> = BTreeMap::new();
    for e in entries {
        // keyed by the script the node reports: an entry counts only for the exact script it carries
        if wanted.contains(&e.spk) {
            let v = out.entry(e.spk.clone()).or_default();
            if !v.iter().any(|x| x.txid == e.txid && x.index == e.index) {
                v.push(e);
            }
        }
    }
    Ok(out)
}

/// The maker's plain KAS UTXOs (P2PK, no covenant id), smallest first.
pub fn maker_funding(node: &str, network: &str, maker: &[u8; 32]) -> Result<Vec<KeyUtxo>, String> {
    let spk = p2pk_spk(maker);
    let mut found = lookup(node, network, std::slice::from_ref(&spk))?.remove(&spk_bytes(&spk)).unwrap_or_default();
    found.retain(|u| u.covenant_id.is_none() && !u.coinbase);
    found.sort_by_key(|u| (u.amount, u.txid, u.index));
    Ok(found.iter().map(|u| KeyUtxo { utxo: utxo_of(u), pubkey: *maker }).collect())
}

/// How a transaction was funded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Funding {
    /// The order's own released KAS paid the fee.
    Own,
    /// One of the maker's P2PK UTXOs was added.
    Key(KeyUtxo),
}

/// Build without funding when `try_unfunded`; when that fails for lack of funds (or is not tried), add the smallest of the
/// maker's P2PK UTXOs that makes the build succeed.
pub fn build_funded(
    try_unfunded: bool,
    node: &str,
    network: &str,
    maker: &[u8; 32],
    build: impl Fn(Vec<KeyUtxo>) -> kob_protocol::Result<BuiltTx>,
) -> Result<(BuiltTx, Funding), String> {
    if try_unfunded {
        match build(vec![]) {
            Ok(b) => return Ok((b, Funding::Own)),
            Err(kob_protocol::Error::InsufficientFunds { .. }) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let utxos = maker_funding(node, network, maker)?;
    if utxos.is_empty() {
        return Err(format!(
            "needs funding and the node holds no plain KAS UTXO of the maker ({})",
            address_of(&p2pk_spk(maker), network)?
        ));
    }
    let mut last = String::new();
    for u in utxos.into_iter().take(FUNDING_TRIES) {
        match build(vec![u.clone()]) {
            Ok(b) => return Ok((b, Funding::Key(u))),
            Err(e @ kob_protocol::Error::InsufficientFunds { .. }) => last = e.to_string(),
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(format!("none of the maker's KAS UTXOs is large enough: {last}"))
}

/// The signing key and its x-only public key.
pub struct Signer {
    /// Secret key bytes.
    pub secret: [u8; 32],
    /// x-only public key.
    pub pubkey: [u8; 32],
}

impl Signer {
    /// From a key source (`HEX | env:VAR | file:PATH`).
    pub fn load(src: &str) -> Result<Signer, String> {
        let sk = crate::token::read_key(&crate::token::parse_key_source(src))?;
        let secret = sk.secret_bytes();
        let pubkey = pubkey_of(&secret).map_err(|e| e.to_string())?;
        Ok(Signer { secret, pubkey })
    }
}

/// What happened to one transaction.
#[derive(Debug, Clone)]
pub struct Sent {
    /// Transaction id.
    pub txid: String,
    /// Fee in sompi.
    pub fee: u64,
    /// The file written, if any.
    pub file: Option<PathBuf>,
    /// The id the node accepted (`None`: dry run).
    pub submitted: Option<String>,
}

/// Sign every request of `built` with the signer (refusing a transaction that needs another key), finalize, validate in the
/// script engine (nothing is written or submitted unless this passes), write `<out_dir>/<file_name>`, submit unless `dry_run`.
pub fn sign_validate_send(
    built: &BuiltTx,
    signer: &Signer,
    out_dir: Option<&Path>,
    file_name: &str,
    extra: Value,
    dry_run: bool,
    node: &str,
) -> Result<Sent, String> {
    if let Some(r) = built.sign.iter().find(|r| r.pubkey != signer.pubkey) {
        return Err(format!("input {} needs the signature of {}, not of this key", r.input_index, to_hex(&r.pubkey)));
    }
    let keys = BTreeMap::from([(signer.pubkey, signer.secret)]);
    let sigs = sign_locally(built, &keys).map_err(|e| format!("sign: {e}"))?;
    let signed: SignedTx = finalize(built, &sigs, FinalizeOptions::default()).map_err(|e| format!("finalize: {e}"))?;
    validate_signed(&signed).map_err(|e| format!("engine validation failed (nothing submitted): {e}"))?;
    let (tx, _) = signed.tx.to_tx().map_err(|e| e.to_string())?;
    let rpc = rpc_transaction_json(&tx);
    let txid = tx.id().to_string();
    let mut file = None;
    if let Some(dir) = out_dir {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let doc = json!({
            "txid": txid,
            "fee": signed.fee.fee.to_string(),
            "info": extra,
            "signed": serde_json::to_value(&signed).map_err(|e| e.to_string())?,
            "submit_request": wrpc::submit_request(1, &rpc, false),
        });
        let path = dir.join(file_name);
        let mut s = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        s.push('\n');
        std::fs::write(&path, s).map_err(|e| format!("write {}: {e}", path.display()))?;
        file = Some(path);
    }
    let submitted = if dry_run {
        None
    } else {
        let id = wrpc::submit_transaction(node, &rpc, false, NODE_TIMEOUT).map_err(|e| format!("submit: {e}"))?;
        if id != txid {
            eprintln!("warning: the node reported transaction id {id}, expected {txid}");
        }
        Some(id)
    };
    Ok(Sent { txid, fee: signed.fee.fee, file, submitted })
}

/// Read and parse a JSON file.
pub fn read_json(path: &Path) -> Result<Value, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{} is not JSON: {e}", path.display()))
}

/// Write a JSON value (pretty, trailing newline).
pub fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    let mut s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    s.push('\n');
    std::fs::write(path, s).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Format sompi as KAS.
pub fn kas(sompi: u64) -> String {
    format!("{}.{:08}", sompi / 100_000_000, sompi % 100_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_follow_the_network() {
        let spk = p2pk_spk(&[0x11; 32]);
        assert!(address_of(&spk, "testnet-10").unwrap().starts_with("kaspatest:"));
        assert!(address_of(&spk, "mainnet").unwrap().starts_with("kaspa:"));
        assert!(address_of(&spk, "elsewhere").is_err());
        assert_eq!(spk_bytes(&spk)[..2], [0, 0]);
        assert_eq!(kas(123_456_789), "1.23456789");
    }

    #[test]
    fn amounts_are_base_units_or_decimals_of_the_tokens_decimals() {
        // a plain integer is base units, with or without a known scale
        assert_eq!(parse_amount("150000000", None), Ok(150_000_000));
        assert_eq!(parse_amount("150000000", Some(100_000_000)), Ok(150_000_000));
        // a decimal is whole tokens: 8 decimals
        assert_eq!(parse_amount("1.5", Some(100_000_000)), Ok(150_000_000));
        assert_eq!(parse_amount("0.00000001", Some(100_000_000)), Ok(1));
        assert_eq!(parse_amount(".5", Some(1_000)), Ok(500));
        assert_eq!(parse_amount("5.", Some(1_000)), Ok(5_000));
        assert_eq!(parse_amount("6.500", Some(1_000)), Ok(6_500));
        // more decimals than the token has, or no decimals known
        assert!(parse_amount("1.0001", Some(1_000)).is_err());
        assert!(parse_amount("1.5", None).is_err());
        assert!(parse_amount("1.5", Some(1)).is_err(), "a token without decimals has no fractions");
        assert!(parse_amount("1.5", Some(1_500)).is_err(), "the scale must be a power of ten");
        // not an amount
        for bad in ["", ".", "-1", "+1", "1e3", "1.2.3", " 5", "0x10", "1,5", "abc"] {
            assert!(parse_amount(bad, Some(1_000)).is_err(), "`{bad}`");
        }
        // positive only, and it must fit
        assert!(parse_amount("0", None).is_err());
        assert!(parse_amount("0.000", Some(1_000)).is_err());
        assert_eq!(parse_amount("9223372036854775807", None), Ok(i64::MAX));
        assert!(parse_amount("9223372036854775808", None).is_err());
        assert_eq!(parse_amount("9223372036854775.807", Some(1_000)), Ok(i64::MAX));
        assert!(parse_amount("9223372036854775.808", Some(1_000)).is_err());
        assert!(parse_amount("99999999999999999999999999999999999999999", None).is_err());
    }
}
