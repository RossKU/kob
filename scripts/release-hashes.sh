#!/usr/bin/env bash
# Hashes to publish with a release (docs/ops/release.md, step 6).
#
#   scripts/release-hashes.sh [--network mainnet] [binary ...]
#
# Prints, in one Markdown block: the commit, the order template hashes and the pinned registry of the
# deployment record (contracts/deploy/<network>), the sha256 of the record files, of contracts/SHA256SUMS,
# of every file of the built web app (web/dist) and of each binary given (e.g. target/release/kob-executor).
# Fails when the deployment record does not reproduce (scripts/build-deploy.sh --check), when web/dist is
# missing, or when the web bundle does not embed and ship (dist/registry/tokens.json, `npm run build:release`)
# the registry the record pins. Read-only. Needs node.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
NETWORK=mainnet
BINS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --network) NETWORK=$2; shift ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    -*) echo "unknown argument: $1" >&2; exit 2 ;;
    *) BINS+=("$1") ;;
  esac
  shift
done
DIR=contracts/deploy/$NETWORK
sha() { tr -d '\r' < "$1" | { sha256sum 2>/dev/null || shasum -a 256; } | cut -d' ' -f1; }
rawsha() { { sha256sum "$1" 2>/dev/null || shasum -a 256 "$1"; } | cut -d' ' -f1; }

scripts/build-deploy.sh "$NETWORK" --check > /dev/null
[ -d web/dist ] || { echo "web/dist is missing: cd web && npm run build:release" >&2; exit 1; }
PIN=$(node -e 'const m = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")); console.log(m.registry ? m.registry.sha256 : "")' "$DIR/deployment.json")
if [ -n "$PIN" ]; then
  grep -rqs "$PIN" web/dist/assets || { echo "web/dist does not embed the pinned registry hash $PIN: rebuild it from this commit" >&2; exit 1; }
  [ -f web/dist/registry/tokens.json ] && [ "$(sha web/dist/registry/tokens.json)" = "$PIN" ]     || { echo "web/dist/registry/tokens.json is missing or not the pinned registry: cd web && npm run build:release" >&2; exit 1; }
fi

echo "## KOB release hashes ($NETWORK)"
echo
DIRTY=$(git status --porcelain --untracked-files=normal -- . 2>/dev/null || echo unknown)
echo "commit \`$(git rev-parse HEAD)\`${DIRTY:+ (WORKING TREE DIRTY: do not publish)}"
echo
echo "### Order templates (contracts/deploy/$NETWORK/deployment.json)"
echo
node -e 'for (const t of JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).templates) console.log(`- ${t.name}: \`${t.hash}\``)' "$DIR/deployment.json"
if [ -n "$PIN" ]; then
  echo
  echo "### Token registry"
  echo
  echo "- registry/tokens.json sha256 (LF): \`$PIN\` (executor \`GET /v1/tokens\` \`registry.sha256\`, web bundle pin)"
fi
echo
echo "### Files (sha256, line endings normalised to LF for text sources)"
echo
echo '```'
for f in contracts/SHA256SUMS "$DIR"/SHA256SUMS "$DIR"/deployment.json "$DIR"/DEPLOYMENT.md; do echo "$(sha "$f")  $f"; done
find web/dist -type f | LC_ALL=C sort | while IFS= read -r f; do echo "$(rawsha "$f")  $f"; done
for b in ${BINS[@]+"${BINS[@]}"}; do echo "$(rawsha "$b")  $b"; done
echo '```'
