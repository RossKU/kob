#!/usr/bin/env bash
# Reproducible contract build.
#
# Compiles every contracts/**/*.sil except contracts/argent, contracts/deploy and contracts/third-party (constructor arguments
# from the sibling <Name>.ctor.json) with silverc and either writes or verifies contracts/artifacts/<Name>.json. The
# third-party programs KOB embeds unmodified are listed in THIRD_PARTY below with their artifact name (constructor
# arguments from the sibling <source>.ctor.json, written by KOB). VARIANTS below are further builds of a source with build
# parameters (a `byte constant` of the source set to another value, the contract renamed to the build's artifact name,
# constructor arguments from the file the entry names); the derived source exists only in the build directory.
#
#   scripts/build-contracts.sh            write artifacts, contracts/argent and contracts/SHA256SUMS
#   scripts/build-contracts.sh --check    fail (exit 1) if any of them differs, or if contracts/artifacts holds
#                                         a file no source produces, or if the wallet-gate kit's artifacts do not
#                                         reproduce from its sources (scripts/check-wallet-gate.mjs); writes nothing
#                                         (two sources with the same basename are refused in both modes)
#   ... --upstream                        use the official silverc v1.0.0 release binary
#                                         (downloaded, sha256-verified against contracts/silverc.lock)
#                                         instead of building vendor/silverscript
#   SILVERC=/path/to/silverc              use this binary as-is (overrides both)
#   ... --no-argent                       skip the Argent artifacts (contracts/argent, needs argentc,
#                                         built from the pinned upstream: see argent/UPSTREAM.md)
#
# Sources are passed to silverc as repo-relative paths from the repo root because the artifact
# records `source_path`; that keeps the output independent of the checkout location.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
MODE=write
UPSTREAM=0
ARGENT=1
for arg in "$@"; do
  case "$arg" in
    --check) MODE=check ;;
    --upstream) UPSTREAM=1 ;;
    --no-argent) ARGENT=0 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

# shellcheck source=lib/silverc.sh
. scripts/lib/silverc.sh
COMPILER=$(resolve_silverc "$UPSTREAM")
echo "compiler: $COMPILER ($(sha256 "$COMPILER"))"

OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT
fail=0
names=()
srcs=()
# index of a source name in names (bash 3 has no associative arrays), empty if none
name_index() { local i; for i in "${!names[@]}"; do [ "${names[$i]}" = "$1" ] && { echo "$i"; return 0; }; done; return 0; }
# Third-party sources compiled as they are vendored (source|artifact name): the KCC20 actor of upstream's KCC20PublicMint
# app (contracts/third-party/kcc20-reference/UPSTREAM.md), and the build of that actor KaspaRocket deploys on testnet-10
# (10 / 10 token slots, contracts/third-party/kasparocket/UPSTREAM.md).
THIRD_PARTY=("contracts/third-party/kcc20-reference/KCC20.public-mint.sil|KCC20PublicMint"
  "contracts/third-party/kasparocket/KCC20.public-mint.10x10.sil|KaspaRocketToken11458")
# Parameterised builds (source|artifact name|constructor arguments|NAME=value ...): the KRON order kinds whose maker
# deliveries are KRON id_type 0 (a token-level signature) instead of 3 (address presence), for token programs whose holders
# are id_type 0 (KForge, testnet-10; contracts/adapters/kron/README.md). TYPE_DELIVERY is the parameter; their constructor
# arguments carry the template chain of this build (the evidence and exit templates are the *KronD0 builds).
D0=contracts/adapters/kron/v2/delivery0
VARIANTS=()
for k in Ask Bid CondAsk CondBid IfdAsk; do
  VARIANTS+=("contracts/adapters/kron/v2/Kob${k}Kron.sil|Kob${k}KronD0|$D0/Kob${k}KronD0.ctor.json|TYPE_DELIVERY=0x00")
done
# KobIfdBidKron delivers only into its exit's custody (no TYPE_DELIVERY): its build differs in the template chain alone.
VARIANTS+=("contracts/adapters/kron/v2/KobIfdBidKron.sil|KobIfdBidKronD0|$D0/KobIfdBidKronD0.ctor.json|")
mkdir -p "$OUT/variants"
# The source of a parameterised build on stdout: each NAME=value sets the one `byte constant NAME = 0x..;` line of the
# source, and the contract (named as the source file) takes the build's name. Anything else is an error.
derive_source() {
  local src=$1 name=$2 base kv k v n
  shift 2
  base=$(basename "$src" .sil)
  n=$(grep -c "^contract $base(" "$src" || true)
  [ "$n" = 1 ] || { echo "$src: expected one 'contract $base(' line, found $n" >&2; return 2; }
  local expr=(-e "s/^contract $base(/contract $name(/")
  for kv in "$@"; do
    k=${kv%%=*}; v=${kv#*=}
    [[ "$k" =~ ^[A-Z_]+$ && "$v" =~ ^0x[0-9a-f]{2}$ ]] || { echo "bad build parameter $kv" >&2; return 2; }
    n=$(grep -cE "^[[:space:]]*byte constant $k = 0x[0-9a-f]{2};" "$src" || true)
    [ "$n" = 1 ] || { echo "$src: expected one 'byte constant $k' line, found $n" >&2; return 2; }
    expr+=(-e "s/^\([[:space:]]*byte constant $k = \)0x[0-9a-f][0-9a-f];/\1$v;/")
  done
  tr -d '\r' < "$src" | sed "${expr[@]}"
}
while IFS= read -r line; do
  IFS='|' read -r src name ctor params <<< "$line"
  [ -n "$name" ] || name=$(basename "$src" .sil)
  # contracts/artifacts is flat: two sources with one basename would write the same artifact.
  i=$(name_index "$name")
  [ -z "$i" ] || { echo "duplicate source name $name: ${srcs[$i]} and $src" >&2; exit 2; }
  input=$src
  if [ -n "$ctor" ]; then
    # a parameterised build (VARIANTS: the entry names its constructor arguments)
    # shellcheck disable=SC2086 # NAME=value words
    derive_source "$src" "$name" $params > "$OUT/variants/$name.sil" || exit 2
    input=$OUT/variants/$name.sil
  else
    ctor="${src%.sil}.ctor.json"
  fi
  [ -f "$ctor" ] || { echo "missing constructor arguments: $ctor" >&2; exit 2; }
  "$COMPILER" "$input" --constructor-args "$ctor" -o "$OUT/$name.json"
  tr -d '\r' < "$OUT/$name.json" > "$OUT/$name.json.lf" && mv "$OUT/$name.json.lf" "$OUT/$name.json"
  names+=("$name"); srcs+=("$src")
  if [ "$MODE" = check ]; then
    if cmp -s <(tr -d '\r' < "contracts/artifacts/$name.json" 2>/dev/null) "$OUT/$name.json"; then
      echo "ok      $name"
    else
      echo "DIFFERS $name" >&2; fail=1
    fi
  else
    mkdir -p contracts/artifacts
    cp "$OUT/$name.json" "contracts/artifacts/$name.json"
    echo "wrote   contracts/artifacts/$name.json"
  fi
done < <(find contracts -name '*.sil' -not -path 'contracts/argent/*' -not -path 'contracts/deploy/*' -not -path 'contracts/third-party/*' | LC_ALL=C sort; printf '%s\n' "${THIRD_PARTY[@]}" "${VARIANTS[@]}")

# contracts/artifacts holds exactly the artifacts of the sources above: any other file there (one no
# source produces, left over from a removed or renamed source, or added by hand) is refused.
orphans=0
while IFS= read -r f; do
  base=$(basename "$f")
  if [ "${base%.json}.json" != "$base" ] || [ -z "$(name_index "${base%.json}")" ]; then
    echo "ORPHAN  $f (no contracts/**/*.sil produces it: remove it)" >&2; orphans=1
  fi
done < <(find contracts/artifacts -mindepth 1 \( -type f -o -type l -o -type d \) 2>/dev/null | LC_ALL=C sort)
if [ "$orphans" = 1 ]; then
  [ "$MODE" = check ] && fail=1 || { echo "contracts/artifacts has files no source produces" >&2; exit 1; }
fi

# Argent side: KOBOrders (hand-written orders wrapped by sil2argent), KOBToken, router.
if [ "$ARGENT" = 1 ]; then
  if [ "$MODE" = check ]; then scripts/build-argent.sh --check || fail=1; else scripts/build-argent.sh; fi
fi

# The wallet-gate kit's own program copies (tools/wallet-gate/contracts) and their artifacts: checked, never written
# (scripts/check-wallet-gate.mjs; the kit's scripts/build-templates.mjs writes them).
if [ "$MODE" = check ]; then
  GATE_SILVERC=$COMPILER
  if command -v cygpath >/dev/null 2>&1; then GATE_SILVERC=$(cygpath -m "$COMPILER"); fi
  node scripts/check-wallet-gate.mjs "$GATE_SILVERC" || fail=1
fi

# Manifest of every input and output, so a change to any of them is visible in review. The deployment
# builds under contracts/deploy/ have their own SHA256SUMS (scripts/build-deploy.sh).
# Text files are hashed with CR removed (a CRLF checkout lists the same digest); binary files (.bin) are hashed as
# their bytes, so the listed digest is the file's sha256.
file_sha() {
  case "$1" in
    *.bin) cat -- "$1" ;;
    *) tr -d '\r' < "$1" ;;
  esac | { sha256sum 2>/dev/null || shasum -a 256; } | cut -d' ' -f1
}
manifest() {
  { find contracts -path contracts/deploy -prune -o \( -name '*.sil' -o -name '*.ctor.json' -o -name '*.bin' -o -name '*.json' -path 'contracts/artifacts/*' -o -type f -path 'contracts/argent/*' -o -type f -path 'contracts/third-party/*' \) -print ; } \
    | LC_ALL=C sort | while IFS= read -r f; do
        printf '%s  %s\n' "$(file_sha "$f")" "$f"
      done
}
if [ "$MODE" = check ]; then
  [ "$fail" = 0 ] || { echo "contract artifacts are stale: run scripts/build-contracts.sh" >&2; exit 1; }
  # SHA256SUMS lives in contracts/, outside the artifacts glob above.
  if ! diff <(manifest) contracts/SHA256SUMS >/dev/null; then
    echo "contracts/SHA256SUMS is stale: run scripts/build-contracts.sh" >&2; diff <(manifest) contracts/SHA256SUMS >&2 || true; exit 1
  fi
  echo "all ${#names[@]} artifacts reproduce"
else
  manifest > contracts/SHA256SUMS
  echo "wrote contracts/SHA256SUMS"
fi
