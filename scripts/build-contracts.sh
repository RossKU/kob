#!/usr/bin/env bash
# Reproducible contract build.
#
# Compiles every contracts/**/*.sil except contracts/argent and contracts/deploy (constructor arguments
# from the sibling <Name>.ctor.json) with silverc and either writes or verifies contracts/artifacts/<Name>.json.
#
#   scripts/build-contracts.sh            write artifacts, contracts/argent and contracts/SHA256SUMS
#   scripts/build-contracts.sh --check    fail (exit 1) if any of them differs; writes nothing
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
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
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
while IFS= read -r src; do
  name=$(basename "$src" .sil)
  ctor="${src%.sil}.ctor.json"
  [ -f "$ctor" ] || { echo "missing constructor arguments: $ctor" >&2; exit 2; }
  "$COMPILER" "$src" --constructor-args "$ctor" -o "$OUT/$name.json"
  tr -d '\r' < "$OUT/$name.json" > "$OUT/$name.json.lf" && mv "$OUT/$name.json.lf" "$OUT/$name.json"
  names+=("$name")
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

# Argent side: KOBOrders (hand-written orders wrapped by sil2argent), KOBToken, router.
if [ "$ARGENT" = 1 ]; then
  if [ "$MODE" = check ]; then scripts/build-argent.sh --check || fail=1; else scripts/build-argent.sh; fi
fi

# Manifest of every input and output, so a change to any of them is visible in review. The deployment
# builds under contracts/deploy/ have their own SHA256SUMS (scripts/build-deploy.sh).
manifest() {
  { find contracts -path contracts/deploy -prune -o \( -name '*.sil' -o -name '*.ctor.json' -o -name '*.bin' -o -name '*.json' -path 'contracts/artifacts/*' -o -type f -path 'contracts/argent/*' -o -type f -path 'contracts/third-party/*' -o -type f -path 'contracts/retired/*' \) -print ; } \
    | LC_ALL=C sort | while IFS= read -r f; do
        printf '%s  %s\n' "$(tr -d '\r' < "$f" | { sha256sum 2>/dev/null || shasum -a 256; } | cut -d' ' -f1)" "$f"
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
