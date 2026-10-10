//! wasm-bindgen surface of `kob-protocol`.
//!
//! The web app and the x402 client SDK call the same Rust code as the CLI and the executor. Every
//! function takes and returns JSON strings in the `kob-protocol` JSON conventions (64-bit integers
//! as decimal strings, bytes as hex); transactions use kaspa-wasm's "safe JSON" layout so they load
//! with `Transaction.deserializeFromSafeJSON` and submit over node RPC.
//!
//! Signing flow: `build(request)` -> give `built.tx` and `built.sign` to the wallet (KasWare,
//! Kaspire, Kastle with `scripts`) -> collect signatures -> `finalize(built, signatures, options)`.
//!
//! [`api`] holds the plain-Rust implementations (tested natively against the golden vectors);
//! the exported functions only map errors to `JsError`.

use wasm_bindgen::prelude::*;

mod issue;

/// Implementations with `String` errors (native-testable).
pub mod api {
    use kob_protocol::artifacts::{self, TemplateId};
    use kob_protocol::budget;
    use kob_protocol::build::{self, Action};
    use kob_protocol::defaults;
    use kob_protocol::json::{from_hex, to_hex};
    use kob_protocol::payload::{self, Record};
    use kob_protocol::state::{self, AnyState, Kcc20State, KronState, Round, TokenState};
    #[cfg(feature = "engine")]
    use kob_protocol::tx::SignedTx;
    use kob_protocol::tx::{self, BuiltTx, FinalizeOptions, InputSignature, TxJson};
    #[cfg(feature = "engine")]
    use kob_protocol::verify;

    type R<T> = Result<T, String>;

    fn parse<T: serde::de::DeserializeOwned>(what: &str, json: &str) -> R<T> {
        serde_json::from_str(json).map_err(|e| format!("{what}: {e}"))
    }
    fn out<T: serde::Serialize>(v: &T) -> R<String> {
        serde_json::to_string(v).map_err(|e| e.to_string())
    }

    pub fn version() -> String {
        kob_protocol::VERSION.to_string()
    }
    pub fn self_check() -> R<()> {
        artifacts::self_check()
    }
    pub fn templates() -> R<String> {
        out(&artifacts::template_infos())
    }
    /// The trigger evidence a batch leg provides (`build::touch_of`): side, token, scale, quote, amount (base units) and
    /// `exposedSince` of a plain KobAsk / KobBid fill (an error for any other leg and for decaying orders).
    pub fn touch_of(leg: &str) -> R<String> {
        let l: build::Leg = parse("leg", leg)?;
        let t = build::touch_of(&l).map_err(|e| e.to_string())?;
        // 64-bit integers are decimal strings in this API (the protocol type serializes them as JSON numbers)
        out(&serde_json::json!({
            "side": t.side,
            "tokenCovId": to_hex(&t.token_cov_id),
            "scale": t.scale.to_string(),
            "price": t.price.to_string(),
            "amount": t.amount.to_string(),
            "exposedSince": t.exposed_since.to_string(),
        }))
    }
    /// the builders do not tie a destination key (order maker, change, token change, taker, keeper) to a signer
    /// key. A request MAY carry `"ownKeys": [<x-only pubkey hex>, ...]` (the wallet's keys); every key the action
    /// pays "to the user" (maker, change, tokenChange, taker, replacement maker) must then be one of them or the
    /// build is refused. Recipients of a plain token send, batch receivers and a route receiver are the deliberate
    /// foreign destinations and are not checked here (the app checks them against what the user typed).
    pub fn build(request: &str) -> R<String> {
        let mut v: serde_json::Value = parse("request", request)?;
        let own: Option<Vec<[u8; 32]>> = match v.as_object_mut().and_then(|o| o.remove("ownKeys")) {
            None | Some(serde_json::Value::Null) => None,
            Some(k) => {
                let list: Vec<String> = serde_json::from_value(k).map_err(|e| format!("ownKeys: {e}"))?;
                Some(list.iter().map(|s| kob_protocol::json::hex32(s).map_err(|e| format!("ownKeys: {e}"))).collect::<R<_>>()?)
            }
        };
        let a: Action = serde_json::from_value(v).map_err(|e| format!("request: {e}"))?;
        if let Some(own) = &own {
            check_own_keys(&a, own)?;
        }
        out(&build::build(&a).map_err(|e| e.to_string())?)
    }
    /// The keys an action pays back to the user must be the wallet's own (see [`build`]).
    pub fn check_own_keys(a: &Action, own: &[[u8; 32]]) -> R<()> {
        let chk = |what: &str, k: Option<[u8; 32]>| -> R<()> {
            match k {
                Some(k) if !own.contains(&k) => Err(format!("{what} {} is not one of the wallet keys", to_hex(&k))),
                _ => Ok(()),
            }
        };
        match a {
            Action::CreateOrder(r) => {
                chk("order maker", Some(r.order.maker()))?;
                chk("change key", r.change)
            }
            Action::CancelOrder(r) => {
                chk("order maker", Some(r.order.state.maker()))?;
                chk("change key", r.change)?;
                if let Some(rep) = &r.replace {
                    chk("replacement order maker", Some(rep.order.maker()))?;
                }
                Ok(())
            }
            Action::AmendOrder(r) => {
                chk("order maker", Some(r.order.state.maker()))?;
                chk("amended order maker", Some(r.amended.maker()))?;
                chk("change key", r.change)
            }
            Action::CancelPosition(r) => chk("change key", r.change),
            Action::RefundOrder(r) => chk("change key", r.change),
            Action::SendTokens(r) => {
                chk("token change key", r.token_change)?;
                chk("change key", r.change)
            }
            Action::Batch(r) => {
                chk("taker key", r.taker)?;
                chk("change key", r.change)
            }
            Action::SwapRoute(r) => chk("change key", r.change),
            Action::SweepOrder(r) => {
                chk("order maker", Some(r.order.state.maker()))?;
                chk("change key", r.change)
            }
        }
    }
    pub fn finalize(built: &str, signatures: &str, options: &str) -> R<String> {
        let b: BuiltTx = parse("built", built)?;
        let s: Vec<InputSignature> = parse("signatures", signatures)?;
        let o: FinalizeOptions = if options.trim().is_empty() { FinalizeOptions::default() } else { parse("options", options)? };
        out(&tx::finalize(&b, &s, o).map_err(|e| e.to_string())?)
    }
    #[cfg(feature = "engine")]
    pub fn validate(signed: &str) -> R<String> {
        let s: SignedTx = parse("signed", signed)?;
        out(&verify::validate_signed(&s).map_err(|e| e.to_string())?)
    }
    #[cfg(not(feature = "engine"))]
    pub fn validate(_signed: &str) -> R<String> {
        Err("validate needs the script engine (build kob-wasm with feature `engine`)".into())
    }
    pub fn masses(tx_json: &str) -> R<String> {
        let t: TxJson = parse("tx", tx_json)?;
        let (tx, entries) = t.to_tx().map_err(|e| e.to_string())?;
        out(&tx::masses(&tx, &entries))
    }
    pub fn encode_state(state: &str) -> R<String> {
        let s: AnyState = parse("state", state)?;
        s.validate().map_err(|e| e.to_string())?;
        Ok(to_hex(&s.try_encode().map_err(|e| e.to_string())?))
    }
    pub fn decode_state(kind: &str, hex: &str) -> R<String> {
        let id = TemplateId::from_name(kind).ok_or_else(|| format!("unknown template {kind}"))?;
        out(&AnyState::decode(id, &from_hex(hex)?).map_err(|e| e.to_string())?)
    }
    pub fn redeem_script(state: &str) -> R<String> {
        let s: AnyState = parse("state", state)?;
        s.validate().map_err(|e| e.to_string())?;
        Ok(to_hex(&s.redeem()))
    }
    pub fn script_public_key(state: &str) -> R<String> {
        let s: AnyState = parse("state", state)?;
        s.validate().map_err(|e| e.to_string())?;
        Ok(tx::spk_to_string(&s.spk()))
    }
    /// Encodes a token state of either family (the KRON layout has `id_type` / `is_minter`).
    pub fn encode_token_state(state: &str) -> R<String> {
        let s: TokenState = parse("token state", state)?;
        Ok(to_hex(&s.encode()))
    }
    /// Decodes a token state span: 46 bytes are KRON, 112 bytes KCC-20.
    pub fn decode_token_state(hex: &str) -> R<String> {
        let b = from_hex(hex)?;
        match b.len() {
            kob_protocol::artifacts::KRON_STATE_LEN => out(&KronState::decode(&b).map_err(|e| e.to_string())?),
            _ => out(&<Kcc20State as kob_protocol::state::StateCodec>::decode(&b).map_err(|e| e.to_string())?),
        }
    }
    /// P2SH script public key of a token state under a token program (`"KCC20Ref"`, `"KronToken2433"`, ...).
    pub fn token_script_public_key(program: &str, state: &str) -> R<String> {
        let id = TemplateId::from_name(program).filter(|t| t.is_token()).ok_or_else(|| format!("unknown token program {program}"))?;
        let s: TokenState = parse("token state", state)?;
        if s.family() != id.family() {
            return Err(format!("a {:?} token state under the {program} program", s.family()));
        }
        Ok(tx::spk_to_string(&s.spk_with(artifacts::token_template(id))))
    }
    pub fn encode_payload(records: &str) -> R<String> {
        let r: Vec<Record> = parse("records", records)?;
        Ok(to_hex(&payload::encode(&r).map_err(|e| e.to_string())?))
    }
    pub fn decode_payload(hex: &str) -> R<String> {
        out(&payload::decode(&from_hex(hex)?).map_err(|e| e.to_string())?)
    }
    pub fn recover_orders(tx_json: &str) -> R<String> {
        let t: TxJson = parse("tx", tx_json)?;
        out(&payload::recover_orders(&t).map_err(|e| e.to_string())?)
    }

    // ---------------------------------------------------------------------------- numbers and defaults
    //
    // 64-bit integers are decimal strings (as everywhere in this API). A helper returns `None` exactly where the covenant's
    // own arithmetic fails (an overflow, a rate below the tip, ...): the order cannot be filled that way.

    fn num(what: &str, s: &str) -> R<i64> {
        s.trim().parse().map_err(|e| format!("{what}: {e}"))
    }
    fn opt(v: Option<i64>) -> Option<String> {
        v.map(|x| x.to_string())
    }
    fn round_of(s: &str) -> R<Round> {
        match s.trim().to_ascii_lowercase().as_str() {
            "up" | "ceil" => Ok(Round::Up),
            "down" | "floor" => Ok(Round::Down),
            _ => Err(format!("round: \"up\" or \"down\", got {s:?}")),
        }
    }
    fn any(state: &str) -> R<AnyState> {
        parse("state", state)
    }
    fn wrong<T>(s: &AnyState, want: &str) -> R<T> {
        Err(format!("{} is not a {want} state", s.template_id().name()))
    }
    fn ask(state: &str) -> R<state::AskState> {
        match any(state)? {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => Ok(s),
            o => wrong(&o, "KobAsk"),
        }
    }
    fn bid(state: &str) -> R<state::BidState> {
        match any(state)? {
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => Ok(s),
            o => wrong(&o, "KobBid"),
        }
    }
    fn cond_ask(state: &str) -> R<state::CondAskState> {
        match any(state)? {
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => Ok(s),
            o => wrong(&o, "KobCondAsk"),
        }
    }
    fn cond_bid(state: &str) -> R<state::CondBidState> {
        match any(state)? {
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => Ok(s),
            o => wrong(&o, "KobCondBid"),
        }
    }
    fn ifd_bid(state: &str) -> R<state::IfdBidState> {
        match any(state)? {
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => Ok(s),
            o => wrong(&o, "KobIfdBid"),
        }
    }
    fn ifd_ask(state: &str) -> R<state::IfdAskState> {
        match any(state)? {
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => Ok(s),
            o => wrong(&o, "KobIfdAsk"),
        }
    }
    fn pair(state: &str) -> R<state::PairState> {
        match any(state)? {
            AnyState::KobPair(s) => Ok(s),
            o => wrong(&o, "KobPair"),
        }
    }
    fn cond_pair(state: &str) -> R<state::CondPairState> {
        match any(state)? {
            AnyState::KobCondPair(s) => Ok(s),
            o => wrong(&o, "KobCondPair"),
        }
    }
    fn ifd_pair(state: &str) -> R<state::IfdPairState> {
        match any(state)? {
            AnyState::KobIfdPair(s) => Ok(s),
            o => wrong(&o, "KobIfdPair"),
        }
    }
    fn flag(what: &str, s: &str) -> R<bool> {
        match s.trim() {
            "true" | "1" => Ok(true),
            "false" | "0" | "" => Ok(false),
            o => Err(format!("{what}: \"true\" or \"false\", got {o:?}")),
        }
    }
    fn evidence(ev: &str) -> R<state::PairEvidence> {
        parse("evidence", ev)
    }

    /// The quote rule of every covenant, `n * rate / scale` rounded `"up"` (what a maker receives) or `"down"` (what a maker
    /// pays) with the covenant's exact split multiplication (`state::quote_of`).
    pub fn quote(n: &str, rate: &str, scale: &str, round: &str) -> R<Option<String>> {
        Ok(opt(state::quote_of(num("n", n)?, num("rate", rate)?, num("scale", scale)?, round_of(round)?)))
    }
    /// The same value in 128-bit arithmetic (not limited to `i64`); `None` only for invalid inputs.
    pub fn quote_exact(n: &str, rate: &str, scale: &str, round: &str) -> R<Option<String>> {
        Ok(state::quote_exact(num("n", n)?, num("rate", rate)?, num("scale", scale)?, round_of(round)?).map(|v| v.to_string()))
    }
    /// `scale` is a power of ten in `1..=10^9` (an error otherwise).
    pub fn check_scale(scale: &str) -> R<()> {
        state::check_scale(num("scale", scale)?)
    }
    /// The full fill of `amount` at `rate` is worth less than 2^62 quote units (an error otherwise; `what` names the rate).
    pub fn check_quote(amount: &str, rate: &str, scale: &str, what: &str) -> R<()> {
        state::check_quote(num("amount", amount)?, num("rate", rate)?, num("scale", scale)?, what)
    }
    /// The numeric gate of a placed order (`AnyState::check_numbers`: scale and the 2^62 bound of every rate it carries).
    pub fn check_numbers(state: &str) -> R<()> {
        any(state)?.check_numbers()
    }
    /// The minimum-fill rule of the kinds whose quantity is an explicit amount: `0 < n <= left`, `n >= minFill` unless n takes all.
    pub fn min_fill_ok(n: &str, left: &str, min_fill: &str) -> R<bool> {
        Ok(state::min_fill_ok(num("n", n)?, num("left", left)?, num("minFill", min_fill)?))
    }
    pub fn default_scale(decimals: u32) -> String {
        defaults::default_scale(decimals).to_string()
    }
    pub fn default_min_fill(amount: &str, price: &str, scale: &str) -> R<String> {
        Ok(defaults::default_min_fill(num("amount", amount)?, num("price", price)?, num("scale", scale)?).to_string())
    }
    pub fn default_min_fill_ifd(amount: &str) -> R<String> {
        Ok(defaults::default_min_fill_ifd(num("amount", amount)?).to_string())
    }
    /// A pair order's default `minFill` (base units of A): `kas_per_whole_a` is the KAS value (sompi per whole A) of A's book,
    /// `""` for none (then `ceil(amount / 4)`).
    pub fn default_min_fill_pair(amount: &str, kas_per_whole_a: &str, scale: &str) -> R<String> {
        let p = if kas_per_whole_a.trim().is_empty() { None } else { Some(num("kasPerWholeA", kas_per_whole_a)?) };
        Ok(defaults::default_min_fill_pair(num("amount", amount)?, p, num("scale", scale)?).to_string())
    }
    /// A stop's default `minTouch`: `amount` is the order's amount (`""`: unknown, then its minimum fill).
    pub fn default_min_touch(min_fill: &str, amount: &str) -> R<String> {
        let a = if amount.trim().is_empty() { 0 } else { num("amount", amount)? };
        Ok(defaults::default_min_touch(num("minFill", min_fill)?, a).to_string())
    }
    /// The wallet default constants: `{defaultOrderCarrier, defaultMinFillSompi, defaultMinFillImmediate, maxScale, quoteLimit,
    /// marketAuctionDaa, slippageBps, marketActivationDaa, iocLifeDaa, stopBandDaa, minRestDaa, daaRateMilli}` (integers as
    /// decimal strings).
    pub fn default_constants() -> R<String> {
        out(&serde_json::json!({
            "defaultOrderCarrier": defaults::DEFAULT_ORDER_CARRIER.to_string(),
            "defaultMinFillSompi": defaults::DEFAULT_MIN_FILL_SOMPI.to_string(),
            "defaultMinFillImmediate": defaults::DEFAULT_MIN_FILL_IMMEDIATE.to_string(),
            "maxScale": state::MAX_SCALE.to_string(),
            "quoteLimit": state::QUOTE_LIMIT.to_string(),
            "marketAuctionDaa": defaults::MARKET_AUCTION_DAA.to_string(),
            "slippageBps": defaults::SLIPPAGE_BPS.to_string(),
            "marketActivationDaa": defaults::MARKET_ACTIVATION_DAA.to_string(),
            "iocLifeDaa": defaults::IOC_LIFE_DAA.to_string(),
            "stopBandDaa": defaults::STOP_BAND_DAA.to_string(),
            "minRestDaa": defaults::MIN_REST_DAA.to_string(),
            "daaRateMilli": defaults::DAA_RATE_MILLI.to_string(),
        }))
    }

    // ---------------------------------------------------------------------------- per-kind helpers
    // Each takes the order state (`{"kind", "state"}`, either family) and base-unit amounts; `t` is the auction time (DAA) and
    // `utxo_daa` the DAA score of the order UTXO's block.

    /// Ask or bid price at auction time `t` (decaying / rising orders; the constant price otherwise); a `KobPair`'s quote at `t` (B
    /// base units per whole A).
    pub fn order_price_at(state: &str, t: &str, utxo_daa: &str) -> R<Option<String>> {
        let (t, d) = (num("t", t)?, num("utxoDaa", utxo_daa)?);
        match any(state)? {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => Ok(opt(s.price_at(t, d))),
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => Ok(opt(s.price_at(t, d))),
            AnyState::KobPair(s) => Ok(opt(s.price_at(t, d))),
            o => wrong(&o, "KobAsk / KobBid / KobPair"),
        }
    }
    /// What a fill of `n` of an ask must pay the maker at auction time `t`: `ceil(n * (price(t) - tip) / scale)`.
    pub fn ask_proceeds(state: &str, n: &str, t: &str, utxo_daa: &str) -> R<Option<String>> {
        Ok(opt(ask(state)?.proceeds(num("n", n)?, num("t", t)?, num("utxoDaa", utxo_daa)?)))
    }
    /// [`ask_proceeds`] at an explicit quote `p`.
    pub fn ask_proceeds_at(state: &str, n: &str, p: &str) -> R<Option<String>> {
        Ok(opt(ask(state)?.proceeds_at(num("n", n)?, num("p", p)?)))
    }
    /// Budget a fill of `n` consumes from a bid's escrow: `ceil(n * (pMax + tip) / scale)`.
    pub fn bid_used(state: &str, n: &str) -> R<Option<String>> {
        Ok(opt(bid(state)?.used(num("n", n)?)))
    }
    /// Most the bid's maker pays for `n` at auction time `t`: `floor(n * (price(t) + tip) / scale)`.
    pub fn bid_spend(state: &str, n: &str, t: &str, utxo_daa: &str) -> R<Option<String>> {
        Ok(opt(bid(state)?.spend(num("n", n)?, num("t", t)?, num("utxoDaa", utxo_daa)?)))
    }
    /// [`bid_spend`] at an explicit quote `p`.
    pub fn bid_spend_at(state: &str, n: &str, p: &str) -> R<Option<String>> {
        Ok(opt(bid(state)?.spend_at(num("n", n)?, num("p", p)?)))
    }
    /// Budget rate `pMax + tip` of a bid (sompi per whole token).
    pub fn bid_budget_rate(state: &str) -> R<Option<String>> {
        Ok(opt(bid(state)?.budget_rate()))
    }
    /// Remaining buying power (base units) of a bid escrow worth `value` sompi.
    pub fn bid_buying_power(state: &str, value: &str) -> R<String> {
        Ok(bid(state)?.buying_power(num("value", value)?).to_string())
    }
    /// Whether a bid whose escrow keeps `left` sompi after a fill can continue.
    pub fn bid_can_continue(state: &str, left: &str) -> R<bool> {
        Ok(bid(state)?.can_continue(num("left", left)?))
    }
    /// Escrow of a bid for `amount` base units in at most `fills` fills.
    pub fn bid_escrow(state: &str, amount: &str, fills: &str) -> R<Option<String>> {
        Ok(opt(bid(state)?.escrow(num("amount", amount)?, num("fills", fills)?)))
    }
    /// The covenant's quantity rules for a fill of `n`: every kind but a plain bid ignores `value`; a bid needs the escrow
    /// `value` (sompi of its UTXO).
    pub fn fill_ok(state: &str, n: &str, value: &str) -> R<bool> {
        let n = num("n", n)?;
        Ok(match any(state)? {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => s.fill_ok(n),
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => s.fill_ok(n, num("value", value)?),
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => s.fill_ok(n),
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => s.fill_ok(n),
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => s.fill_ok(n),
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => s.fill_ok(n),
            AnyState::KobPair(s) => s.fill_ok(n),
            AnyState::KobCondPair(s) => s.fill_ok(n),
            AnyState::KobIfdPair(s) => s.fill_ok(n),
        })
    }
    /// Conditional ask: least proceeds of `n` at a leg price (`ceil(n * (legPrice - tip) / scale)`).
    pub fn cond_ask_proceeds(state: &str, n: &str, leg_price: &str) -> R<Option<String>> {
        Ok(opt(cond_ask(state)?.proceeds(num("n", n)?, num("legPrice", leg_price)?)))
    }
    /// Conditional ask (repeat): the budget a take-profit of `n` returns to the entry, `ceil(n * rptPrice / scale)`.
    pub fn cond_ask_rpt_budget(state: &str, n: &str) -> R<Option<String>> {
        Ok(opt(cond_ask(state)?.rpt_budget(num("n", n)?)))
    }
    /// Conditional bid: most paid for `n` at a leg price (`floor(n * (legPrice + tip) / scale)`).
    pub fn cond_bid_spend(state: &str, n: &str, leg_price: &str) -> R<Option<String>> {
        Ok(opt(cond_bid(state)?.spend(num("n", n)?, num("legPrice", leg_price)?)))
    }
    /// Conditional bid (repeat): the entry's proceeds of a take-profit of `n`, `ceil(n * rptPrice / scale)`.
    pub fn cond_bid_rpt_proceeds(state: &str, n: &str) -> R<Option<String>> {
        Ok(opt(cond_bid(state)?.rpt_proceeds(num("n", n)?)))
    }
    /// Conditional bid (repeat): the prefund of `n` returned to the entry, `ceil(n * rptPre / scale)`.
    pub fn cond_bid_rpt_prefund(state: &str, n: &str) -> R<Option<String>> {
        Ok(opt(cond_bid(state)?.rpt_prefund(num("n", n)?)))
    }
    /// Conditional bid: escrow for all `amountLeft` at the worst leg in at most `fills` fills.
    pub fn cond_bid_escrow(state: &str, fills: &str) -> R<Option<String>> {
        Ok(opt(cond_bid(state)?.escrow(num("fills", fills)?)))
    }
    /// If-done buy entry: most paid for `n` at quote `p`.
    pub fn ifd_bid_spend(state: &str, n: &str, p: &str) -> R<Option<String>> {
        Ok(opt(ifd_bid(state)?.spend(num("n", n)?, num("p", p)?)))
    }
    /// If-done buy entry: what a merge of `m` returns to the entry, `ceil(m * (price + tip) / scale)`.
    pub fn ifd_bid_merge_budget(state: &str, m: &str) -> R<Option<String>> {
        Ok(opt(ifd_bid(state)?.merge_budget(num("m", m)?)))
    }
    /// If-done buy entry: its escrow (limit spend of the whole amount, carriers per possible fill).
    pub fn ifd_bid_escrow(state: &str) -> R<Option<String>> {
        Ok(opt(ifd_bid(state)?.escrow()))
    }
    /// If-done sell entry: least proceeds of `n` at quote `p`.
    pub fn ifd_ask_proceeds(state: &str, n: &str, p: &str) -> R<Option<String>> {
        Ok(opt(ifd_ask(state)?.proceeds(num("n", n)?, num("p", p)?)))
    }
    /// If-done sell entry: the prefund of `n` base units, `ceil(n * prefund / scale)`.
    pub fn ifd_ask_prefund(state: &str, n: &str) -> R<Option<String>> {
        Ok(opt(ifd_ask(state)?.prefund_of(num("n", n)?)))
    }
    /// If-done sell entry: what a merge of `m` returns from an exit that sells out holding `exit_value`.
    pub fn ifd_ask_merge_sellout_back(state: &str, m: &str, exit_value: &str) -> R<Option<String>> {
        Ok(opt(ifd_ask(state)?.merge_sellout_back(num("m", m)?, num("exitValue", exit_value)?)))
    }
    /// If-done sell entry: its value (carrier `carrier` plus the prefund of the whole amount and the exit carriers).
    pub fn ifd_ask_escrow(state: &str, carrier: &str) -> R<Option<String>> {
        Ok(opt(ifd_ask(state)?.escrow(num("carrier", carrier)?)))
    }

    // ---------------------------------------------------------------------------- pair orders
    //
    // `KobPair` (plain), `KobCondPair` (stops, take-profit, OCO, the exits of if-done entries), `KobIfdPair` (if-done entries): an
    // amount of the base token A at a price in B BASE UNITS PER WHOLE A (`scale(A)` base units), the maker-favour rounding of
    // every kind. Tips, keeper tips and carriers are KAS (`tip`: sompi per whole A, released rounded down).

    fn pair_token_json(t: &state::PairToken) -> serde_json::Value {
        serde_json::json!({
            "covId": to_hex(&t.cov_id),
            "tplHash": to_hex(&t.tpl_hash),
            "prefixLen": t.prefix_len,
            "suffixLen": t.suffix_len,
            "family": t.family,
            "scale": t.scale.to_string(),
            "ext": t.ext.map(|e| to_hex(&e)),
        })
    }
    fn side_name(ask: bool) -> &'static str {
        if ask {
            "ask"
        } else {
            "bid"
        }
    }
    /// The two tokens of a pair order: `{kind, side: "ask" | "bid", a, b}`, each token `{covId, tplHash, prefixLen, suffixLen,
    /// family, scale, ext}` (`ext`: the extension commitment of the outputs the order creates of that token, `null` where the state
    /// carries none). A `KobIfdPair` buy-first entry is a `bid`, a sell-first one an `ask`.
    pub fn pair_tokens(state: &str) -> R<String> {
        let s = any(state)?;
        let t = s.pair_tokens().ok_or_else(|| format!("{} is not a pair order", s.template_id().name()))?;
        let ask = match &s {
            AnyState::KobPair(p) => p.is_ask(),
            AnyState::KobCondPair(c) => c.is_ask(),
            AnyState::KobIfdPair(i) => !i.is_buy_first(),
            _ => unreachable!("pair_tokens is Some for the pair kinds only"),
        };
        out(&serde_json::json!({
            "kind": s.template_id().name(),
            "side": side_name(ask),
            "a": pair_token_json(&t.a),
            "b": pair_token_json(&t.b),
        }))
    }
    /// The token programs of a pair order's A and B (`build::pair_programs`): `{a: {covId, program}, b: {covId, program}}`.
    pub fn pair_programs(state: &str) -> R<String> {
        let (a, b) = build::pair_programs(&any(state)?).map_err(|e| e.to_string())?;
        out(&serde_json::json!({
            "a": { "covId": to_hex(&a.0), "program": a.1.name() },
            "b": { "covId": to_hex(&b.0), "program": b.1.name() },
        }))
    }
    /// The exact custodies an order holds, in record order (`AnyState::custodies`): `[{token, amount}]`. A pair order: the custody
    /// of the token it sells; a sell-first `KobIfdPair` its A custody then its B prefund (each only when non-zero).
    pub fn custodies(state: &str) -> R<String> {
        let v: Vec<serde_json::Value> =
            any(state)?.custodies().iter().map(|(t, a)| serde_json::json!({ "token": to_hex(t), "amount": a.to_string() })).collect();
        out(&v)
    }
    /// The order rules every builder applies to a NEW order (`build::check_new_order`; pair orders: both programs, carriers, the
    /// escrow / custodies they need, KRON caps, ...). An error names the first rule broken.
    pub fn check_new_order(state: &str) -> R<()> {
        build::check_new_order(&any(state)?).map_err(|e| e.to_string())
    }
    /// The least KAS a new order UTXO must hold (`build::min_order_value`; a pair order: its carriers and the tip of its amount).
    pub fn min_order_value(state: &str) -> R<String> {
        Ok(build::min_order_value(&any(state)?).to_string())
    }
    /// S (the token the order sells / pays) released by a fill of `n` base units of A at quote `p` (B per whole A): an ask `n`,
    /// a bid `floor(n * p / scale(A))` (`KobPair`, `KobCondPair` at a leg price).
    pub fn pair_s_out(state: &str, n: &str, p: &str) -> R<Option<String>> {
        let (n, p) = (num("n", n)?, num("p", p)?);
        match any(state)? {
            AnyState::KobPair(s) => Ok(opt(s.s_out(n, p))),
            AnyState::KobCondPair(s) => Ok(opt(s.s_out(n, p))),
            o => wrong(&o, "KobPair / KobCondPair"),
        }
    }
    /// The least T the maker receives for `n` base units of A at quote `p`: an ask `ceil(n * p / scale(A))` of B, a bid exactly `n`
    /// of A (`KobPair`, `KobCondPair` at a leg price).
    pub fn pair_t_out_min(state: &str, n: &str, p: &str) -> R<Option<String>> {
        let (n, p) = (num("n", n)?, num("p", p)?);
        match any(state)? {
            AnyState::KobPair(s) => Ok(opt(s.t_out_min(n, p))),
            AnyState::KobCondPair(s) => Ok(opt(s.t_out_min(n, p))),
            o => wrong(&o, "KobPair / KobCondPair"),
        }
    }
    /// The KAS tip (sompi) a fill of `n` base units of A releases to the filler, `floor(n * tip / scale(A))` (every pair kind).
    pub fn pair_tip_kas(state: &str, n: &str) -> R<Option<String>> {
        let n = num("n", n)?;
        match any(state)? {
            AnyState::KobPair(s) => Ok(opt(s.tip_kas(n))),
            AnyState::KobCondPair(s) => Ok(opt(s.tip_kas(n))),
            AnyState::KobIfdPair(s) => Ok(opt(s.tip_kas(n))),
            o => wrong(&o, "KobPair / KobCondPair / KobIfdPair"),
        }
    }
    /// The complete covenant rule of a `KobPair` fill of `n` at quote `p` of an order UTXO holding `value` sompi
    /// (`PairState::fill`): `{sOut, tOut, tipKas, rest, outAmount}` (decimal strings; `rest` boolean), or an error with the reason.
    pub fn pair_fill(state: &str, n: &str, p: &str, value: &str) -> R<String> {
        let f = pair(state)?.fill(num("n", n)?, num("p", p)?, num("value", value)?)?;
        out(&serde_json::json!({
            "sOut": f.s_out.to_string(),
            "tOut": f.t_out.to_string(),
            "tipKas": f.tip_kas.to_string(),
            "rest": f.rest,
            "outAmount": f.out_amount.to_string(),
        }))
    }
    /// The largest `n` anyone can take now from a `KobPair` at quote `p` with the order UTXO holding `value` sompi (0: none).
    pub fn pair_max_takeable(state: &str, p: &str, value: &str) -> R<String> {
        Ok(pair(state)?.max_takeable(num("p", p)?, num("value", value)?).to_string())
    }
    /// The highest quote of a `KobPair` (a rising bid's `priceEnd`, else `price`; an ask's highest).
    pub fn pair_price_max(state: &str) -> R<String> {
        Ok(pair(state)?.price_max().to_string())
    }
    /// A `KobPair` BID's B escrow for `amount` base units of A in at most `fills` fills: `ceil(amount * pMax / scale(A)) + fills`.
    pub fn pair_bid_escrow(state: &str, amount: &str, fills: &str) -> R<Option<String>> {
        Ok(opt(pair(state)?.bid_escrow(num("amount", amount)?, num("fills", fills)?)))
    }
    /// KAS (sompi) a pair order UTXO needs: `KobPair` / `KobCondPair` one delivery carrier per fill (`fills`) and the tip of the
    /// whole amount; `KobIfdPair` (ignores `fills`) per possible fill a delivery and an exit carrier, the tip, and a repeating
    /// entry one more exit carrier.
    pub fn pair_kas_value(state: &str, fills: &str) -> R<Option<String>> {
        match any(state)? {
            AnyState::KobPair(s) => Ok(opt(s.kas_value(num("fills", fills)?))),
            AnyState::KobCondPair(s) => Ok(opt(s.kas_value(num("fills", fills)?))),
            AnyState::KobIfdPair(s) => Ok(opt(s.kas_value())),
            o => wrong(&o, "KobPair / KobCondPair / KobIfdPair"),
        }
    }
    /// Most fills a pair order can take, `ceil(amountLeft / minFill)`.
    pub fn pair_max_fills(state: &str) -> R<String> {
        match any(state)? {
            AnyState::KobPair(s) => Ok(s.max_fills().to_string()),
            AnyState::KobCondPair(s) => Ok(s.max_fills().to_string()),
            AnyState::KobIfdPair(s) => Ok(s.max_fills().to_string()),
            o => wrong(&o, "KobPair / KobCondPair / KobIfdPair"),
        }
    }
    /// The deliveries a NEW `KobPair` / `KobCondPair` must fund itself (`funded_fills`: one when it never rests after a fill, else a
    /// partial fill and the fill of its rest, a TWAP / DCA order one per `maxFill` slice): `pairKasValue(state, pairFundedFills(state))`
    /// is the least KAS of its UTXO (`minOrderValue`).
    pub fn pair_funded_fills(state: &str) -> R<String> {
        match any(state)? {
            AnyState::KobPair(s) => Ok(s.funded_fills().to_string()),
            AnyState::KobCondPair(s) => Ok(s.funded_fills().to_string()),
            o => wrong(&o, "KobPair / KobCondPair"),
        }
    }
    /// `KobIfdPair`: the KAS each exit needs (its `exitCarrier` at least): the keeper reserve of an exit stop, then the exit's own
    /// deliveries and tip of the entry's whole amount, or its refund tip (`IfdPairState::exit_carrier_needed`).
    pub fn ifd_pair_exit_carrier_needed(state: &str) -> R<Option<String>> {
        Ok(opt(ifd_pair(state)?.exit_carrier_needed()))
    }
    /// `KobCondPair` leg price (`leg` 0 = take-profit / limit, 1 = stop) at auction time `t` (`trigger`: armed by this very
    /// transaction's evidence, `"true"` / `"false"`).
    pub fn cond_pair_leg_price(state: &str, leg: &str, trigger: &str, t: &str, utxo_daa: &str) -> R<Option<String>> {
        let s = cond_pair(state)?;
        Ok(opt(s.leg_price(num("leg", leg)?, flag("trigger", trigger)?, num("t", t)?, num("utxoDaa", utxo_daa)?)))
    }
    /// `KobCondPair` bounds: `{stopWorst, worst, lowest}` (the band's worst stop price; a BID's worst price either leg pays; an
    /// ASK's lowest price either leg receives).
    pub fn cond_pair_bounds(state: &str) -> R<String> {
        let s = cond_pair(state)?;
        out(&serde_json::json!({
            "stopWorst": s.stop_worst().to_string(),
            "worst": s.worst().to_string(),
            "lowest": s.lowest().to_string(),
        }))
    }
    /// `KobCondPair` (repeat exit): `ceil(n * rptPrice / scale(A))` (an ASK exit: the entry's budget; a BID exit: its proceeds).
    pub fn cond_pair_rpt_proceeds(state: &str, n: &str) -> R<Option<String>> {
        Ok(opt(cond_pair(state)?.rpt_proceeds(num("n", n)?)))
    }
    /// `KobCondPair` (repeat BID exit): the prefund returned to the entry, `ceil(n * rptPre / scale(A))`.
    pub fn cond_pair_rpt_back(state: &str, n: &str) -> R<Option<String>> {
        Ok(opt(cond_pair(state)?.rpt_back(num("n", n)?)))
    }
    /// `KobCondPair` BID: the B escrow for all of `amountLeft` at the worst leg plus one base unit per fill (`fills`).
    pub fn cond_pair_bid_escrow(state: &str, fills: &str) -> R<Option<String>> {
        Ok(opt(cond_pair(state)?.bid_escrow(num("fills", fills)?)))
    }
    /// Whether trigger evidence arms the stop of a `KobCondPair` / a stop entry of a `KobIfdPair` (`undefined` where the covenant's
    /// arithmetic fails). `evidence`: `{"mode": "kasBooks", "a": <sompi per whole A>, "b": <sompi per whole B>}` (two KAS-book fills,
    /// the implied rate) or `{"mode": "pair", "price": <B per whole A>}` (a resting `KobPair` of the pair).
    pub fn pair_arms(state: &str, ev: &str) -> R<Option<bool>> {
        let ev = evidence(ev)?;
        match any(state)? {
            AnyState::KobCondPair(s) => Ok(s.arms(ev)),
            AnyState::KobIfdPair(s) => Ok(s.arms(ev)),
            o => wrong(&o, "KobCondPair / KobIfdPair"),
        }
    }
    /// The trailing ratchet the evidence justifies on a `KobCondPair` (the valid and maximal k), or `undefined` (none).
    pub fn cond_pair_trail_k(state: &str, ev: &str) -> R<Option<String>> {
        Ok(opt(cond_pair(state)?.trail_k(evidence(ev)?)))
    }
    /// The covenant's trailing rule for a given `k` (valid and maximal).
    pub fn cond_pair_trail_check(state: &str, ev: &str, k: &str) -> R<bool> {
        Ok(cond_pair(state)?.trail_check(evidence(ev)?, num("k", k)?))
    }
    /// The least B evidence fill of a mode-0 (two KAS books) trigger at the order's current stop, `ceil(minTouch * stop / scale(A))`.
    pub fn pair_min_touch_b(state: &str) -> R<Option<String>> {
        match any(state)? {
            AnyState::KobCondPair(s) => Ok(opt(s.min_touch_b())),
            AnyState::KobIfdPair(s) => Ok(opt(s.min_touch_b())),
            o => wrong(&o, "KobCondPair / KobIfdPair"),
        }
    }
    /// `KobIfdPair`: the committed exit (a `KobCondPair` state `{"kind", "state"}` with amountLeft, custody and the repeat fields 0).
    pub fn ifd_pair_exit(state: &str) -> R<String> {
        out(&AnyState::KobCondPair(ifd_pair(state)?.exit().map_err(|e| e.to_string())?))
    }
    /// `KobIfdPair`: the exit a fill of `n` creates holding `custody` (buy-first: n of A; sell-first: the proceeds plus the prefund of
    /// n, in B), booked with `parent` (the entry's covenant id, hex) and `rptUntil` `until` when the entry repeats (`parent` `""`: not
    /// booked).
    pub fn ifd_pair_exit_for(state: &str, n: &str, custody: &str, parent: &str, until: &str) -> R<String> {
        let s = ifd_pair(state)?;
        let booking = if parent.trim().is_empty() {
            None
        } else {
            Some(state::Booking {
                parent: kob_protocol::json::hex32(parent).map_err(|e| format!("parent: {e}"))?,
                until: num("until", until)?,
            })
        };
        out(&AnyState::KobCondPair(s.exit_for(num("n", n)?, num("custody", custody)?, booking).map_err(|e| e.to_string())?))
    }
    /// The `exitState` (hex, `IFD_PAIR_EXIT_COMMIT` = 432 bytes) a `KobIfdPair` entry commits for the exit `exit` (a `KobCondPair`
    /// state `{"kind", "state"}`; its amountLeft, custody and repeat fields are not part of the commitment).
    pub fn ifd_pair_commit_exit(exit: &str) -> R<String> {
        Ok(to_hex(&state::IfdPairState::commit_exit(&cond_pair(exit)?)))
    }
    /// `KobIfdPair` entry quote at time `t` (a stop entry's auction from `entryStop` to `price`; the limit otherwise).
    pub fn ifd_pair_price_at(state: &str, trigger: &str, t: &str, utxo_daa: &str) -> R<Option<String>> {
        let s = ifd_pair(state)?;
        Ok(opt(s.price_at(flag("trigger", trigger)?, num("t", t)?, num("utxoDaa", utxo_daa)?)))
    }
    /// `KobIfdPair` amounts of a fill of `n` at quote `p`: `{spend, proceeds, pre, mergeBudget}` (buy-first: the most B released,
    /// `floor(n * p / scale(A))`; sell-first: the least proceeds `ceil(n * p / scale(A))` and the prefund moved to the exit
    /// `ceil(n * prefund / scale(A))`; a merge of n: the budget a buy-first entry gets back, `ceil(n * price / scale(A))`); `null`
    /// where the covenant's arithmetic fails.
    pub fn ifd_pair_amounts(state: &str, n: &str, p: &str) -> R<String> {
        let s = ifd_pair(state)?;
        let (n, p) = (num("n", n)?, num("p", p)?);
        out(&serde_json::json!({
            "spend": opt(s.spend(n, p)),
            "proceeds": opt(s.proceeds(n, p)),
            "pre": opt(s.pre_of(n)),
            "mergeBudget": opt(s.merge_budget(n)),
        }))
    }
    /// `KobIfdPair`: the B custody a new entry needs (buy-first: the spend of its whole amount at the limit; sell-first: the prefund
    /// of its whole amount plus one base unit per possible fill but the last).
    pub fn ifd_pair_b_custody_needed(state: &str) -> R<Option<String>> {
        Ok(opt(ifd_pair(state)?.b_custody_needed()))
    }
    /// Mode-0 evidence comparison `a <= floor(x * b / scale(B))`: the KAS books imply a rate of at most `x` (B per whole A).
    pub fn implied_le(a: &str, b: &str, x: &str, b_scale: &str) -> R<Option<bool>> {
        Ok(state::implied_le(num("a", a)?, num("b", b)?, num("x", x)?, num("bScale", b_scale)?))
    }
    /// Mode-0 evidence comparison `a >= ceil(x * b / scale(B))`: the implied rate is at least `x`.
    pub fn implied_ge(a: &str, b: &str, x: &str, b_scale: &str) -> R<Option<bool>> {
        Ok(state::implied_ge(num("a", a)?, num("b", b)?, num("x", x)?, num("bScale", b_scale)?))
    }
    /// The least B evidence fill of a mode-0 trigger at `stop`: `ceil(minTouch * stop / scale(A))`.
    pub fn min_touch_b(min_touch: &str, stop: &str, a_scale: &str) -> R<Option<String>> {
        Ok(opt(state::min_touch_b(num("minTouch", min_touch)?, num("stop", stop)?, num("aScale", a_scale)?)))
    }
    /// The two evidence modes of the pair conditionals (`evMode`): `[{mode, name, description}]`.
    pub fn pair_evidence_modes() -> R<String> {
        out(&serde_json::json!([
            {
                "mode": 0,
                "name": "kasBooks",
                "description": "Two plain resting KAS-book orders, one of A and one of B, filled together in one transaction: the \
                    implied rate is a * scale(B) / b (a, b: their KAS prices per whole token). Each rested at least minRestDaa, \
                    not decaying, of the token's standard scale; the A fill is at least minTouch base units of A and the B fill \
                    at least ceil(minTouch * stop / scale(A)) of B. Only quotes that cost money to fake count: a sell stop arms \
                    on a resting ask of A and a resting bid of B (the rate fell), a buy stop on a resting bid of A and a resting \
                    ask of B (the rate rose)."
            },
            {
                "mode": 1,
                "name": "pair",
                "description": "A resting KobPair order of the same pair (same A, B and scales) filled in the transaction: not \
                    decaying, rested at least minRestDaa, at least minTouch base units of A. A sell stop arms on a resting pair \
                    ASK quoting at or below the stop, a buy stop on a resting pair BID quoting at or above it. No KAS price is \
                    derived from such fills."
            }
        ]))
    }
    /// The trigger rule of a pair conditional (`KobCondPair` with a stop, a stop entry of `KobIfdPair`): which evidence arms it and,
    /// for a trailing stop, which evidence ratchets it. `{kind, stop, direction: "fallsTo" | "risesTo", minTouch, minTouchB,
    /// minRestDaa, arm: {kasBooks: {a, b}, pair}, trail: null | {direction: "up" | "down", kasBooks: {a, b}, pair, step, gap}}`
    /// (`a` / `b` / `pair` name the resting order side, `"ask"` or `"bid"`, that counts). `null` for an order without a stop.
    pub fn pair_trigger_rule(state: &str) -> R<String> {
        let books = |a: bool, b: bool| serde_json::json!({ "a": side_name(a), "b": side_name(b) });
        let rule = |kind: &str, sell: bool, stop: i64, mt: i64, mtb: Option<i64>, rest: i64, trail: serde_json::Value| {
            serde_json::json!({
                "kind": kind,
                "stop": stop.to_string(),
                "direction": if sell { "fallsTo" } else { "risesTo" },
                "minTouch": mt.to_string(),
                "minTouchB": opt(mtb),
                "minRestDaa": rest.to_string(),
                // a sell stop arms on cheap A offered and dear B bid (a resting ask of A, a resting bid of B, or a pair ASK at or below
                // the stop); a buy stop on the mirror image
                "arm": { "kasBooks": books(sell, !sell), "pair": side_name(sell) },
                "trail": trail,
            })
        };
        match any(state)? {
            AnyState::KobCondPair(c) => {
                if c.stop_price <= 0 {
                    return out(&serde_json::Value::Null);
                }
                let sell = c.is_ask();
                let trail = if c.trail_step > 0 {
                    // a sell stop trails UP on a high rate (a bid of A, an ask of B, or a pair BID); a buy stop DOWN on a low one
                    serde_json::json!({
                        "direction": if sell { "up" } else { "down" },
                        "kasBooks": books(!sell, sell),
                        "pair": side_name(!sell),
                        "step": c.trail_step.to_string(),
                        "gap": c.trail_gap.to_string(),
                    })
                } else {
                    serde_json::Value::Null
                };
                out(&rule("KobCondPair", sell, c.stop_price, c.min_touch, c.min_touch_b(), c.min_rest_daa, trail))
            }
            AnyState::KobIfdPair(i) => {
                if i.entry_stop <= 0 {
                    return out(&serde_json::Value::Null);
                }
                // buy-first entry = a buy stop (the rate rose), sell-first = a sell stop
                out(&rule(
                    "KobIfdPair",
                    !i.is_buy_first(),
                    i.entry_stop,
                    i.min_touch,
                    i.min_touch_b(),
                    i.min_rest_daa,
                    serde_json::Value::Null,
                ))
            }
            o => wrong(&o, "KobCondPair / KobIfdPair"),
        }
    }
    /// The in-place amends of a transaction (AMEND records). A signed transaction proves the previous state by its
    /// order inputs' signature scripts; for a built, unsigned one pass its signing plans (`built.plans`, JSON) and the
    /// previous states are the plans'.
    pub fn recover_amends(tx_json: &str, plans_json: &str) -> R<String> {
        let t: TxJson = parse("tx", tx_json)?;
        if plans_json.trim().is_empty() {
            out(&payload::recover_amends(&t).map_err(|e| e.to_string())?)
        } else {
            let plans: Vec<tx::SigPlan> = parse("plans", plans_json)?;
            out(&payload::recover_amends_planned(&t, &plans).map_err(|e| e.to_string())?)
        }
    }
    pub fn budget_table() -> R<String> {
        out(budget::table())
    }
    pub fn budget_for(role: &str) -> R<u16> {
        budget::lookup(role).map_err(|e| e.to_string())
    }
    pub fn keeper_tips() -> R<String> {
        out(defaults::tips_table())
    }
    /// The pair kinds' keeper tips per program pair (`<program of A>+<program of B>` -> tips; the golden `pairKeeperTips`).
    pub fn pair_keeper_tips() -> R<String> {
        out(defaults::pair_tips_table())
    }
    /// Default tips of a pair order whose A runs `program_a` and B `program_b` (token program names).
    pub fn pair_tips(program_a: &str, program_b: &str) -> R<String> {
        let p = |n: &str| TemplateId::from_name(n).filter(|t| t.is_token()).ok_or_else(|| format!("unknown token program {n}"));
        out(&defaults::pair_tips(p(program_a)?, p(program_b)?).map_err(|e| e.to_string())?)
    }
    /// Default tips of an order by its kind (`defaults::tips_for`): a pair order's from the pair table, any other kind's from its
    /// token program's.
    pub fn tips_for(state: &str) -> R<String> {
        out(&defaults::tips_for(&any(state)?).map_err(|e| e.to_string())?)
    }
    pub fn day_order(d0: &str, t0: &str, rate_milli: &str) -> R<String> {
        let num = |what: &str, s: &str| -> R<u64> { s.trim().parse().map_err(|e| format!("{what}: {e}")) };
        let rate = if rate_milli.trim().is_empty() { None } else { Some(num("rateMilli", rate_milli)?) };
        out(&defaults::day_order(num("d0", d0)?, num("t0", t0)?, rate))
    }
    pub fn mutable_windows(kind: &str) -> R<String> {
        let id = TemplateId::from_name(kind).ok_or_else(|| format!("unknown template {kind}"))?;
        out(&state::mutable_windows(id).to_vec())
    }
    /// Fixed-supply KCC-20 issuance: `{built, token, docs, warnings}` (see `crate::issue`).
    pub fn issue(spec: &str) -> R<String> {
        crate::issue::issue(spec)
    }
    /// Constants and rules of the issuance flow.
    pub fn issue_limits() -> R<String> {
        crate::issue::issue_limits()
    }
}

fn js<T>(r: Result<T, String>) -> Result<T, JsError> {
    r.map_err(|e| JsError::new(&e))
}

/// Version of the KOB protocol build compiled into this module.
#[wasm_bindgen]
pub fn version() -> String {
    api::version()
}

/// Verifies every embedded artifact (template-hash pinning, network constants).
#[wasm_bindgen(js_name = selfCheck)]
pub fn self_check() -> Result<(), JsError> {
    js(api::self_check())
}

/// Embedded templates: name, pinned hash, prefix/state/suffix sizes, entry dispatch tags.
#[wasm_bindgen]
pub fn templates() -> Result<String, JsError> {
    js(api::templates())
}

/// The trigger evidence a batch leg provides (touch trigger): `{"side", "tokenCovId", "scale", "price", "amount",
/// "exposedSince"}` (the numbers as decimal strings) of a plain KobAsk / KobBid leg (`amount`: base units of the fill); an error for any other leg.
#[wasm_bindgen(js_name = touchOf)]
pub fn touch_of(leg: &str) -> Result<String, JsError> {
    js(api::touch_of(leg))
}

/// Builds any action (`{"action": "createOrder" | "cancelOrder" | "cancelPosition" | "refundOrder" |
/// "sendTokens" | "batch" | "swapRoute", ...}`) into an unsigned transaction with its signing plan. Order
/// states are tagged by kind; the KRON family has its own kinds (`KobAskKron`, ...) and token programs
/// (`KronToken2433`, `KronToken2732`). A batch arms stops from plain fills of the same batch
/// (`evidence` leg indices, `updates`).
#[wasm_bindgen]
pub fn build(request: &str) -> Result<String, JsError> {
    js(api::build(request))
}

/// Checks wallet signatures and assembles the signed transaction.
#[wasm_bindgen]
pub fn finalize(built: &str, signatures: &str, options: &str) -> Result<String, JsError> {
    js(api::finalize(built, signatures, options))
}

/// Runs a signed transaction through the script engine with consensus rules.
#[wasm_bindgen]
pub fn validate(signed: &str) -> Result<String, JsError> {
    js(api::validate(signed))
}

/// Mass report (size, compute, transient, storage, fee mass) of a safe-JSON transaction.
#[wasm_bindgen]
pub fn masses(tx: &str) -> Result<String, JsError> {
    js(api::masses(tx))
}

/// Encodes an order state (`{"kind": "KobAsk", "state": {...}}`) to its state span.
#[wasm_bindgen(js_name = encodeState)]
pub fn encode_state(state: &str) -> Result<String, JsError> {
    js(api::encode_state(state))
}

/// Decodes a state span of the named template.
#[wasm_bindgen(js_name = decodeState)]
pub fn decode_state(kind: &str, hex: &str) -> Result<String, JsError> {
    js(api::decode_state(kind, hex))
}

/// Redeem script of an order state.
#[wasm_bindgen(js_name = redeemScript)]
pub fn redeem_script(state: &str) -> Result<String, JsError> {
    js(api::redeem_script(state))
}

/// P2SH script public key (kaspa string form) of an order state.
#[wasm_bindgen(js_name = scriptPublicKey)]
pub fn script_public_key(state: &str) -> Result<String, JsError> {
    js(api::script_public_key(state))
}

/// Encodes a token state of either family (KCC-20 `owner_scheme` layout or KRON `id_type` layout).
#[wasm_bindgen(js_name = encodeTokenState)]
pub fn encode_token_state(state: &str) -> Result<String, JsError> {
    js(api::encode_token_state(state))
}

/// Decodes a token state span (46 bytes: KRON, 112 bytes: KCC-20).
#[wasm_bindgen(js_name = decodeTokenState)]
pub fn decode_token_state(hex: &str) -> Result<String, JsError> {
    js(api::decode_token_state(hex))
}

/// P2SH script public key of a token state under a token program (`"KCC20Ref"`, `"KCC20Ref_8x8"`,
/// `"KronToken2433"`, `"KronToken2732"`, ...).
#[wasm_bindgen(js_name = tokenScriptPublicKey)]
pub fn token_script_public_key(program: &str, state: &str) -> Result<String, JsError> {
    js(api::token_script_public_key(program, state))
}

/// Encodes `KOB1` payload records.
#[wasm_bindgen(js_name = encodePayload)]
pub fn encode_payload(records: &str) -> Result<String, JsError> {
    js(api::encode_payload(records))
}

/// Decodes a transaction payload (`null` if it is neither `KOB1` nor legacy x402).
#[wasm_bindgen(js_name = decodePayload)]
pub fn decode_payload(hex: &str) -> Result<String, JsError> {
    js(api::decode_payload(hex))
}

/// Re-derives the orders a genesis transaction created from its `KOB1` payload (trusting nothing).
#[wasm_bindgen(js_name = recoverOrders)]
pub fn recover_orders(tx: &str) -> Result<String, JsError> {
    js(api::recover_orders(tx))
}

/// Re-derives the in-place amends (AMEND records) of a transaction: of a signed one from its order inputs'
/// signature scripts (`plans` empty), of a built one from its signing plans (`JSON.stringify(built.plans)`).
#[wasm_bindgen(js_name = recoverAmends)]
pub fn recover_amends(tx: &str, plans: &str) -> Result<String, JsError> {
    js(api::recover_amends(tx, plans))
}

/// The embedded compute-budget table.
#[wasm_bindgen(js_name = budgetTable)]
pub fn budget_table() -> Result<String, JsError> {
    js(api::budget_table())
}

/// Compute budget of one input role.
#[wasm_bindgen(js_name = budgetFor)]
pub fn budget_for(role: &str) -> Result<u16, JsError> {
    js(api::budget_for(role))
}

/// Per-program default keeper tips (`refundTip`, `keeperTip`) of the KAS kinds, derived from measured fees.
#[wasm_bindgen(js_name = keeperTips)]
pub fn keeper_tips() -> Result<String, JsError> {
    js(api::keeper_tips())
}

/// Default keeper tips of the pair kinds per program pair (`<program of A>+<program of B>` -> tips).
#[wasm_bindgen(js_name = pairKeeperTips)]
pub fn pair_keeper_tips() -> Result<String, JsError> {
    js(api::pair_keeper_tips())
}

/// Default tips (`{refundFee, refundTip, updateFee, keeperTip}`) of a pair order whose A runs `programA` and B `programB`.
#[wasm_bindgen(js_name = pairTips)]
pub fn pair_tips(program_a: &str, program_b: &str) -> Result<String, JsError> {
    js(api::pair_tips(program_a, program_b))
}

/// Default tips of an order by its kind: a pair order's from the pair table (its two token programs), any other kind's from its
/// token program's.
#[wasm_bindgen(js_name = tipsFor)]
pub fn tips_for(state: &str) -> Result<String, JsError> {
    js(api::tips_for(state))
}

/// Day order ending at the next 00:00 UTC: `{expiryDaa, deadline}` from the node's DAA score `d0`,
/// the UTC clock `t0` (unix seconds) and the measured DAA rate in milli-DAA/s (`""` = 10,000).
#[wasm_bindgen(js_name = dayOrder)]
pub fn day_order(d0: &str, t0: &str, rate_milli: &str) -> Result<String, JsError> {
    js(api::day_order(d0, t0, rate_milli))
}

/// Mutable fields of an order template and their bytecode windows (for indexers).
#[wasm_bindgen(js_name = mutableWindows)]
pub fn mutable_windows(kind: &str) -> Result<String, JsError> {
    js(api::mutable_windows(kind))
}

/// Plans a fixed-supply KCC-20 issuance (the `kob token issue` flow, reference program `KCC20Ref` in its standard 3 / 3
/// configuration; `program: "public-mint"` for the published build).
/// Takes `{name, ticker, decimals, supply, holders: [{owner, ownerScheme, amount, borrowScheme?, borrowGuard?}],
/// extensionCommitment?, carrier?, feeRate?, funding: [{transactionId, index, amount, pubkey}], changeTo?, description?,
/// icon?, website?, network?}` and returns `{built, token, docs: {supply, metadata, registryEntry}, warnings}`.
/// `built` is a normal `BuiltTx` (P2PK signing plan): sign it and pass it to `finalize` / `validate` unchanged.
#[wasm_bindgen]
pub fn issue(spec: &str) -> Result<String, JsError> {
    js(api::issue(spec))
}

/// Limits and rules of the issuance flow (maximum supply and genesis outputs, default carrier, ticker / name rules).
#[wasm_bindgen(js_name = issueLimits)]
pub fn issue_limits() -> Result<String, JsError> {
    js(api::issue_limits())
}

// ------------------------------------------------------------------------------------ numbers and defaults
//
// Integers are decimal strings. A helper returning `string | undefined` gives `undefined` exactly where the covenant's own
// arithmetic fails (an overflow, a price below the tip): the order cannot be filled that way.

/// The covenant quote rule `n * rate / scale` rounded `"up"` (what a maker receives) or `"down"` (what a maker pays).
#[wasm_bindgen]
pub fn quote(n: &str, rate: &str, scale: &str, round: &str) -> Result<Option<String>, JsError> {
    js(api::quote(n, rate, scale, round))
}

/// [`quote`] in 128-bit arithmetic (not limited to 64 bits).
#[wasm_bindgen(js_name = quoteExact)]
pub fn quote_exact(n: &str, rate: &str, scale: &str, round: &str) -> Result<Option<String>, JsError> {
    js(api::quote_exact(n, rate, scale, round))
}

/// Throws unless `scale` is a power of ten in `1..=10^9`.
#[wasm_bindgen(js_name = checkScale)]
pub fn check_scale(scale: &str) -> Result<(), JsError> {
    js(api::check_scale(scale))
}

/// Throws unless the full fill of `amount` at `rate` is worth less than 2^62 quote units.
#[wasm_bindgen(js_name = checkQuote)]
pub fn check_quote(amount: &str, rate: &str, scale: &str, what: &str) -> Result<(), JsError> {
    js(api::check_quote(amount, rate, scale, what))
}

/// The numeric gate every builder applies to an order state (scale, 2^62 bound of every rate it carries); throws on failure.
#[wasm_bindgen(js_name = checkNumbers)]
pub fn check_numbers(state: &str) -> Result<(), JsError> {
    js(api::check_numbers(state))
}

/// The minimum-fill rule of the explicit-amount kinds: `0 < n <= left`, and `n >= minFill` unless `n == left`.
#[wasm_bindgen(js_name = minFillOk)]
pub fn min_fill_ok(n: &str, left: &str, min_fill: &str) -> Result<bool, JsError> {
    js(api::min_fill_ok(n, left, min_fill))
}

/// Default `scale` (base units per whole token): `10^min(decimals, 9)`.
#[wasm_bindgen(js_name = defaultScale)]
pub fn default_scale(decimals: u32) -> String {
    api::default_scale(decimals)
}

/// Default `minFill` of a limit order: the amount worth `DEFAULT_MIN_FILL_SOMPI` at `price`, clamped to `1..=amount`.
#[wasm_bindgen(js_name = defaultMinFill)]
pub fn default_min_fill(amount: &str, price: &str, scale: &str) -> Result<String, JsError> {
    js(api::default_min_fill(amount, price, scale))
}

/// Default `minFill` of an if-done entry: `ceil(amount / 4)`.
#[wasm_bindgen(js_name = defaultMinFillIfd)]
pub fn default_min_fill_ifd(amount: &str) -> Result<String, JsError> {
    js(api::default_min_fill_ifd(amount))
}

/// Default `minFill` of a pair order (base units of A): the amount of A worth `DEFAULT_MIN_FILL_SOMPI` at `kasPerWholeA` (sompi per
/// whole A on A's KAS book; `""` = no quote: `ceil(amount / 4)`).
#[wasm_bindgen(js_name = defaultMinFillPair)]
pub fn default_min_fill_pair(amount: &str, kas_per_whole_a: &str, scale: &str) -> Result<String, JsError> {
    js(api::default_min_fill_pair(amount, kas_per_whole_a, scale))
}

/// Default `minTouch` of a stop: the larger of the order's own `minFill` and a quarter of its `amount` (at most the amount,
/// at least 1; `amount` omitted or `""`: the minimum fill).
#[wasm_bindgen(js_name = defaultMinTouch)]
pub fn default_min_touch(min_fill: &str, amount: Option<String>) -> Result<String, JsError> {
    js(api::default_min_touch(min_fill, amount.as_deref().unwrap_or("")))
}

/// `DEFAULT_MIN_FILL_SOMPI`: the quote value (sompi) a default minimum fill is worth.
#[wasm_bindgen(js_name = defaultMinFillSompi)]
pub fn default_min_fill_sompi() -> String {
    kob_protocol::defaults::DEFAULT_MIN_FILL_SOMPI.to_string()
}

/// Wallet default constants as JSON (`defaultOrderCarrier`, `defaultMinFillSompi`, `defaultMinFillImmediate`, `maxScale`,
/// `quoteLimit`, the market / stop DAA parameters).
#[wasm_bindgen(js_name = defaultConstants)]
pub fn default_constants() -> Result<String, JsError> {
    js(api::default_constants())
}

// ------------------------------------------------------------------------------------ per-kind helpers
// `state` is the order state `{"kind", "state"}` of either family; `t` the auction time and `utxoDaa` the DAA score of the
// order UTXO's block (both DAA, decimal strings).

/// Ask / bid price at auction time `t` (the constant price when not decaying / rising); a `KobPair`'s quote at `t` (B per whole A).
#[wasm_bindgen(js_name = orderPriceAt)]
pub fn order_price_at(state: &str, t: &str, utxo_daa: &str) -> Result<Option<String>, JsError> {
    js(api::order_price_at(state, t, utxo_daa))
}

/// Ask: the least the maker receives for `n` base units at auction time `t`, `ceil(n * (price(t) - tip) / scale)`.
#[wasm_bindgen(js_name = askProceeds)]
pub fn ask_proceeds(state: &str, n: &str, t: &str, utxo_daa: &str) -> Result<Option<String>, JsError> {
    js(api::ask_proceeds(state, n, t, utxo_daa))
}

/// [`ask_proceeds`] at an explicit quote `p`.
#[wasm_bindgen(js_name = askProceedsAt)]
pub fn ask_proceeds_at(state: &str, n: &str, p: &str) -> Result<Option<String>, JsError> {
    js(api::ask_proceeds_at(state, n, p))
}

/// Bid: the budget a fill of `n` consumes from the escrow, `ceil(n * (pMax + tip) / scale)`.
#[wasm_bindgen(js_name = bidUsed)]
pub fn bid_used(state: &str, n: &str) -> Result<Option<String>, JsError> {
    js(api::bid_used(state, n))
}

/// Bid: the most the maker pays for `n` base units at auction time `t`, `floor(n * (price(t) + tip) / scale)`.
#[wasm_bindgen(js_name = bidSpend)]
pub fn bid_spend(state: &str, n: &str, t: &str, utxo_daa: &str) -> Result<Option<String>, JsError> {
    js(api::bid_spend(state, n, t, utxo_daa))
}

/// [`bid_spend`] at an explicit quote `p`.
#[wasm_bindgen(js_name = bidSpendAt)]
pub fn bid_spend_at(state: &str, n: &str, p: &str) -> Result<Option<String>, JsError> {
    js(api::bid_spend_at(state, n, p))
}

/// Bid: the budget rate `pMax + tip` (sompi per whole token).
#[wasm_bindgen(js_name = bidBudgetRate)]
pub fn bid_budget_rate(state: &str) -> Result<Option<String>, JsError> {
    js(api::bid_budget_rate(state))
}

/// Bid: the remaining buying power (base units) of an escrow worth `value` sompi.
#[wasm_bindgen(js_name = bidBuyingPower)]
pub fn bid_buying_power(state: &str, value: &str) -> Result<String, JsError> {
    js(api::bid_buying_power(state, value))
}

/// Bid: whether an escrow keeping `left` sompi after a fill can continue (one more minimum fill of buying power).
#[wasm_bindgen(js_name = bidCanContinue)]
pub fn bid_can_continue(state: &str, left: &str) -> Result<bool, JsError> {
    js(api::bid_can_continue(state, left))
}

/// Bid: the escrow for `amount` base units delivered in at most `fills` fills.
#[wasm_bindgen(js_name = bidEscrow)]
pub fn bid_escrow(state: &str, amount: &str, fills: &str) -> Result<Option<String>, JsError> {
    js(api::bid_escrow(state, amount, fills))
}

/// The covenant's quantity rules for a fill of `n` of any kind (a plain bid also needs the escrow `value` in sompi; the other
/// kinds ignore it: pass `""`).
#[wasm_bindgen(js_name = fillOk)]
pub fn fill_ok(state: &str, n: &str, value: &str) -> Result<bool, JsError> {
    js(api::fill_ok(state, n, value))
}

/// Conditional ask: the least proceeds of `n` at a leg price.
#[wasm_bindgen(js_name = condAskProceeds)]
pub fn cond_ask_proceeds(state: &str, n: &str, leg_price: &str) -> Result<Option<String>, JsError> {
    js(api::cond_ask_proceeds(state, n, leg_price))
}

/// Conditional ask (repeat): the budget a take-profit of `n` returns to the entry, `ceil(n * rptPrice / scale)`.
#[wasm_bindgen(js_name = condAskRptBudget)]
pub fn cond_ask_rpt_budget(state: &str, n: &str) -> Result<Option<String>, JsError> {
    js(api::cond_ask_rpt_budget(state, n))
}

/// Conditional bid: the most paid for `n` at a leg price.
#[wasm_bindgen(js_name = condBidSpend)]
pub fn cond_bid_spend(state: &str, n: &str, leg_price: &str) -> Result<Option<String>, JsError> {
    js(api::cond_bid_spend(state, n, leg_price))
}

/// Conditional bid (repeat): the entry's proceeds of a take-profit of `n`, `ceil(n * rptPrice / scale)`.
#[wasm_bindgen(js_name = condBidRptProceeds)]
pub fn cond_bid_rpt_proceeds(state: &str, n: &str) -> Result<Option<String>, JsError> {
    js(api::cond_bid_rpt_proceeds(state, n))
}

/// Conditional bid (repeat): the prefund of `n` returned to the entry, `ceil(n * rptPrefund / scale)`.
#[wasm_bindgen(js_name = condBidRptPrefund)]
pub fn cond_bid_rpt_prefund(state: &str, n: &str) -> Result<Option<String>, JsError> {
    js(api::cond_bid_rpt_prefund(state, n))
}

/// Conditional bid: the escrow for all `amountLeft` at the worst leg in at most `fills` fills.
#[wasm_bindgen(js_name = condBidEscrow)]
pub fn cond_bid_escrow(state: &str, fills: &str) -> Result<Option<String>, JsError> {
    js(api::cond_bid_escrow(state, fills))
}

/// If-done buy entry: the most paid for `n` at quote `p`.
#[wasm_bindgen(js_name = ifdBidSpend)]
pub fn ifd_bid_spend(state: &str, n: &str, p: &str) -> Result<Option<String>, JsError> {
    js(api::ifd_bid_spend(state, n, p))
}

/// If-done buy entry: what a merge of `m` returns to the entry.
#[wasm_bindgen(js_name = ifdBidMergeBudget)]
pub fn ifd_bid_merge_budget(state: &str, m: &str) -> Result<Option<String>, JsError> {
    js(api::ifd_bid_merge_budget(state, m))
}

/// If-done buy entry: its escrow.
#[wasm_bindgen(js_name = ifdBidEscrow)]
pub fn ifd_bid_escrow(state: &str) -> Result<Option<String>, JsError> {
    js(api::ifd_bid_escrow(state))
}

/// If-done sell entry: the least proceeds of `n` at quote `p`.
#[wasm_bindgen(js_name = ifdAskProceeds)]
pub fn ifd_ask_proceeds(state: &str, n: &str, p: &str) -> Result<Option<String>, JsError> {
    js(api::ifd_ask_proceeds(state, n, p))
}

/// If-done sell entry: the prefund of `n` base units.
#[wasm_bindgen(js_name = ifdAskPrefund)]
pub fn ifd_ask_prefund(state: &str, n: &str) -> Result<Option<String>, JsError> {
    js(api::ifd_ask_prefund(state, n))
}

/// If-done sell entry: what a merge of `m` returns from an exit that sells out holding `exitValue`.
#[wasm_bindgen(js_name = ifdAskMergeSelloutBack)]
pub fn ifd_ask_merge_sellout_back(state: &str, m: &str, exit_value: &str) -> Result<Option<String>, JsError> {
    js(api::ifd_ask_merge_sellout_back(state, m, exit_value))
}

/// If-done sell entry: its value for a carrier of `carrier` sompi.
#[wasm_bindgen(js_name = ifdAskEscrow)]
pub fn ifd_ask_escrow(state: &str, carrier: &str) -> Result<Option<String>, JsError> {
    js(api::ifd_ask_escrow(state, carrier))
}

// ------------------------------------------------------------------------------------ pair orders
// `KobPair` (plain: limit, IOC / FOK, market, Dutch / rising, TWAP / DCA, close, streaming), `KobCondPair` (stop, stop-limit,
// trailing, take-profit, OCO and the exits of if-done entries), `KobIfdPair` (IFD / IFO / bracket / repeat entries). Prices are B
// base units per WHOLE A (`scale(A)` base units of A); tips, keeper tips and carriers are KAS. Orders are built with `build`
// (`createOrder`, `cancelOrder` with `prefund`, `refundOrder`, `cancelPosition`, batch legs `pair` / `condPair` / `ifdPair`).

/// The two tokens of a pair order: `{kind, side: "ask" | "bid", a, b}`, each `{covId, tplHash, prefixLen, suffixLen, family, scale,
/// ext}`.
#[wasm_bindgen(js_name = pairTokens)]
pub fn pair_tokens(state: &str) -> Result<String, JsError> {
    js(api::pair_tokens(state))
}

/// The token programs of a pair order's A and B: `{a: {covId, program}, b: {covId, program}}`.
#[wasm_bindgen(js_name = pairPrograms)]
pub fn pair_programs(state: &str) -> Result<String, JsError> {
    js(api::pair_programs(state))
}

/// The exact custodies an order of any kind holds, in record order: `[{token, amount}]` (a sell-first pair entry: its A custody
/// then its B prefund).
#[wasm_bindgen]
pub fn custodies(state: &str) -> Result<String, JsError> {
    js(api::custodies(state))
}

/// The rules every builder applies to a new order (throws with the first rule broken).
#[wasm_bindgen(js_name = checkNewOrder)]
pub fn check_new_order(state: &str) -> Result<(), JsError> {
    js(api::check_new_order(state))
}

/// The least KAS (sompi) a new order UTXO must hold.
#[wasm_bindgen(js_name = minOrderValue)]
pub fn min_order_value(state: &str) -> Result<String, JsError> {
    js(api::min_order_value(state))
}

/// S released by a fill of `n` base units of A at quote `p` (`KobPair`, `KobCondPair`): an ask `n`, a bid `floor(n * p / scale(A))`.
#[wasm_bindgen(js_name = pairSOut)]
pub fn pair_s_out(state: &str, n: &str, p: &str) -> Result<Option<String>, JsError> {
    js(api::pair_s_out(state, n, p))
}

/// The least T the maker receives for `n` at quote `p`: an ask `ceil(n * p / scale(A))` of B, a bid exactly `n` of A.
#[wasm_bindgen(js_name = pairTOutMin)]
pub fn pair_t_out_min(state: &str, n: &str, p: &str) -> Result<Option<String>, JsError> {
    js(api::pair_t_out_min(state, n, p))
}

/// The KAS tip a fill of `n` base units of A releases, `floor(n * tip / scale(A))` (every pair kind).
#[wasm_bindgen(js_name = pairTipKas)]
pub fn pair_tip_kas(state: &str, n: &str) -> Result<Option<String>, JsError> {
    js(api::pair_tip_kas(state, n))
}

/// The covenant rule of a `KobPair` fill (`n` at quote `p`, order UTXO `value` sompi): `{sOut, tOut, tipKas, rest, outAmount}`;
/// throws with the reason when the covenant refuses it.
#[wasm_bindgen(js_name = pairFill)]
pub fn pair_fill(state: &str, n: &str, p: &str, value: &str) -> Result<String, JsError> {
    js(api::pair_fill(state, n, p, value))
}

/// The largest `n` anyone can take now from a `KobPair` at quote `p` (order UTXO `value` sompi; "0": none).
#[wasm_bindgen(js_name = pairMaxTakeable)]
pub fn pair_max_takeable(state: &str, p: &str, value: &str) -> Result<String, JsError> {
    js(api::pair_max_takeable(state, p, value))
}

/// The highest quote of a `KobPair` (a rising bid's `priceEnd`, else `price`).
#[wasm_bindgen(js_name = pairPriceMax)]
pub fn pair_price_max(state: &str) -> Result<String, JsError> {
    js(api::pair_price_max(state))
}

/// A `KobPair` BID's B escrow for `amount` base units of A in at most `fills` fills.
#[wasm_bindgen(js_name = pairBidEscrow)]
pub fn pair_bid_escrow(state: &str, amount: &str, fills: &str) -> Result<Option<String>, JsError> {
    js(api::pair_bid_escrow(state, amount, fills))
}

/// KAS (sompi) a pair order UTXO needs (`KobPair` / `KobCondPair`: `fills` delivery carriers and the tip of the whole amount;
/// `KobIfdPair` ignores `fills`: its carriers per possible fill, the tip, a repeating entry's extra exit carrier).
#[wasm_bindgen(js_name = pairKasValue)]
pub fn pair_kas_value(state: &str, fills: &str) -> Result<Option<String>, JsError> {
    js(api::pair_kas_value(state, fills))
}

/// Most fills a pair order can take, `ceil(amountLeft / minFill)`.
#[wasm_bindgen(js_name = pairMaxFills)]
pub fn pair_max_fills(state: &str) -> Result<String, JsError> {
    js(api::pair_max_fills(state))
}

/// The deliveries a new `KobPair` / `KobCondPair` must fund itself; its least order value is `pairKasValue(state, pairFundedFills(state))`.
#[wasm_bindgen(js_name = pairFundedFills)]
pub fn pair_funded_fills(state: &str) -> Result<String, JsError> {
    js(api::pair_funded_fills(state))
}

/// `KobIfdPair`: the KAS each exit needs (its `exitCarrier` at least).
#[wasm_bindgen(js_name = ifdPairExitCarrierNeeded)]
pub fn ifd_pair_exit_carrier_needed(state: &str) -> Result<Option<String>, JsError> {
    js(api::ifd_pair_exit_carrier_needed(state))
}

/// `KobCondPair` leg price (`leg` "0" take-profit / limit, "1" stop) at auction time `t`; `trigger` "true" when this transaction's
/// evidence arms it.
#[wasm_bindgen(js_name = condPairLegPrice)]
pub fn cond_pair_leg_price(state: &str, leg: &str, trigger: &str, t: &str, utxo_daa: &str) -> Result<Option<String>, JsError> {
    js(api::cond_pair_leg_price(state, leg, trigger, t, utxo_daa))
}

/// `KobCondPair` bounds `{stopWorst, worst, lowest}`.
#[wasm_bindgen(js_name = condPairBounds)]
pub fn cond_pair_bounds(state: &str) -> Result<String, JsError> {
    js(api::cond_pair_bounds(state))
}

/// `KobCondPair` repeat exit: `ceil(n * rptPrice / scale(A))`.
#[wasm_bindgen(js_name = condPairRptProceeds)]
pub fn cond_pair_rpt_proceeds(state: &str, n: &str) -> Result<Option<String>, JsError> {
    js(api::cond_pair_rpt_proceeds(state, n))
}

/// `KobCondPair` repeat BID exit: the prefund returned to the entry, `ceil(n * rptPre / scale(A))`.
#[wasm_bindgen(js_name = condPairRptBack)]
pub fn cond_pair_rpt_back(state: &str, n: &str) -> Result<Option<String>, JsError> {
    js(api::cond_pair_rpt_back(state, n))
}

/// `KobCondPair` BID: the B escrow for all of `amountLeft` at the worst leg in at most `fills` fills.
#[wasm_bindgen(js_name = condPairBidEscrow)]
pub fn cond_pair_bid_escrow(state: &str, fills: &str) -> Result<Option<String>, JsError> {
    js(api::cond_pair_bid_escrow(state, fills))
}

/// Whether trigger evidence arms a pair stop (`KobCondPair`, a `KobIfdPair` stop entry). `evidence`: `{"mode": "kasBooks", "a", "b"}`
/// (KAS prices of A and B per whole token, decimal strings) or `{"mode": "pair", "price"}` (a resting `KobPair`'s quote).
#[wasm_bindgen(js_name = pairArms)]
pub fn pair_arms(state: &str, evidence: &str) -> Result<Option<bool>, JsError> {
    js(api::pair_arms(state, evidence))
}

/// The valid and maximal trailing ratchet `k` of a `KobCondPair` for the evidence (`undefined`: none).
#[wasm_bindgen(js_name = condPairTrailK)]
pub fn cond_pair_trail_k(state: &str, evidence: &str) -> Result<Option<String>, JsError> {
    js(api::cond_pair_trail_k(state, evidence))
}

/// The covenant's trailing rule for a given `k`.
#[wasm_bindgen(js_name = condPairTrailCheck)]
pub fn cond_pair_trail_check(state: &str, evidence: &str, k: &str) -> Result<bool, JsError> {
    js(api::cond_pair_trail_check(state, evidence, k))
}

/// The least B evidence fill of a two-KAS-book trigger at the order's current stop.
#[wasm_bindgen(js_name = pairMinTouchB)]
pub fn pair_min_touch_b(state: &str) -> Result<Option<String>, JsError> {
    js(api::pair_min_touch_b(state))
}

/// `KobIfdPair`: its committed exit as a `KobCondPair` state.
#[wasm_bindgen(js_name = ifdPairExit)]
pub fn ifd_pair_exit(state: &str) -> Result<String, JsError> {
    js(api::ifd_pair_exit(state))
}

/// `KobIfdPair`: the exit a fill of `n` creates holding `custody`, booked with `parent` / `until` (`parent` "" = not booked).
#[wasm_bindgen(js_name = ifdPairExitFor)]
pub fn ifd_pair_exit_for(state: &str, n: &str, custody: &str, parent: &str, until: &str) -> Result<String, JsError> {
    js(api::ifd_pair_exit_for(state, n, custody, parent, until))
}

/// The `exitState` (hex, 432 bytes) a `KobIfdPair` commits for the exit `exit` (a `KobCondPair` state).
#[wasm_bindgen(js_name = ifdPairCommitExit)]
pub fn ifd_pair_commit_exit(exit: &str) -> Result<String, JsError> {
    js(api::ifd_pair_commit_exit(exit))
}

/// `KobIfdPair` entry quote at time `t` (a stop entry's auction; the limit otherwise).
#[wasm_bindgen(js_name = ifdPairPriceAt)]
pub fn ifd_pair_price_at(state: &str, trigger: &str, t: &str, utxo_daa: &str) -> Result<Option<String>, JsError> {
    js(api::ifd_pair_price_at(state, trigger, t, utxo_daa))
}

/// `KobIfdPair` amounts of a fill of `n` at quote `p`: `{spend, proceeds, pre, mergeBudget}` (`null` where the arithmetic fails).
#[wasm_bindgen(js_name = ifdPairAmounts)]
pub fn ifd_pair_amounts(state: &str, n: &str, p: &str) -> Result<String, JsError> {
    js(api::ifd_pair_amounts(state, n, p))
}

/// `KobIfdPair`: the B custody a new entry needs.
#[wasm_bindgen(js_name = ifdPairBCustodyNeeded)]
pub fn ifd_pair_b_custody_needed(state: &str) -> Result<Option<String>, JsError> {
    js(api::ifd_pair_b_custody_needed(state))
}

/// Two-KAS-book evidence: the implied rate `a * scale(B) / b` is at most `x` (B per whole A).
#[wasm_bindgen(js_name = impliedLe)]
pub fn implied_le(a: &str, b: &str, x: &str, b_scale: &str) -> Result<Option<bool>, JsError> {
    js(api::implied_le(a, b, x, b_scale))
}

/// Two-KAS-book evidence: the implied rate is at least `x`.
#[wasm_bindgen(js_name = impliedGe)]
pub fn implied_ge(a: &str, b: &str, x: &str, b_scale: &str) -> Result<Option<bool>, JsError> {
    js(api::implied_ge(a, b, x, b_scale))
}

/// The least B evidence fill of a two-KAS-book trigger at `stop`: `ceil(minTouch * stop / scale(A))`.
#[wasm_bindgen(js_name = minTouchB)]
pub fn min_touch_b(min_touch: &str, stop: &str, a_scale: &str) -> Result<Option<String>, JsError> {
    js(api::min_touch_b(min_touch, stop, a_scale))
}

/// The two evidence modes of the pair conditionals: `[{mode, name, description}]`.
#[wasm_bindgen(js_name = pairEvidenceModes)]
pub fn pair_evidence_modes() -> Result<String, JsError> {
    js(api::pair_evidence_modes())
}

/// The trigger rule of a pair stop (which resting sides arm / trail it, thresholds); `null` without a stop.
#[wasm_bindgen(js_name = pairTriggerRule)]
pub fn pair_trigger_rule(state: &str) -> Result<String, JsError> {
    js(api::pair_trigger_rule(state))
}

// ------------------------------------------------------------------------------------------ x402

#[cfg(feature = "x402")]
mod x402_exports {
    use super::{js, x402};
    use wasm_bindgen::prelude::*;

    /// Canonical JSON (UTF-16 key order, integers only) of a JSON text.
    #[wasm_bindgen(js_name = x402Canonical)]
    pub fn canonical(json: &str) -> Result<String, JsError> {
        js(x402::canonical(json))
    }

    /// SHA-256 (hex) of UTF-8 text.
    #[wasm_bindgen(js_name = x402Sha256)]
    pub fn sha256_hex(text: &str) -> String {
        x402::sha256_hex(text)
    }

    /// `paymentRequirementsHash`: SHA-256 of the canonical JSON of the requirements.
    #[wasm_bindgen(js_name = x402RequirementsHash)]
    pub fn requirements_hash(requirements: &str) -> Result<String, JsError> {
        js(x402::requirements_hash(requirements))
    }

    /// The reference SDK request fingerprint `sha256(canonical({method,url,body|null,paymentRequirementsHash}))`; `body` is JSON text (`null` for none).
    #[wasm_bindgen(js_name = x402RequestHash)]
    pub fn request_hash(method: &str, url: &str, body: &str, requirements_hash: &str) -> Result<String, JsError> {
        js(x402::request_hash(method, url, body, requirements_hash))
    }

    /// Serialized script public key of an address (`extra.payToScriptPublicKey`).
    #[wasm_bindgen(js_name = x402AddressToSpk)]
    pub fn address_to_spk(address: &str) -> Result<String, JsError> {
        js(x402::address_to_spk(address))
    }

    /// Digest of the binding's Schnorr-signed request authorization.
    #[wasm_bindgen(js_name = x402SignedAuthDigest)]
    pub fn signed_auth_digest(request: &str) -> Result<String, JsError> {
        js(x402::signed_auth_digest(request))
    }

    /// Digest of the KOB payload-commitment authorization.
    #[wasm_bindgen(js_name = x402PayloadCommitDigest)]
    pub fn payload_commit_digest(request: &str) -> Result<String, JsError> {
        js(x402::payload_commit_digest(request))
    }

    /// `accepts` entry for a KAS `standard-native` offer.
    #[wasm_bindgen(js_name = x402NativeRequirements)]
    pub fn native_requirements(request: &str) -> Result<String, JsError> {
        js(x402::native_requirements(request))
    }

    /// `accepts` entry for a `kcc20` offer.
    #[wasm_bindgen(js_name = x402Kcc20Requirements)]
    pub fn kcc20_requirements(request: &str) -> Result<String, JsError> {
        js(x402::kcc20_requirements(request))
    }

    /// `extra.token` of a `kcc20` offer.
    #[wasm_bindgen(js_name = x402TokenOffer)]
    pub fn token_offer(request: &str) -> Result<String, JsError> {
        js(x402::token_offer(request))
    }

    /// Pinned program hash and extension commitment of a registry token.
    #[wasm_bindgen(js_name = x402ResolveToken)]
    pub fn resolve_token(network: &str, asset: &str) -> Result<String, JsError> {
        js(x402::resolve_token(network, asset))
    }

    /// `accepts` entry for a swap-and-pay offer (`extra.route`).
    #[wasm_bindgen(js_name = x402SwapRequirements)]
    pub fn swap_requirements(request: &str) -> Result<String, JsError> {
        js(x402::swap_requirements(request))
    }

    /// Builds and signs a KAS payment with a local key; returns the payload and its summary.
    #[wasm_bindgen(js_name = x402PayNative)]
    pub fn pay_native(request: &str) -> Result<String, JsError> {
        js(x402::pay_native(request))
    }

    /// Builds, signs locally and finalizes a KCC-20 payment.
    #[wasm_bindgen(js_name = x402PayKcc20)]
    pub fn pay_kcc20(request: &str) -> Result<String, JsError> {
        js(x402::pay_kcc20(request))
    }

    /// Wallet flow step 1 (KCC-20): unsigned payment `{built, template}`; the wallet signs `built.sign`.
    #[wasm_bindgen(js_name = x402BuildKcc20Unsigned)]
    pub fn build_kcc20_unsigned(request: &str) -> Result<String, JsError> {
        js(x402::build_kcc20_unsigned(request))
    }

    /// Wallet flow step 2 (KCC-20): signatures -> payload.
    #[wasm_bindgen(js_name = x402FinishKcc20)]
    pub fn finish_kcc20(built: &str, template: &str, signatures: &str) -> Result<String, JsError> {
        js(x402::finish_kcc20(built, template, signatures))
    }

    /// Wallet flow step 1 (swap-and-pay): unsigned payment; the wallet signs `built.sign`.
    #[wasm_bindgen(js_name = x402PrepareSwap)]
    pub fn prepare_swap(request: &str) -> Result<String, JsError> {
        js(x402::prepare_swap(request))
    }

    /// Wallet flow step 2 (swap-and-pay): signatures -> payload.
    #[wasm_bindgen(js_name = x402FinishSwap)]
    pub fn finish_swap(prepared: &str, signatures: &str) -> Result<String, JsError> {
        js(x402::finish_swap(prepared, signatures))
    }

    /// Builds, signs locally and assembles a swap-and-pay payment.
    #[wasm_bindgen(js_name = x402PaySwap)]
    pub fn pay_swap(request: &str) -> Result<String, JsError> {
        js(x402::pay_swap(request))
    }

    /// Payer-side verification of a built payment (the facilitator's logic); returns `{ok, ...}`.
    #[wasm_bindgen(js_name = x402Preflight)]
    pub fn preflight(request: &str) -> Result<String, JsError> {
        js(x402::preflight(request))
    }

    /// Signed self-spend that makes an unsettled payment unconfirmable.
    #[wasm_bindgen(js_name = x402IntentRequirements)]
    pub fn x402_intent_requirements(request: &str) -> Result<String, JsError> {
        js(x402::intent_requirements(request))
    }

    #[wasm_bindgen(js_name = x402PrepareIntent)]
    pub fn x402_prepare_intent(request: &str) -> Result<String, JsError> {
        js(x402::prepare_intent(request))
    }

    #[wasm_bindgen(js_name = x402FinishIntent)]
    pub fn x402_finish_intent(prepared: &str, signatures: &str) -> Result<String, JsError> {
        js(x402::finish_intent(prepared, signatures))
    }

    #[wasm_bindgen(js_name = x402PayIntent)]
    pub fn x402_pay_intent(request: &str) -> Result<String, JsError> {
        js(x402::pay_intent(request))
    }

    #[wasm_bindgen(js_name = x402CancelIntent)]
    pub fn x402_cancel_intent(request: &str) -> Result<String, JsError> {
        js(x402::cancel_intent(request))
    }

    #[wasm_bindgen(js_name = x402FinishCancel)]
    pub fn x402_finish_cancel(built: &str, signatures: &str) -> Result<String, JsError> {
        js(x402::finish_cancel(built, signatures))
    }

    #[wasm_bindgen(js_name = x402InvoiceId)]
    pub fn x402_invoice_id(invoice: &str) -> Result<String, JsError> {
        js(x402::invoice_id(invoice))
    }

    #[wasm_bindgen(js_name = x402CheckInvoice)]
    pub fn x402_check_invoice(invoice: &str, id: &str, now_ms: f64) -> Result<String, JsError> {
        js(x402::check_invoice(invoice, id, now_ms as u64))
    }

    #[wasm_bindgen(js_name = x402Revoke)]
    pub fn revoke(request: &str) -> Result<String, JsError> {
        js(x402::revoke(request))
    }

    #[wasm_bindgen(js_name = x402FinishRevoke)]
    pub fn x402_finish_revoke(prepared: &str, signatures: &str) -> Result<String, JsError> {
        js(x402::finish_revoke(prepared, signatures))
    }

    #[wasm_bindgen(js_name = x402ExpireIntent)]
    pub fn x402_expire_intent(request: &str) -> Result<String, JsError> {
        js(x402::expire_intent(request))
    }

    /// The retry step (`resend` / `rebuild` / `stop`) after a failed paid request: `{ status?, diagnostic?, retryable? }`.
    #[wasm_bindgen(js_name = x402RetryDecision)]
    pub fn x402_retry_decision(request: &str) -> Result<String, JsError> {
        js(x402::retry_decision(request))
    }

    /// Every diagnostic spelling of the x402 verifier and facilitator.
    #[wasm_bindgen(js_name = x402Diagnostics)]
    pub fn x402_diagnostics() -> Result<String, JsError> {
        js(x402::diagnostics())
    }
}

#[cfg(feature = "x402")]
pub mod x402;

#[cfg(test)]
mod tests {
    use super::api;

    #[test]
    fn version_matches_protocol_crate() {
        assert_eq!(api::version(), kob_protocol::VERSION);
    }

    /// The same checks the node test runs against the wasm build, natively.
    #[test]
    #[cfg_attr(feature = "deploy-tn10", ignore = "the golden vectors are the reference build (placeholder R_ID)")]
    fn golden_vectors_reproduce_through_the_api() {
        let text = include_str!("../../kob-protocol/vectors/golden.json");
        let g: kob_protocol::vectors::Golden = serde_json::from_str(text).unwrap();
        let s = |v: &dyn erased::Ser| v.json();
        api::self_check().unwrap();
        assert_eq!(api::templates().unwrap(), s(&g.templates));
        for v in &g.transactions {
            let built = api::build(&s(&v.request)).unwrap_or_else(|e| panic!("{}: {e}", v.name));
            assert_eq!(built, s(&v.built), "{}: built", v.name);
            let signed = api::finalize(&built, &s(&v.signatures), &s(&v.finalize)).unwrap();
            assert_eq!(signed, s(&v.signed), "{}: signed", v.name);
            api::validate(&signed).unwrap_or_else(|e| panic!("{}: {e}", v.name));
        }
        for st in &g.states {
            assert_eq!(api::encode_state(&s(&st.state)).unwrap(), kob_protocol::json::to_hex(&st.encoded));
        }
        for p in &g.payloads {
            assert_eq!(api::decode_payload(&kob_protocol::json::to_hex(&p.payload)).unwrap(), s(&Some(p.decoded.clone())));
        }
        assert_eq!(api::keeper_tips().unwrap(), s(&g.keeper_tips));
        assert_eq!(api::pair_keeper_tips().unwrap(), s(&g.pair_keeper_tips));
        for d in &g.day_orders {
            let rate = d.rate_milli.map(|r| r.to_string()).unwrap_or_default();
            assert_eq!(api::day_order(&d.d0.to_string(), &d.t0.to_string(), &rate).unwrap(), s(&d.result));
        }
    }

    fn golden_state(name: &str) -> String {
        let text = include_str!("../../kob-protocol/vectors/golden.json");
        let g: kob_protocol::vectors::Golden = serde_json::from_str(text).unwrap();
        let st = g.states.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no state vector {name}"));
        serde_json::to_string(&st.state).unwrap()
    }

    #[test]
    fn quote_rule_and_defaults() {
        // ceil / floor of n * rate / scale with the covenant's split multiplication
        assert_eq!(api::quote("1500", "250000000", "1000", "up").unwrap().as_deref(), Some("375000000"));
        assert_eq!(api::quote("1", "999", "1000", "up").unwrap().as_deref(), Some("1"));
        assert_eq!(api::quote("1", "999", "1000", "down").unwrap().as_deref(), Some("0"));
        assert_eq!(api::quote(&i64::MAX.to_string(), "2", "1", "up").unwrap(), None, "result outside i64");
        assert_eq!(api::quote_exact(&i64::MAX.to_string(), "2", "1", "up").unwrap().as_deref(), Some("18446744073709551614"));
        assert!(api::quote("1", "1", "1", "sideways").is_err());
        assert!(api::check_scale("1000").is_ok());
        assert!(api::check_scale("1500").unwrap_err().contains("power of ten"));
        assert!(api::check_quote("1000", "1000", "1000", "price").is_ok());
        assert!(api::check_quote(&i64::MAX.to_string(), "2", "1", "price").is_err());
        // defaults: the amount worth 10 KAS at the limit price, clamped to the amount
        assert_eq!(api::default_scale(8), "100000000");
        assert_eq!(api::default_scale(18), "1000000000");
        assert_eq!(api::default_min_fill("10000", "250000000", "1000").unwrap(), "4000");
        assert_eq!(api::default_min_fill("1000", "250000000", "1000").unwrap(), "1000");
        assert_eq!(api::default_min_fill_ifd("10001").unwrap(), "2501");
        assert_eq!(api::default_min_fill_pair("10000", "", "1000").unwrap(), "2500");
        assert_eq!(api::default_min_fill_pair("10000", "250000000", "1000").unwrap(), "4000");
        assert_eq!(api::default_min_touch("0", "").unwrap(), "1");
        assert_eq!(api::default_min_touch("4000", "").unwrap(), "4000");
        assert_eq!(api::default_min_touch("4000", "100000").unwrap(), "25000");
        let c: serde_json::Value = serde_json::from_str(&api::default_constants().unwrap()).unwrap();
        assert_eq!(c["defaultMinFillSompi"], "1000000000");
        assert_eq!(c["defaultOrderCarrier"], "200000000");
        assert_eq!(c["maxScale"], "1000000000");
    }

    #[test]
    fn per_kind_helpers_follow_the_protocol_methods() {
        let ask = golden_state("create.ask");
        // price 250_000_000 per 1000 base units, tip 100_000: ceil(n * 249_900_000 / 1000)
        assert_eq!(api::ask_proceeds(&ask, "1000", "0", "0").unwrap().as_deref(), Some("249900000"));
        assert_eq!(api::ask_proceeds_at(&ask, "3", "250000000").unwrap().as_deref(), Some("749700"));
        assert_eq!(api::order_price_at(&ask, "5", "7").unwrap().as_deref(), Some("250000000"));
        assert!(api::fill_ok(&ask, "1000", "").unwrap());
        assert!(!api::fill_ok(&ask, "999", "").unwrap(), "below minFill and not everything left");
        assert!(api::fill_ok(&ask, "10000", "").unwrap());
        assert!(!api::fill_ok(&ask, "10001", "").unwrap());
        // the KRON ask is the same state under another kind
        assert_eq!(api::ask_proceeds(&golden_state("kron.create.ask"), "1000", "0", "0").unwrap().as_deref(), Some("249900000"));
        let bid = golden_state("create.bid");
        // used = ceil(n * (price + tip) / scale), spend = floor(n * (price + tip) / scale)
        assert_eq!(api::bid_budget_rate(&bid).unwrap().as_deref(), Some("245100000"));
        assert_eq!(api::bid_used(&bid, "1000").unwrap().as_deref(), Some("245100000"));
        assert_eq!(api::bid_spend(&bid, "1000", "0", "0").unwrap().as_deref(), Some("245100000"));
        assert_eq!(api::bid_spend_at(&bid, "1", "245000000").unwrap().as_deref(), Some("245100"));
        // 1_000_000_000 sompi escrow, delivery carrier 1_000_000_000: nothing left to buy with
        assert_eq!(api::bid_buying_power(&bid, "1000000000").unwrap(), "0");
        let power: i64 = api::bid_buying_power(&bid, "3000000000").unwrap().parse().unwrap();
        let used: i64 = api::bid_used(&bid, &power.to_string()).unwrap().unwrap().parse().unwrap();
        assert!(used <= 2_000_000_000 && power > 0);
        assert!(api::bid_used(&bid, &(power + 1).to_string()).unwrap().unwrap().parse::<i64>().unwrap() > 2_000_000_000);
        assert!(api::fill_ok(&bid, "1000", "3000000000").unwrap());
        assert!(api::fill_ok(&bid, "1000", "").is_err(), "a bid needs the escrow value");
        assert!(api::bid_can_continue(&bid, "3000000000").unwrap());
        assert!(!api::bid_can_continue(&bid, "1000000000").unwrap());
        assert!(api::bid_escrow(&bid, "10000", "4").unwrap().is_some());
        // wrong kind
        assert!(api::bid_used(&ask, "1").unwrap_err().contains("not a KobBid"));
        assert!(api::pair_tip_kas(&ask, "1").is_err());
        let mut v: serde_json::Value = serde_json::from_str(&ask).unwrap();
        v["state"]["scale"] = "1500".into();
        assert!(api::check_numbers(&v.to_string()).unwrap_err().contains("power of ten"));
    }

    fn field(state: &str, k: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(state).unwrap();
        v["state"][k].as_str().unwrap_or_else(|| panic!("no field {k}")).to_string()
    }

    /// The pair helpers are the protocol's own arithmetic: the maker-favour quote rule in B per whole A, the custodies, the
    /// default tips of the program pair, the trigger rules.
    #[test]
    fn pair_helpers_follow_the_protocol_methods() {
        let up = |n: &str, r: &str, sc: &str| api::quote(n, r, sc, "up").unwrap();
        let down = |n: &str, r: &str, sc: &str| api::quote(n, r, sc, "down").unwrap();
        // KobPair ask: sells n of A, receives ceil(n * p / scale(A)) of B
        let pa = golden_state("pair.create.ask");
        assert_eq!(api::order_price_at(&pa, "5", "0").unwrap().as_deref(), Some("1000"));
        assert_eq!(api::pair_s_out(&pa, "1333", "1001").unwrap().as_deref(), Some("1333"));
        assert_eq!(api::pair_t_out_min(&pa, "1333", "1001").unwrap(), up("1333", "1001", "1000"));
        assert!(api::fill_ok(&pa, "1000", "").unwrap() && !api::fill_ok(&pa, "999", "").unwrap());
        let f: serde_json::Value = serde_json::from_str(&api::pair_fill(&pa, "4000", "1000", "1000000000").unwrap()).unwrap();
        assert_eq!(
            (f["sOut"].as_str(), f["tOut"].as_str(), f["rest"].as_bool(), f["outAmount"].as_str()),
            (Some("4000"), Some("4000"), Some(true), Some("6000"))
        );
        assert!(api::pair_fill(&pa, "999", "1000", "1000000000").unwrap_err().contains("minFill"));
        assert_eq!(api::pair_max_takeable(&pa, "1000", "1000000000").unwrap(), "10000");
        let t: serde_json::Value = serde_json::from_str(&api::pair_tokens(&pa).unwrap()).unwrap();
        assert_eq!(t["side"], "ask");
        assert_eq!(t["a"]["covId"], field(&pa, "sCovId"));
        assert_eq!(t["b"]["covId"], field(&pa, "tCovId"));
        let c: serde_json::Value = serde_json::from_str(&api::custodies(&pa).unwrap()).unwrap();
        assert_eq!(c, serde_json::json!([{ "token": field(&pa, "sCovId"), "amount": "10000" }]));
        api::check_new_order(&pa).unwrap();
        let p: serde_json::Value = serde_json::from_str(&api::pair_programs(&pa).unwrap()).unwrap();
        let tips = api::pair_tips(p["a"]["program"].as_str().unwrap(), p["b"]["program"].as_str().unwrap()).unwrap();
        assert_eq!(api::tips_for(&pa).unwrap(), tips);
        assert!(api::pair_tips("KobAsk", "KCC20Ref").is_err(), "not a token program");
        // KobPair bid: pays exactly floor(n * p / scale(A)) of B from its escrow, receives n of A
        let pb = golden_state("pair.create.bid");
        assert_eq!(api::pair_s_out(&pb, "1333", "1001").unwrap(), down("1333", "1001", "1000"));
        assert_eq!(api::pair_t_out_min(&pb, "1333", "1001").unwrap().as_deref(), Some("1333"));
        // the bid pays exact floors (subadditive): floor(amount * pMax / scale(A)) + 1, no slack per fill
        assert_eq!(api::pair_bid_escrow(&pb, "10000", "4").unwrap().as_deref(), Some("10001"));
        assert_eq!(api::pair_bid_escrow(&pb, "10000", "4").unwrap().as_deref(), Some(field(&pb, "custody").as_str()));
        assert_eq!(api::pair_price_max(&pb).unwrap(), "1000");
        assert!(api::min_order_value(&pb).unwrap().parse::<i64>().unwrap() > 0);
        // a resting order funds a partial fill and the fill of its rest (two deliveries)
        assert_eq!(api::pair_funded_fills(&pb).unwrap(), "2");
        assert_eq!(api::pair_kas_value(&pb, "2").unwrap(), api::min_order_value(&pb).ok());
        let mut ioc: serde_json::Value = serde_json::from_str(&pb).unwrap();
        ioc["state"]["tif"] = "1".into();
        assert_eq!(api::pair_funded_fills(&ioc.to_string()).unwrap(), "1");
        assert!(api::pair_funded_fills(&golden_state("pair.create.ifdBid")).is_err());
        assert_eq!(api::pair_max_fills(&pb).unwrap(), "10");
        // a broken escrow is refused by the new-order rules
        let mut v: serde_json::Value = serde_json::from_str(&pb).unwrap();
        v["state"]["custody"] = "9000".into();
        assert!(api::check_new_order(&v.to_string()).unwrap_err().contains("escrow"));
        // KobCondPair: legs, bounds, trigger rule in both evidence modes
        let ca = golden_state("pair.create.condAsk");
        assert_eq!(api::cond_pair_leg_price(&ca, "0", "false", "0", "0").unwrap().as_deref(), Some("1200"));
        assert_eq!(
            api::cond_pair_leg_price(&ca, "1", "true", "0", "0").unwrap().as_deref(),
            Some("1000"),
            "trigger with a band trades at the stop"
        );
        let b: serde_json::Value = serde_json::from_str(&api::cond_pair_bounds(&ca).unwrap()).unwrap();
        assert_eq!((b["stopWorst"].as_str(), b["lowest"].as_str()), (Some("970"), Some("970")));
        assert_eq!(api::pair_arms(&ca, r#"{"mode":"pair","price":"1000"}"#).unwrap(), Some(true));
        assert_eq!(api::pair_arms(&ca, r#"{"mode":"pair","price":"1001"}"#).unwrap(), Some(false));
        // mode 0: rate = a * scale(B) / b; a = 1000 sompi per whole A, b = 1000 sompi per whole B -> 1000 B per whole A
        assert_eq!(api::pair_arms(&ca, r#"{"mode":"kasBooks","a":"1000","b":"1000"}"#).unwrap(), Some(true));
        assert_eq!(api::pair_arms(&ca, r#"{"mode":"kasBooks","a":"1001","b":"1000"}"#).unwrap(), Some(false));
        assert_eq!(api::pair_min_touch_b(&ca).unwrap(), up("1000", "1000", "1000"));
        let r: serde_json::Value = serde_json::from_str(&api::pair_trigger_rule(&ca).unwrap()).unwrap();
        assert_eq!(r["direction"], "fallsTo");
        assert_eq!(r["arm"], serde_json::json!({ "kasBooks": { "a": "ask", "b": "bid" }, "pair": "ask" }));
        assert!(r["trail"].is_null());
        let cb = golden_state("pair.create.condBid");
        let r: serde_json::Value = serde_json::from_str(&api::pair_trigger_rule(&cb).unwrap()).unwrap();
        assert_eq!(r["direction"], "risesTo");
        assert_eq!(r["arm"], serde_json::json!({ "kasBooks": { "a": "bid", "b": "ask" }, "pair": "bid" }));
        assert_eq!(api::cond_pair_bid_escrow(&cb, "10").unwrap().as_deref(), Some(field(&cb, "custody").as_str()));
        let mut v: serde_json::Value = serde_json::from_str(&ca).unwrap();
        v["state"]["trailStep"] = "10".into();
        v["state"]["trailGap"] = "5".into();
        let trailing = v.to_string();
        let r: serde_json::Value = serde_json::from_str(&api::pair_trigger_rule(&trailing).unwrap()).unwrap();
        assert_eq!(r["trail"]["direction"], "up");
        assert_eq!(r["trail"]["pair"], "bid");
        // stop 1000, step 10, gap 5: a pair BID at 1036 justifies k = 3 (1030 + 5 <= 1036, 1040 + 5 > 1036)
        assert_eq!(api::cond_pair_trail_k(&trailing, r#"{"mode":"pair","price":"1036"}"#).unwrap().as_deref(), Some("3"));
        assert!(api::cond_pair_trail_check(&trailing, r#"{"mode":"pair","price":"1036"}"#, "3").unwrap());
        assert!(!api::cond_pair_trail_check(&trailing, r#"{"mode":"pair","price":"1036"}"#, "2").unwrap());
        assert!(api::pair_arms(&ca, r#"{"mode":"other"}"#).is_err());
        // KobIfdPair: the committed exit round-trips, the exit of a fill, the amounts, the custodies of a sell-first entry
        let ib = golden_state("pair.create.ifdBid");
        let exit = api::ifd_pair_exit(&ib).unwrap();
        assert_eq!(api::ifd_pair_commit_exit(&exit).unwrap(), field(&ib, "exitState"));
        let x: serde_json::Value = serde_json::from_str(&api::ifd_pair_exit_for(&ib, "4000", "4000", "", "0").unwrap()).unwrap();
        assert_eq!(
            (x["state"]["amountLeft"].as_str(), x["state"]["custody"].as_str(), x["state"]["side"].as_str()),
            (Some("4000"), Some("4000"), Some("1"))
        );
        let parent = "05".repeat(32);
        let x: serde_json::Value = serde_json::from_str(&api::ifd_pair_exit_for(&ib, "4000", "4000", &parent, "77").unwrap()).unwrap();
        assert_eq!(
            (x["state"]["parent"].as_str(), x["state"]["rptPrice"].as_str(), x["state"]["rptUntil"].as_str()),
            (Some(parent.as_str()), Some("1000"), Some("77"))
        );
        let a: serde_json::Value = serde_json::from_str(&api::ifd_pair_amounts(&ib, "1333", "1001").unwrap()).unwrap();
        assert_eq!(a["spend"].as_str().map(String::from), down("1333", "1001", "1000"));
        assert_eq!(a["proceeds"].as_str().map(String::from), up("1333", "1001", "1000"));
        assert_eq!(api::ifd_pair_b_custody_needed(&ib).unwrap().as_deref(), Some("10000"));
        let need: i64 = api::ifd_pair_exit_carrier_needed(&ib).unwrap().unwrap().parse().unwrap();
        assert!(need > 0 && need <= field(&ib, "exitCarrier").parse::<i64>().unwrap(), "the golden entry funds its exits");
        assert_eq!(api::ifd_pair_price_at(&ib, "false", "0", "0").unwrap().as_deref(), Some("1000"));
        assert_eq!(api::pair_trigger_rule(&ib).unwrap(), "null", "a limit entry has no trigger");
        let ia = golden_state("pair.create.ifdAsk");
        let c: Vec<serde_json::Value> = serde_json::from_str(&api::custodies(&ia).unwrap()).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!((c[0]["token"].as_str(), c[0]["amount"].as_str()), (Some(field(&ia, "aCovId").as_str()), Some("10000")));
        assert_eq!((c[1]["token"].as_str(), c[1]["amount"].as_str()), (Some(field(&ia, "bCovId").as_str()), Some("2009")));
        assert_eq!(api::ifd_pair_b_custody_needed(&ia).unwrap().as_deref(), Some("2009"));
        let a: serde_json::Value = serde_json::from_str(&api::ifd_pair_amounts(&ia, "1001", "1000").unwrap()).unwrap();
        assert_eq!(a["pre"].as_str().map(String::from), up("1001", "200", "1000"));
        let t: serde_json::Value = serde_json::from_str(&api::pair_tokens(&ia).unwrap()).unwrap();
        assert_eq!(t["side"], "ask");
        // evidence helpers
        assert_eq!(api::implied_le("1000", "1000", "1000", "1000").unwrap(), Some(true));
        assert_eq!(api::implied_ge("999", "1000", "1000", "1000").unwrap(), Some(false));
        assert_eq!(api::implied_le("1", "0", "1", "1000").unwrap(), None);
        assert_eq!(api::min_touch_b("1000", "1500", "1000").unwrap().as_deref(), Some("1500"));
        let m: Vec<serde_json::Value> = serde_json::from_str(&api::pair_evidence_modes().unwrap()).unwrap();
        assert_eq!(m.iter().map(|x| x["name"].as_str().unwrap()).collect::<Vec<_>>(), ["kasBooks", "pair"]);
        // wrong kinds
        assert!(api::pair_fill(&ca, "1", "1", "1").unwrap_err().contains("not a KobPair"));
        assert!(api::ifd_pair_exit(&pa).is_err());
        assert!(api::pair_tokens(&golden_state("create.ask")).is_err());
    }

    mod erased {
        pub trait Ser {
            fn json(&self) -> String;
        }
        impl<T: serde::Serialize> Ser for T {
            fn json(&self) -> String {
                serde_json::to_string(self).unwrap()
            }
        }
    }
}
