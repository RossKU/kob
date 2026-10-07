#!/usr/bin/env bash
# Deployment record of a network.
#
#   scripts/build-deploy.sh <network>           write contracts/deploy/<network>/ (deployment.json,
#                                               DEPLOYMENT.md, SHA256SUMS)
#   scripts/build-deploy.sh <network> --check   fail (exit 1) unless it reproduces byte for byte
#
#   networks: testnet-10, mainnet (contracts/deploy/<network>; docs/ops/release.md is the mainnet checklist)
#
# No template depends on a network or a genesis (protocol v2.6: a stop is armed by a fill in the same
# transaction; there is no receipt covenant and no R_ID), so nothing is recompiled: the record lists the
# reference artifacts of contracts/artifacts (scripts/lib/deploy.mjs) and SHA256SUMS pins them, so a change
# of any order template makes the deployment record stale. When registry/tokens.json is the registry of the
# network (mainnet), the record pins it too (sha256, strict templates, listed tokens) and SHA256SUMS lists it,
# so a registry change makes the record stale as well. Needs node.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
NETWORK=
MODE=write
while [ $# -gt 0 ]; do
  case "$1" in
    --check) MODE=check ;;
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    -*) echo "unknown argument: $1" >&2; exit 2 ;;
    *) [ -z "$NETWORK" ] || { echo "one network only" >&2; exit 2; }; NETWORK=$1 ;;
  esac
  shift
done
[[ "$NETWORK" =~ ^[a-z0-9-]+$ ]] || { echo "usage: scripts/build-deploy.sh <network> [--check]" >&2; exit 2; }
DIR=contracts/deploy/$NETWORK

OUT=target/build-deploy.$$
rm -rf "$OUT"; mkdir -p "$OUT"
trap 'rm -rf "$OUT" "$OUT.sums"' EXIT
node scripts/lib/deploy.mjs "$NETWORK" "$OUT"

# SHA256SUMS: the generated files and the artifacts they list. Text files are hashed with CR removed (a CRLF
# checkout lists the same digest); binary files (.bin) are hashed as their bytes.
file_sha() {
  case "$1" in
    *.bin) cat -- "$1" ;;
    *) tr -d '\r' < "$1" ;;
  esac | { sha256sum 2>/dev/null || shasum -a 256; } | cut -d' ' -f1
}
{
  (cd "$OUT" && find . -type f | sed 's#^\./##') | while IFS= read -r f; do
    printf '%s  %s\n' "$(file_sha "$OUT/$f")" "$DIR/$f"
  done
  node -e 'const m = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")); for (const t of m.templates) console.log(t.artifact); if (m.registry) console.log(m.registry.path)' "$OUT/deployment.json" \
    | tr -d '\r' | while IFS= read -r f; do
        printf '%s  %s\n' "$(file_sha "$f")" "$f"
      done
} | LC_ALL=C sort -k2 > "$OUT.sums"
mv "$OUT.sums" "$OUT/SHA256SUMS"

if [ "$MODE" = check ]; then
  fail=0
  want=$(cd "$OUT" && find . -type f | LC_ALL=C sort)
  have=$( { [ -d "$DIR" ] && cd "$DIR" && find . -type f | LC_ALL=C sort; } || true)
  [ "$want" = "$have" ] || { echo "file set differs in $DIR:" >&2; diff <(echo "$have") <(echo "$want") >&2 || true; fail=1; }
  while IFS= read -r f; do
    if cmp -s <(tr -d '\r' < "$DIR/$f" 2>/dev/null) "$OUT/$f"; then echo "ok      $DIR/${f#./}"; else echo "DIFFERS $DIR/${f#./}" >&2; fail=1; fi
  done <<< "$want"
  [ "$fail" = 0 ] || { echo "$DIR is stale: run scripts/build-deploy.sh $NETWORK" >&2; exit 1; }
  echo "$DIR reproduces"
else
  rm -rf "$DIR"; mkdir -p "$(dirname "$DIR")"
  cp -r "$OUT" "$DIR"
  echo "wrote $DIR"
fi
