#!/usr/bin/env bash
# Reproducible build of the Argent side of KOB (contracts/argent/).
#
#   scripts/build-argent.sh            write contracts/argent/{KOBOrders,KOBOrdersKron,KOBToken,router,examples-out,port-out}
#   scripts/build-argent.sh --check    fail (exit 1) if any of them differs; writes nothing
#
# Pipeline (all inputs are committed; outputs are deterministic):
#   0. router.ag   tools/router-gen/gen-router.sh writes contracts/argent/kob_router.ag (one actor per
#                  fill shape); --check fails if the committed file is not what the generator writes.
#   1. argent/build-argentc.sh    argentc from the pinned upstream commit + patches
#   2. KOBOrders   argentc compiles the interface KOBOrders.ag to a skeleton; tools/sil2argent
#                  replaces every generated contract by the silverc artifact of the hand-written
#                  contracts/v2/*.sil (contracts/artifacts/*.json) and recomputes handles and id.
#   2b. KOBOrdersKron  the same for KOBOrdersKron.ag (the KRON limit orders KobAskKron, KobBidKron of
#                  contracts/adapters/kron/v2/*.sil): a separate app, so the KOBOrders id stays as published.
#   3. KOBToken    argentc compiles kcc20_8x8.ag; its program must equal
#                  contracts/kcc20/variants/KCC20Ref_8x8.sil and its handle the silverc template.
#   4. router      argentc compiles kob_router.ag, importing the KOBOrders and KOBOrdersKron artifacts
#                  (steps 2, 2b); the token programs are open ICC handles in the intents' state.
#   5. examples    contracts/argent/examples/*.ag (third-party imports) must compile; their generated SilverScript is
#                  published under contracts/argent/examples-out/ (engine-tested), nothing else.
#   6. port        contracts/argent/port/kob_ask_port.ag (KobAsk ported 1:1, a size measurement) must compile; its
#                  generated SilverScript is published under contracts/argent/port-out/.
# Needs the silverc artifacts to be current: scripts/build-contracts.sh runs this after them.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
ROOT=$PWD
MODE=write
for arg in "$@"; do
  case "$arg" in
    --check) MODE=check ;;
    -h|--help) sed -n "2,24p" "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

# 0. kob_router.ag is generated (tools/router-gen); a stale committed file fails the check.
if [ "$MODE" = check ]; then bash tools/router-gen/gen-router.sh --check || exit 1; else bash tools/router-gen/gen-router.sh; fi

# 1. argentc (pinned upstream + patches) and sil2argent
ARGENTC=$(argent/build-argentc.sh --verify | tail -n1)
cargo build --locked -q -p sil2argent
S2A="${CARGO_TARGET_DIR:-$ROOT/target}/debug/sil2argent"
[ -x "$S2A" ] || S2A="$S2A.exe"
echo "argentc:    $ARGENTC"
echo "sil2argent: $S2A"

# Stage inside the repo (not /tmp): argentc records canonical absolute paths, which sil2argent
# strips again, so the stage path must not depend on symlinks or 8.3 short names.
STAGE="$ROOT/target/argent-stage"
rm -rf "$STAGE"
mkdir -p "$STAGE/src" "$STAGE/out"
trap 'rm -rf "$STAGE"' EXIT
cp contracts/argent/*.ag "$STAGE/src/"
STAGE_NATIVE=$(cygpath -m "$STAGE/src" 2>/dev/null || echo "$STAGE/src")

# 2. KOBOrders
actors=()
for f in $(ls contracts/v2/*.sil | LC_ALL=C sort); do
  n=$(basename "$f" .sil)
  actors+=("$n=contracts/artifacts/$n.json")
done
(cd "$STAGE/src" && "$ARGENTC" build KOBOrders.ag --out ../out/orders_skeleton >/dev/null)
mkdir -p "$STAGE/src/KOBOrders"
"$S2A" build "$STAGE/out/orders_skeleton/artifact.json" "$STAGE/src/KOBOrders/artifact.json" --root "$STAGE_NATIVE" "${actors[@]}"
"$S2A" verify "$STAGE/src/KOBOrders/artifact.json" "${actors[@]}" >/dev/null
# The router (and every example) imports KOBOrders by PINNED id (`import "./KOBOrders/artifact.json" id "<id>";`,
# argentc patch 0002 refuses any other artifact). The pin is a reviewed constant in the repo: a
# change of the orders changes the id, and the build stops here until the pin is updated on purpose.
PIN=$(sed -n 's/^import "[^"]*KOBOrders\/artifact\.json" id "\([0-9a-f]\{64\}\)";$/\1/p' tools/router-gen/router_head.ag)
[ -n "$PIN" ] || { echo "tools/router-gen/router_head.ag: no pinned KOBOrders id (import \"./KOBOrders/artifact.json\" id \"<id>\";)" >&2; exit 1; }
KOB_ORDERS_ID=$(sed -n 's/^  "id": "\([0-9a-f]\{64\}\)",$/\1/p' "$STAGE/src/KOBOrders/artifact.json" | head -n1)
if [ "$KOB_ORDERS_ID" != "$PIN" ]; then
  echo "KOBOrders is $KOB_ORDERS_ID but the router pins $PIN: review the order change, then update the pin in" >&2
  echo "tools/router-gen/router_head.ag and contracts/argent/examples/*.ag (and docs/argent.md)" >&2
  exit 1
fi
"$S2A" verify "$STAGE/src/KOBOrders/artifact.json" --id "$PIN" "${actors[@]}" >/dev/null
for f in contracts/argent/examples/*.ag; do
  if grep -q '^import "[^"]*KOBOrders/artifact\.json"' "$f" && ! grep -q "^import \"[^\"]*KOBOrders/artifact\.json\" id \"$PIN\";" "$f"; then
    echo "$f: its KOBOrders import does not pin $PIN" >&2; exit 1
  fi
done

# 2b. KOBOrdersKron (the KRON limit orders; the router observes KobBidKron by closed ICC, pinned like KOBOrders)
kron_actors=()
for n in KobAskKron KobBidKron; do kron_actors+=("$n=contracts/artifacts/$n.json"); done
(cd "$STAGE/src" && "$ARGENTC" build KOBOrdersKron.ag --out ../out/orders_kron_skeleton >/dev/null)
mkdir -p "$STAGE/src/KOBOrdersKron"
"$S2A" build "$STAGE/out/orders_kron_skeleton/artifact.json" "$STAGE/src/KOBOrdersKron/artifact.json" --root "$STAGE_NATIVE" "${kron_actors[@]}"
KPIN=$(sed -n 's/^import "[^"]*KOBOrdersKron\/artifact\.json" id "\([0-9a-f]\{64\}\)";$/\1/p' tools/router-gen/router_head.ag)
[ -n "$KPIN" ] || { echo "tools/router-gen/router_head.ag: no pinned KOBOrdersKron id" >&2; exit 1; }
KOB_ORDERS_KRON_ID=$(sed -n 's/^  "id": "\([0-9a-f]\{64\}\)",$/\1/p' "$STAGE/src/KOBOrdersKron/artifact.json" | head -n1)
if [ "$KOB_ORDERS_KRON_ID" != "$KPIN" ]; then
  echo "KOBOrdersKron is $KOB_ORDERS_KRON_ID but the router pins $KPIN: review the order change, then update the pin in" >&2
  echo "tools/router-gen/router_head.ag (and docs/argent.md)" >&2
  exit 1
fi
"$S2A" verify "$STAGE/src/KOBOrdersKron/artifact.json" --id "$KPIN" "${kron_actors[@]}" >/dev/null

# 3. KOBToken
(cd "$STAGE/src" && "$ARGENTC" build kcc20_8x8.ag --out ../out/KOBToken >/dev/null)
strip_sil() { tr -d '\r' < "$1" | sed -E 's#^[[:space:]]*//.*$##' | grep -v '^[[:space:]]*$' || true; }
if ! diff <(strip_sil "$STAGE/out/KOBToken/sil/KCC20.sil") <(strip_sil contracts/kcc20/variants/KCC20Ref_8x8.sil) >/dev/null; then
  echo "KOBToken: argentc output differs from contracts/kcc20/variants/KCC20Ref_8x8.sil" >&2
  diff <(strip_sil "$STAGE/out/KOBToken/sil/KCC20.sil") <(strip_sil contracts/kcc20/variants/KCC20Ref_8x8.sil) >&2 || true
  exit 1
fi
"$S2A" handles "$STAGE/out/KOBToken/artifact.json" KCC20=contracts/artifacts/KCC20Ref_8x8.json >/dev/null

# 4. router (imports ./KOBOrders/artifact.json relative to its source)
(cd "$STAGE/src" && "$ARGENTC" build kob_router.ag --out ../out/router >/dev/null)
# The dependency copies under apps/ are the two artifacts above; do not publish them twice.
rm -rf "$STAGE/out/router/apps"

# 5. Third-party examples (docs/argent.md): compiled so they cannot rot. Only their generated SilverScript is
#    published (contracts/argent/examples-out/<example>/, for the engine tests of argent_router_tests.rs).
mkdir -p "$STAGE/src/examples"
cp contracts/argent/examples/*.ag "$STAGE/src/examples/"
for f in contracts/argent/examples/*.ag; do
  n=$(basename "$f" .ag)
  (cd "$STAGE/src/examples" && "$ARGENTC" build "$n.ag" --out "../../out/examples/$n" >/dev/null)
done

# 6. The 1:1 Argent port of KobAsk (contracts/argent/port/, docs/argent-feedback.md item 10): a measurement, not a KOB
#    contract. Only its generated SilverScript is published (contracts/argent/port-out/, compared with the hand-written
#    KobAsk by crates/kob-tests/tests/argent_port_tests.rs).
mkdir -p "$STAGE/src/port"
cp contracts/argent/port/*.ag "$STAGE/src/port/"
(cd "$STAGE/src/port" && "$ARGENTC" build kob_ask_port.ag --out ../../out/port >/dev/null)
# its idiomatic variant (the body checks the generated ones repeat removed; argent_become_splice_tests.rs)
(cd "$STAGE/src/port" && "$ARGENTC" build kob_ask_port_idiomatic.ag --out ../../out/port-idiomatic >/dev/null)

# Publish tree: LF, repo-relative paths.
PUB="$STAGE/pub"
mkdir -p "$PUB"
cp -r "$STAGE/src/KOBOrders" "$PUB/KOBOrders"
cp -r "$STAGE/src/KOBOrdersKron" "$PUB/KOBOrdersKron"
cp -r "$STAGE/out/KOBToken" "$PUB/KOBToken"
cp -r "$STAGE/out/router" "$PUB/router"
mkdir -p "$PUB/examples-out"
for f in contracts/argent/examples/*.ag; do
  n=$(basename "$f" .ag)
  cp -r "$STAGE/out/examples/$n/sil" "$PUB/examples-out/$n"
done
# published as KobAsk.port.sil: contracts/**/KobAsk.sil must stay the hand-written order (the tests find sources by name)
mkdir -p "$PUB/port-out"
cp "$STAGE/out/port/sil/KobAsk.sil" "$PUB/port-out/KobAsk.port.sil"
cp "$STAGE/out/port-idiomatic/sil/KobAsk.sil" "$PUB/port-out/KobAsk.idiomatic.sil"
files=()
while IFS= read -r f; do files+=("$f"); done < <(find "$PUB" -name '*.json' | LC_ALL=C sort)
"$S2A" relativize "$STAGE_NATIVE" "${files[@]}"
find "$PUB" -type f -exec sh -c 'tr -d "\r" < "$1" > "$1.lf" && mv "$1.lf" "$1"' _ {} \;

# The router must link exactly the artifacts published next to it.
"$S2A" links "$PUB/router/artifact.json" "$PUB/KOBOrders/artifact.json" "$PUB/KOBOrdersKron/artifact.json" "$PUB/KOBToken/artifact.json"

fail=0
for d in KOBOrders KOBOrdersKron KOBToken router examples-out port-out; do
  if [ "$MODE" = check ]; then
    if diff -r --strip-trailing-cr "$PUB/$d" "contracts/argent/$d" >/dev/null 2>&1; then
      echo "ok      contracts/argent/$d"
    else
      echo "DIFFERS contracts/argent/$d" >&2; fail=1
    fi
  else
    rm -rf "contracts/argent/$d"
    cp -r "$PUB/$d" "contracts/argent/$d"
    echo "wrote   contracts/argent/$d"
  fi
done
[ "$fail" = 0 ] || { echo "contracts/argent is stale: run scripts/build-argent.sh" >&2; exit 1; }
