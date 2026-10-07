#!/usr/bin/env bash
# Reproducible contract build.
#
# Compiles every contracts/**/*.sil except contracts/argent and contracts/deploy (constructor arguments
# from the sibling <Name>.ctor.json) with silverc and either writes or verifies contracts/artifacts/<Name>.json.
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
while IFS= read -r src; do
  name=$(basename "$src" .sil)
  # contracts/artifacts is flat: two sources with one basename would write the same artifact.
  i=$(name_index "$name")
  [ -z "$i" ] || { echo "duplicate source name $name: ${srcs[$i]} and $src" >&2; exit 2; }
  ctor="${src%.sil}.ctor.json"
  [ -f "$ctor" ] || { echo "missing constructor arguments: $ctor" >&2; exit 2; }
  "$COMPILER" "$src" --constructor-args "$ctor" -o "$OUT/$name.json"
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
done < <(find contracts -name '*.sil' -not -path 'contracts/argent/*' -not -path 'contracts/deploy/*' | LC_ALL=C sort)

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
