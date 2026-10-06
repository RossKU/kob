#!/usr/bin/env bash
# Generator of contracts/argent/kob_router.ag (docs/argent.md, "The router generator").
#
#   tools/router-gen/gen-router.sh            write contracts/argent/kob_router.ag
#   tools/router-gen/gen-router.sh --check    fail (exit 1) if the committed file differs; writes nothing
#   tools/router-gen/gen-router.sh FILE       write FILE
#
# Argent has no macros and an `observes` group has an exact shape, so every fill shape of an intent
# is its own actor (one template, one small script): the same pattern at different widths. This
# script writes that pattern out; `router_head.ag` (states, helper functions, the reviewed rules
# in prose) is copied verbatim. The generated .ag is committed, and the build fails if it is not
# what this script writes. Pure bash, LF output.
#
# PIN=1 (or PIN=ask / PIN=bid) adds the continuation pin of every resting order (both sides, or only
# resting asks / only resting KCC-20 bids) for measurements; the shipped router has none
# (docs/argent-feedback.md, item 6).
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
TARGET=$ROOT/contracts/argent/kob_router.ag
CHECK=0
case "${1:-}" in
  --check) CHECK=1; OUT=$(mktemp) ;;
  -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
  "") OUT=$TARGET ;;
  *) OUT=$1 ;;
esac

TOK='byte[32](token_covid)'

# Continuation pins of resting orders (PIN, see the header): off in the shipped router.
pin_ask() { [ "${PIN:-0}" = 1 ] || [ "${PIN:-0}" = ask ]; }
pin_bid() { [ "${PIN:-0}" = 1 ] || [ "${PIN:-0}" = bid ]; }

# Family of token A (the token a token intent locks and sells): kcc20 (KCC20State, KobBid) or kron
# (KronTokenState, KobBidKron). Token B (bought from KobAsks, delivered to the merchant) is always KCC-20.
# Every token is observed under the intent's open handle (self.token_type, self.token_a_type, self.token_b_type).
bid_type() { if [ "$1" = kron ]; then echo "KOBOrdersKron::KobBidKron"; else echo "KOBOrders::KobBid"; fi; }
# The lock check of token A. $1 = family, $2 = projection of the observed lock input.
own_lock() {
  if [ "$1" = kron ]; then echo "        own_kron($2.owner, $2.id_type, $2.is_minter, byte[32](self.cov_id));";
  else echo "        own_tokens($2.owner, $2.owner_scheme, $2.borrow_scheme, byte[32](self.cov_id));"; fi
  lock_pin "$1" "$2"
}
# The lock pin (router_head.ag, "LOCK PIN"): the observed lock is the one the payer created. $1 = family, $2 = projection.
lock_pin() {
  echo "        require($2.amount == lock_amount);"
  [ "$1" = kron ] || echo "        require($2.extension_commitment == lock_extension);"
}
# A token-A output state held by a key. $1 = family, $2 = local name, $3 = owner, $4 = amount, $5 = extension (KCC-20).
a_lit() { if [ "$1" = kron ]; then kron_lit "$2" "$3" "$4"; else tok_lit "$2" "$3" "$4" "$5"; fi; }

# ---- projections of the ask/bid of group $1 (handle `ask` / `bid`)
ask_args() { local g=$1; echo "$g.inputs.ask.tokenCovId, $g.inputs.ask.scale, $g.inputs.ask.amountLeft, $g.inputs.ask.price"; }
ask_args_np() { local g=$1; echo "$g.inputs.ask.tokenCovId, $g.inputs.ask.amountLeft"; }

ask_next_literal() { # $1 = group of the resting ask, $2 = take expression
  local g=$1 t=$2 a="$1.inputs.ask"
  cat <<EOT
        KobAskState next_a = KobAskState {
            maker: $a.maker,
            tokenCovId: $a.tokenCovId,
            tokenTplHash: $a.tokenTplHash,
            tplPrefixLen: $a.tplPrefixLen,
            tplSuffixLen: $a.tplSuffixLen,
            scale: $a.scale,
            minFill: $a.minFill,
            price: $a.price,
            tip: $a.tip,
            tif: $a.tif,
            activeFrom: $a.activeFrom,
            expiryDaa: $a.expiryDaa,
            refundTip: $a.refundTip,
            interval: $a.interval,
            maxFill: $a.maxFill,
            slope: $a.slope,
            priceEnd: $a.priceEnd,
            decayStep: $a.decayStep,
            amountLeft: $a.amountLeft - $t,
        };
EOT
}

ask_obs() { # $1 = group name, $2 = covid expr, $3 = rest|out
  echo "    observes $1 by $2 {"
  echo "        inputs {"
  echo "            ask: KOBOrders::KobAsk,"
  echo "        }"
  echo "        outputs {"
  if [ "$3" = rest ]; then echo "            next: KOBOrders::KobAsk,"; fi
  echo "        }"
  echo "    }"
}
bid_obs() { # $1 = group name, $2 = covid expr, $3 = rest|out, $4 = family of the bid's token
  local t; t=$(bid_type "$4")
  echo "    observes $1 by $2 {"
  echo "        inputs {"
  echo "            bid: $t,"
  echo "        }"
  echo "        outputs {"
  if [ "$3" = rest ]; then echo "            next: $t,"; fi
  echo "        }"
  echo "    }"
}

join() { local IFS="$1"; shift; echo "$*"; }

# Every entry: the intent is the LAST input (output me + 1 is then no input's positional slot).
prologue() {
  echo "        require(tx.inputs.length == me + 1);"
}
# The observed ask $1 runs its fill (n > 0 in the 8-byte first push).
filled() {
  echo "        require(OpTxInputScriptSigSubstr(OpCovInputIdx($1, 0), 0, 1) == byte[](0x08));"
  echo "        require(int(byte[8](OpTxInputScriptSigSubstr(OpCovInputIdx($1, 0), 1, 9))) > 0);"
}

tok_lit() { # $1 = local name, $2 = owner expr, $3 = amount expr, $4 = extension_commitment expr
  cat <<EOT
        KCC20State $1 = {
            amount: $3,
            owner: $2,
            owner_scheme: OWNER_P2PK_SCHNORR,
            borrow_scheme: BORROW_DISABLED,
            borrow_guard: ZERO32,
            extension_commitment: $4,
        };
EOT
}

kron_lit() { # $1 = local name, $2 = owner expr, $3 = amount expr: a KRON UTXO held by address presence
  cat <<EOT
        KronTokenState $1 = {
            owner: $2,
            id_type: KRON_ID_ADDRESS,
            amount: $3,
            is_minter: 0x00,
        };
EOT
}

esc_change() { # $1 = local name, $2 = escrow projection, $3 = take expression
  cat <<EOT
        KCC20State $1 = {
            amount: $2.amount - $3,
            owner: $2.owner,
            owner_scheme: OWNER_COVENANT_ID,
            borrow_scheme: $2.borrow_scheme,
            borrow_guard: $2.borrow_guard,
            extension_commitment: $2.extension_commitment,
        };
EOT
}

# ---------------------------------------------------------------- KasToToken
k2t_entry() { # $1 = k, $2 = rest|out
  local k=$1 last=$2 name m E=token.inputs.escrow M=token.inputs.mine
  name=buy; [ "$k" -gt 1 ] && name=buy$k; [ "$last" = out ] && name=${name}_out
  local args=()
  for m in $(seq 1 "$k"); do args+=("cov_id ask${m}_covid"); done
  echo
  if [ "$k" = 1 ] && [ "$last" = rest ]; then echo "    // One ask, RESTING: the ask continues with amountLeft - amount.";
  elif [ "$k" = 1 ]; then echo "    // One ask, SOLD OUT: the merchant gets the ask's whole custody, the ask ends.";
  elif [ "$last" = rest ]; then echo "    // $k asks: the first $((k-1)) SOLD OUT, the last RESTING.";
  else echo "    // $k asks, all SOLD OUT."; fi
  echo "    entry $name($(join , "${args[@]}" | sed 's/,/, /g'))"
  for m in $(seq 1 "$k"); do
    if [ "$m" = "$k" ]; then ask_obs "book$m" "ask${m}_covid" "$last"; else ask_obs "book$m" "ask${m}_covid" out; fi
  done
  echo "    observes token by self.token_covid {"
  echo "        inputs {"
  for m in $(seq 1 "$k"); do echo "            escrow$m: self.token_type,"; done
  echo "        }"
  echo "        outputs {"
  [ "$last" = rest ] && echo "            change: self.token_type,"
  echo "            merchant: self.token_type,"
  echo "        }"
  echo "    }"
  echo "    emits none {"
  echo "        int me = this.activeInputIndex;"
  prologue
  for m in $(seq 1 "$k"); do filled "ask${m}_covid"; done
  local prev="" sum
  # takes
  for m in $(seq 1 "$k"); do
    if [ "$m" -lt "$k" ] || [ "$last" = out ]; then
      TK[$m]="$E$m.amount"
    else
      local expr="amount"
      for j in $(seq 1 $((k-1))); do expr="$expr - $E$j.amount"; done
      echo "        int take$m = $expr;"; TK[$m]="take$m"
    fi
  done
  if [ "$last" = out ]; then
    sum="${E}1.amount"; for j in $(seq 2 "$k"); do sum="$sum + $E$j.amount"; done
    echo "        require($sum == amount);"
  else
    echo "        require(take$k < $E$k.amount);"
    echo "        require(book$k.inputs.ask.tif == 0);"
  fi
  for m in $(seq 1 "$k"); do
    echo "        int pay$m = ask_leg($(ask_args book$m), $E$m.owner, $E$m.owner_scheme, $E$m.amount, byte[32](ask${m}_covid), $TOK, ${TK[$m]});"
  done
  sum="pay1"; for j in $(seq 2 "$k"); do sum="$sum + pay$j"; done
  echo "        int pay = $sum;"
  echo "        require(pay <= max_pay);"
  echo
  echo "        require(tx.outputs[me].scriptPubKey == byte[](new ScriptPubKeyP2PK(payer)));"
  echo "        require(tx.outputs[me].value + pay + max_extra >= self.value);"
  if [ "$last" = rest ]; then echo "        require(OpCovOutputIdx(token_covid, 1) == me + 1);"; else echo "        require(OpCovOutputIdx(token_covid, 0) == me + 1);"; fi
  echo
  if [ "$last" = rest ]; then
    if pin_ask; then ask_next_literal "book$k" "take$k"; fi
    esc_change change_state "$E$k" "take$k"
  fi
  tok_lit merchant_state merchant amount "${E}1.extension_commitment"
  if [ "$last" = rest ] && pin_ask; then
    echo "        require book$k.outputs become {"
    echo "            next <- KOBOrders::KobAsk(next_a),"
    echo "        };"
  fi
  echo "        require token.outputs become {"
  [ "$last" = rest ] && echo "            change <- self.token_type(change_state),"
  echo "            merchant <- self.token_type(merchant_state),"
  echo "        };"
  echo "    }"
}

# ---------------------------------------------------------------- TokenToKas
t2k_entry() { # $1 = k, $2 = rest|out, $3 = family of token A (kcc20|kron)
  local k=$1 last=$2 fam=$3 name m E=token.inputs.escrow M=token.inputs.mine
  name=sell; [ "$k" -gt 1 ] && name=sell$k; [ "$last" = out ] && name=${name}_out
  local args=()
  for m in $(seq 1 "$k"); do args+=("cov_id bid${m}_covid"); done
  for m in $(seq 1 "$k"); do args+=("int n$m"); done
  echo
  if [ "$k" = 1 ] && [ "$last" = rest ]; then echo "    // One bid, RESTING: n1 base units go into it and it continues.";
  elif [ "$k" = 1 ]; then echo "    // One bid, ENDING (exhausted, IOC or FOK): n1 base units go into it.";
  elif [ "$last" = rest ]; then echo "    // $k bids: the first $((k-1)) END, the last RESTS. n1 .. n$k base units, at most max_sell in all.";
  else echo "    // $k bids, all ENDING. n1 .. n$k base units, at most max_sell in all."; fi
  echo "    entry $name($(join , "${args[@]}" | sed 's/,/, /g'))"
  for m in $(seq 1 "$k"); do
    if [ "$m" = "$k" ]; then bid_obs "book$m" "bid${m}_covid" "$last" "$fam"; else bid_obs "book$m" "bid${m}_covid" out "$fam"; fi
  done
  echo "    observes token by self.token_covid {"
  echo "        inputs {"
  echo "            mine: self.token_type,"
  echo "        }"
  echo "        outputs {"
  for m in $(seq 1 "$k"); do echo "            delivery$m: self.token_type,"; done
  echo "            change: self.token_type,"
  echo "        }"
  echo "    }"
  echo "    emits none {"
  echo "        int me = this.activeInputIndex;"
  prologue
  own_lock "$fam" "$M"
  for m in $(seq 1 "$k"); do
    echo "        bid_fits(book$m.inputs.bid.tokenCovId, $TOK, n$m);"
  done
  [ "$last" = rest ] && echo "        require(book$k.inputs.bid.tif == 0);"
  local sum="n1"; for j in $(seq 2 "$k"); do sum="$sum + n$j"; done
  echo "        int sold = $sum;"
  echo "        require(sold <= max_sell);"
  echo "        require(sold <= $M.amount);"
  echo
  echo "        require(tx.outputs[me].scriptPubKey == byte[](new ScriptPubKeyP2PK(merchant)));"
  echo "        require(tx.outputs[me].value >= merchant_kas);"
  echo "        require(OpCovOutputIdx(token_covid, $k) == me + 1);"
  echo
  for m in $(seq 1 "$k"); do
    a_lit "$fam" "delivery${m}_state" "byte[32](book$m.inputs.bid.maker)" "n$m" "book$m.inputs.bid.extensionCommitment"
  done
  a_lit "$fam" change_state "byte[32](payer)" "$M.amount - sold" "$M.extension_commitment"
  if [ "$last" = rest ] && pin_bid && [ "$fam" = kcc20 ]; then
    echo "        KobBidState next_b = state(book$k.inputs.bid);"
    echo "        require book$k.outputs become {"
    echo "            next <- KOBOrders::KobBid(next_b),"
    echo "        };"
  fi
  echo "        require token.outputs become {"
  for m in $(seq 1 "$k"); do echo "            delivery$m <- self.token_type(delivery${m}_state),"; done
  echo "            change <- self.token_type(change_state),"
  echo "        };"
  echo "    }"
}

# ---------------------------------------------------------------- TokenSwap
swap_entry() { # $1 = name, $2 = kb (bids), $3 = bid last rest|out, $4 = ka (asks), $5 = ask last rest|out, $6 = family of token A
  local name=$1 kb=$2 bl=$3 ka=$4 al=$5 fam=$6 m E=token_b_group.inputs.escrow M=token_a_group.inputs.mine
  local args=()
  for m in $(seq 1 "$kb"); do args+=("cov_id bid${m}_covid"); done
  for m in $(seq 1 "$ka"); do args+=("cov_id ask${m}_covid"); done
  for m in $(seq 1 "$kb"); do args+=("int n_a$m"); done
  echo
  echo "    // $kb bid(s) (the last one $( [ "$bl" = rest ] && echo RESTING || echo ENDING ), the others END) and $ka ask(s) (the last one $( [ "$al" = rest ] && echo RESTING || echo SOLD OUT ), the others SOLD OUT)."
  echo "    entry $name($(join , "${args[@]}" | sed 's/,/, /g'))"
  for m in $(seq 1 "$kb"); do
    if [ "$m" = "$kb" ]; then bid_obs "bids$m" "bid${m}_covid" "$bl" "$fam"; else bid_obs "bids$m" "bid${m}_covid" out "$fam"; fi
  done
  for m in $(seq 1 "$ka"); do
    if [ "$m" = "$ka" ]; then ask_obs "asks$m" "ask${m}_covid" "$al"; else ask_obs "asks$m" "ask${m}_covid" out; fi
  done
  echo "    observes token_a_group by self.token_a {"
  echo "        inputs {"
  echo "            mine: self.token_a_type,"
  echo "        }"
  echo "        outputs {"
  for m in $(seq 1 "$kb"); do echo "            delivery$m: self.token_a_type,"; done
  echo "            change: self.token_a_type,"
  echo "        }"
  echo "    }"
  echo "    observes token_b_group by self.token_b {"
  echo "        inputs {"
  for m in $(seq 1 "$ka"); do echo "            escrow$m: self.token_b_type,"; done
  echo "        }"
  echo "        outputs {"
  [ "$al" = rest ] && echo "            escrow_change: self.token_b_type,"
  echo "            merchant_out: self.token_b_type,"
  echo "        }"
  echo "    }"
  # Stack-lean body: the swaps observe four orders and two token groups under two handles, and `swap2` is the tightest
  # script (227 live bindings and 234 combined stack items of the 244 allowed on the v2.6 router; the lock pin added
  # two state fields and two checks, and argentc, which refuses an entry over the limit, still compiles it). So no local
  # is kept that an expression can stand for: the input index (this.activeInputIndex), the sum of the base units sold
  # into the bids and the take of a resting last ask are written out where they are used.
  local ME=this.activeInputIndex
  echo "    emits none {"
  echo "        require(tx.inputs.length == $ME + 1);"
  for m in $(seq 1 "$ka"); do filled "ask${m}_covid"; done
  echo
  echo "        require(token_a != token_b);"
  own_lock "$fam" "$M"
  for m in $(seq 1 "$kb"); do
    echo "        bid_fits(bids$m.inputs.bid.tokenCovId, byte[32](token_a), n_a$m);"
    SD[$m]="n_a$m"
  done
  [ "$bl" = rest ] && echo "        require(bids$kb.inputs.bid.tif == 0);"
  local sum="${SD[1]}"; for j in $(seq 2 "$kb"); do sum="$sum + ${SD[$j]}"; done
  echo "        require($sum <= max_sell_a);"
  echo "        require($sum <= $M.amount);"
  # buy side
  for m in $(seq 1 "$ka"); do
    if [ "$m" -lt "$ka" ] || [ "$al" = out ]; then
      TK[$m]="$E$m.amount"
    else
      local expr="amount_b"
      for j in $(seq 1 $((ka-1))); do expr="$expr - $E$j.amount"; done
      [ "$ka" -gt 1 ] && expr="($expr)"
      TK[$m]="$expr"
    fi
  done
  if [ "$al" = out ]; then
    local bsum="${E}1.amount"; for j in $(seq 2 "$ka"); do bsum="$bsum + $E$j.amount"; done
    echo "        require($bsum == amount_b);"
  else
    echo "        require(${TK[$ka]} < $E$ka.amount);"
    echo "        require(asks$ka.inputs.ask.tif == 0);"
  fi
  for m in $(seq 1 "$ka"); do
    echo "        ask_fits($(ask_args_np asks$m), $E$m.owner, $E$m.owner_scheme, $E$m.amount, byte[32](ask${m}_covid), byte[32](token_b), ${TK[$m]});"
  done
  echo
  if [ "$al" = rest ]; then echo "        require(OpCovOutputIdx(token_b, 1) == $ME);"; else echo "        require(OpCovOutputIdx(token_b, 0) == $ME);"; fi
  echo "        require(OpCovOutputIdx(token_a, $kb) == $ME + 1);"
  echo
  if [ "$al" = rest ]; then
    if pin_ask; then ask_next_literal "asks$ka" "${TK[$ka]}"; fi
    esc_change b_change "$E$ka" "${TK[$ka]}"
  fi
  for m in $(seq 1 "$kb"); do
    a_lit "$fam" "a_delivery$m" "byte[32](bids$m.inputs.bid.maker)" "${SD[$m]}" "bids$m.inputs.bid.extensionCommitment"
  done
  a_lit "$fam" a_change "byte[32](payer)" "$M.amount - ($sum)" "$M.extension_commitment"
  tok_lit b_merchant merchant amount_b "${E}1.extension_commitment"
  if [ "$bl" = rest ] && pin_bid && [ "$fam" = kcc20 ]; then
    echo "        KobBidState next_b = state(bids$kb.inputs.bid);"
    echo "        require bids$kb.outputs become {"
    echo "            next <- KOBOrders::KobBid(next_b),"
    echo "        };"
  fi
  if [ "$al" = rest ] && pin_ask; then
    echo "        require asks$ka.outputs become {"
    echo "            next <- KOBOrders::KobAsk(next_a),"
    echo "        };"
  fi
  echo "        require token_a_group.outputs become {"
  for m in $(seq 1 "$kb"); do echo "            delivery$m <- self.token_a_type(a_delivery$m),"; done
  echo "            change <- self.token_a_type(a_change),"
  echo "        };"
  echo "        require token_b_group.outputs become {"
  [ "$al" = rest ] && echo "            escrow_change <- self.token_b_type(b_change),"
  echo "            merchant_out <- self.token_b_type(b_merchant),"
  echo "        };"
  echo "    }"
}

# After the deadline anyone may return the intent to the payer (router_head.ag, "DEADLINE").
# $1 = intent (KasToToken | TokenToKas | TokenSwap), $2 = family of token A.
expire_entry() {
  local grp=token cov=token_covid h=self.token_type
  [ "$1" = TokenSwap ] && { grp=token_a_group; cov=token_a; h=self.token_a_type; }
  echo
  if [ "$1" = KasToToken ]; then
    echo "    // From the deadline on, anyone: the intent's KAS back to the payer at out j, less at most EXPIRE_MAX_FEE."
    echo "    entry expire() emits none {"
  else
    echo "    // From the deadline on, anyone: the intent's KAS back to the payer at out j, less at most EXPIRE_MAX_FEE,"
    echo "    // and the locked tokens, whole and with their whole carrier, to the payer's key at out j + 1. Input j + 1 is"
    echo "    // those tokens, so no other input's positional rule can claim out j + 1."
    echo "    entry expire()"
    lock_obs "$grp" "$cov" "$h"
    echo "    emits none {"
  fi
  echo "        int me = this.activeInputIndex;"
  echo "        require(tx.time >= deadline);"
  echo "        require(tx.outputs[me].scriptPubKey == byte[](new ScriptPubKeyP2PK(payer)));"
  echo "        require(tx.outputs[me].value + EXPIRE_MAX_FEE >= self.value);"
  if [ "$1" != KasToToken ]; then
    local M=$grp.inputs.mine
    echo "        require(OpCovInputIdx($cov, 0) == me + 1);"
    echo "        require(OpCovOutputIdx($cov, 0) == me + 1);"
    echo "        require(tx.outputs[me + 1].value >= $M.value);"
    lock_back "$grp" "$2" "$h"
  fi
  echo "    }"
}

# The observed lock of a token intent: the one token input (owned by this intent) and the one token output,
# both under the intent's handle of token A. $1 = group, $2 = token covid field, $3 = handle.
lock_obs() {
  echo "    observes $1 by self.$2 {"
  echo "        inputs {"
  echo "            mine: $3,"
  echo "        }"
  echo "        outputs {"
  echo "            back: $3,"
  echo "        }"
  echo "    }"
}

# The lock goes back whole to the payer's key. $1 = group, $2 = family, $3 = handle.
lock_back() {
  local M=$1.inputs.mine
  own_lock "$2" "$M"
  a_lit "$2" back_state "byte[32](payer)" "$M.amount" "$M.extension_commitment"
  echo "        require $1.outputs become {"
  echo "            back <- $3(back_state),"
  echo "        };"
}

# $1 = intent, $2 = family of token A.
cancel_entry() {
  local grp=token cov=token_covid h=self.token_type
  [ "$1" = TokenSwap ] && { grp=token_a_group; cov=token_a; h=self.token_a_type; }
  echo
  if [ "$1" = KasToToken ]; then
    echo "    entry cancel(sig s) emits none {"
    echo "        require_cancel_sig(s, payer);"
  else
    local M=$grp.inputs.mine
    echo "    // The payer, any time. Input j + 1 must be the lock, the one token input of the locked token, read under the"
    echo "    // intent's handle: owned by this intent's id. The cancel ends the intent covenant, and tokens owned by its id"
    echo "    // could never move afterwards. Where they go (the one token output) is the payer's signed choice (SIGHASH_ALL)."
    echo "    entry cancel(sig s)"
    lock_obs "$grp" "$cov" "$h"
    echo "    emits none {"
    echo "        require_cancel_sig(s, payer);"
    echo "        require(OpCovInputIdx($cov, 0) == this.activeInputIndex + 1);"
    echo "        require($M.owner == byte[32](self.cov_id));"
    if [ "$2" = kron ]; then
      echo "        require($M.id_type == KRON_ID_COVENANT);"
    else
      echo "        require($M.owner_scheme == OWNER_COVENANT_ID);"
    fi
    lock_pin "$2" "$M"
  fi
  echo "    }"
  echo "}"
}

k2t_name() { local n=buy; [ "$1" -gt 1 ] && n=buy$1; [ "$2" = out ] && n=${n}_out; echo "$n"; }
t2k_name() { local n=sell; [ "$1" -gt 1 ] && n=sell$1; [ "$2" = out ] && n=${n}_out; echo "$n"; }

ACTORS=()
TK=()
SD=()
SWAP_SHAPES="swap 1 rest 1 rest
swap_bid_out 1 out 1 rest
swap_ask_out 1 rest 1 out
swap_out 1 out 1 out
swap2 2 rest 2 rest
swap2_out 2 out 2 out"
# One actor (= one template = one small script) per intent shape: its entry, expire and cancel.
# $1 = actor prefix, $2 = intent (KasToToken | TokenToKas | TokenSwap), $3 = family of token A, $4 = state,
# $5 = shape (= entry name); entry text on stdin.
emit_actor() {
  echo "actor ${1}_${5} owns ${4} {"
  sed '1{/^$/d}'
  expire_entry "$2" "$3"
  cancel_entry "$2" "$3"
  echo
  ACTORS+=("${1}_${5}")
}

{
  cat "$HERE/router_head.ag"
  echo "// ---------------------------------------------------------------- KasToToken"
  echo
  echo "// Tx shape (all buy shapes): [.., ask_1 .. ask_k, escrow_1 .. escrow_k (token B), .., this j = the LAST input]"
  echo "//   out i_m = ask_m maker payout, [ask_k continuation, escrow change (rest only)],"
  echo "//   out j = payer change, out j+1 = merchant delivery (the last token B output)."
  echo
  for k in 1 2 3; do for last in rest out; do
    emit_actor KasToToken KasToToken kcc20 KasToTokenState "$(k2t_name $k $last)" < <(k2t_entry $k $last)
  done; done
  echo "// ---------------------------------------------------------------- TokenToKas"
  echo
  echo "// Tx shape (all sell shapes): [bid_1 .. bid_k, our token A (leader), this j = the LAST input] ->"
  echo "//   out i_m = bid_m delivery (token to the bid maker), [bid_k continuation (rest only)],"
  echo "//   out j = merchant KAS, out j+1 = payer token change (the last token A output)."
  echo
  for k in 1 2 3; do for last in rest out; do
    emit_actor TokenToKas TokenToKas kcc20 TokenToKasState "$(t2k_name $k $last)" < <(t2k_entry $k $last kcc20)
  done; done
  echo "// ---------------------------------------------------------------- TokenSwap"
  echo
  echo "// Token A -> KAS -> token B. Tx shape (all swap shapes):"
  echo "//   in  [.., bid_1 .., ask_1 .., token A (leader, owned by this), escrow_1 .. (leaders, owned"
  echo "//        by the asks), .., this j = the LAST input]"
  echo "//   out bid deliveries (A to the bid makers), ask maker payouts, continuations, escrow B"
  echo "//       change (rest only),"
  echo "//       out j = merchant delivery (B), out j+1 = payer token change (A)."
  echo "// The KAS the bids release pays the asks; the router only bounds what the payer gives (A) and"
  echo "// gets (B), not how the keeper routes the KAS in between."
  echo
  while read -r nm kb bl ka al; do
    emit_actor TokenSwap TokenSwap kcc20 TokenSwapState "$nm" < <(swap_entry "$nm" "$kb" "$bl" "$ka" "$al" kcc20)
  done <<< "$SWAP_SHAPES"
  echo "// ---------------------------------------------------------------- TokenToKasKron"
  echo
  echo "// TokenToKas with a KRON token A (KronTokenState under the intent's KRON handle) sold into KobBidKrons."
  echo "// Same tx shape and rules; the lock is id_type 2 (owner = this intent), deliveries and change id_type 3."
  echo
  for k in 1 2 3; do for last in rest out; do
    emit_actor TokenToKasKron TokenToKas kron TokenToKasKronState "$(t2k_name $k $last)" < <(t2k_entry $k $last kron)
  done; done
  echo "// ---------------------------------------------------------------- TokenSwapKron"
  echo
  echo "// TokenSwap with a KRON token A sold into KobBidKrons; token B (KCC-20) is bought from KobAsks as in TokenSwap."
  echo
  while read -r nm kb bl ka al; do
    emit_actor TokenSwapKron TokenSwap kron TokenSwapKronState "$nm" < <(swap_entry "$nm" "$kb" "$bl" "$ka" "$al" kron)
  done <<< "$SWAP_SHAPES"
  echo "app KobRouter {"
  for a in "${ACTORS[@]}"; do echo "    actor $a;"; done
  echo "}"
} | tr -d '\r' > "$OUT"
[ "$CHECK" = 1 ] || echo "wrote $OUT"
if [ "$CHECK" = 1 ]; then
  if cmp -s "$OUT" "$TARGET"; then
    echo "ok      contracts/argent/kob_router.ag (matches tools/router-gen)"; rm -f "$OUT"
  else
    rm -f "$OUT"; echo "contracts/argent/kob_router.ag differs from tools/router-gen output: run tools/router-gen/gen-router.sh" >&2; exit 1
  fi
fi
