#!/usr/bin/env bash
# Builds the web app for the soak's UI server against the TN10 deployment bindings of kob-wasm, WITHOUT touching web/wasm (the web
# tests use the reference bindings): the app sources are staged in target/soak/webbuild with the TN10 bindings, vite builds into
# tools/soak/run/web-dist.
#
#   tools/soak/scripts/build-ui.sh [<kob-wasm web bindings dir, default crates/kob-wasm/pkg-tn10>]
#
# The build pins the sha256 of the soak's own registry (tools/soak/run/registry/tokens.json, the file serve-ui.mjs serves) as the app's
# default registry: no custom-registry dot or notice on the network badge and no "not official" labels on the soak tokens.
# `UI_OUT=<dir under the repository>` builds elsewhere (a staging copy while the soak keeps serving run/web-dist).
# `UI_REGISTRY=<file>` pins another file (`UI_REGISTRY=registry/tokens.json`: the repository's default, as a release build does).
# Rebuild after `soak.mjs setup` rewrites the registry (a new token): a served registry that no longer matches the pin shows the notice again.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../../.."
ROOT=$PWD
PKG=${1:-crates/kob-wasm/pkg-tn10}
STAGE=target/soak/webbuild
OUT=${UI_OUT:-tools/soak/run/web-dist}
[ -f "$PKG/kob_wasm.js" ] || { echo "no web bindings in $PKG (scripts/build-wasm.sh --features deploy-tn10)" >&2; exit 1; }
[ -d web/node_modules ] || { echo "run npm ci in web/ first" >&2; exit 1; }
[ -f web/vendor/kaspa-web/kaspa.js ] || { echo "run npm run fetch-sdk in web/ first" >&2; exit 1; }
rm -rf "$STAGE"
mkdir -p "$STAGE/wasm"
cp -r web/src web/public web/index.html web/vite.config.ts web/tsconfig.json web/package.json web/*.mjs web/*.d.mts "$STAGE/"
# vite.config.ts pins the sha256 of the default registry (web/registry-pin.mjs reads ../registry/tokens.json): stage the soak's registry there
REG=${UI_REGISTRY:-tools/soak/run/registry/tokens.json}
[ -f "$REG" ] || { echo "no registry at $REG (soak.mjs setup writes tools/soak/run/registry/tokens.json)" >&2; exit 1; }
mkdir -p "$STAGE/../registry"
cp "$REG" "$STAGE/../registry/tokens.json"
echo "registry pin: $REG (sha256 $(sha256sum "$REG" | cut -d' ' -f1))"
cp -r web/vendor "$STAGE/vendor"
cp -r "$PKG" "$STAGE/wasm/web"
# node_modules: a directory junction on Windows, a symlink elsewhere
if [ "${OS:-}" = "Windows_NT" ]; then
  cmd //c mklink //J "$(cygpath -w "$STAGE/node_modules")" "$(cygpath -w "$ROOT/web/node_modules")" >/dev/null
else
  ln -s "$ROOT/web/node_modules" "$STAGE/node_modules"
fi
rm -rf "$OUT"
(cd "$STAGE" && node node_modules/vite/bin/vite.js build --outDir "$ROOT/$OUT" --emptyOutDir)
echo "UI built into $OUT"
