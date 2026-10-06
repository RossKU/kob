#!/usr/bin/env bash
# Builds kob-wasm for wasm32 and generates the JS bindings.
#
#   scripts/build-wasm.sh            node bindings in crates/kob-wasm/pkg-node (used by the node test)
#   scripts/build-wasm.sh --web      also browser/bundler ES-module bindings in crates/kob-wasm/pkg
#   scripts/build-wasm.sh --test     build, then run the node golden-vector tests (Node >= 22): the protocol
#                                    vectors (crates/kob-wasm/tests/node) and the x402 SDK package tests, whose
#                                    wasm-golden test replays crates/kob-x402/vectors/golden/*.json
#   scripts/build-wasm.sh --tn10     the testnet-10 deployment variant (kob-wasm --features deploy-tn10: records the network of
#                                    contracts/deploy/testnet-10; the templates are the reference ones) into
#                                    crates/kob-wasm/pkg-node-tn10 and, with --web, crates/kob-wasm/pkg-tn10; the default
#                                    bindings are not touched
#   scripts/build-wasm.sh --mainnet  the mainnet deployment variant (kob-wasm --features deploy-mainnet: records the network of
#                                    contracts/deploy/mainnet; same templates) into crates/kob-wasm/pkg-node-mainnet and,
#                                    with --web, crates/kob-wasm/pkg-mainnet (docs/ops/release.md)
#   scripts/build-wasm.sh --slim    the web-app variant (kob-wasm --no-default-features --features engine: no x402 payer / merchant
#                                    exports, so the browser bundle carries no secret-key entry points) into
#                                    crates/kob-wasm/pkg-node-slim and, with --web, crates/kob-wasm/pkg-slim
#   scripts/build-wasm.sh --install-bindgen
#                                    first install wasm-bindgen-cli 0.2.100 under $TARGET_DIR/tools
#
# wasm-bindgen-cli must be exactly the wasm-bindgen crate version in Cargo.lock (0.2.100; the
# bindings embed a schema version and a mismatch fails at run time). Either install it globally:
#   cargo install wasm-bindgen-cli --version 0.2.100 --locked
# or keep it inside the build directory (nothing written to ~/.cargo/bin; deleted with target/):
#   cargo install wasm-bindgen-cli --version 0.2.100 --locked --root "${CARGO_TARGET_DIR:-target}/tools"
# which is what --install-bindgen runs. The CLI is taken from $WASM_BINDGEN if set, else from
# $TARGET_DIR/tools/bin if present, else from PATH. The build is reproducible: the node test checks
# every golden vector byte for byte against the native library.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
WEB=0
TEST=0
INSTALL=0
TN10=0
MAINNET=0
SLIM=0
for arg in "$@"; do
  case "$arg" in
    --web) WEB=1 ;;
    --test) TEST=1 ;;
    --install-bindgen) INSTALL=1 ;;
    --tn10) TN10=1 ;;
    --mainnet) MAINNET=1 ;;
    --slim) SLIM=1 ;;
    -h|--help) sed -n '2,29p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

if [ "$TN10" = 1 ] && [ "$TEST" = 1 ]; then
  echo "--test replays the reference golden vectors: not with --tn10" >&2; exit 2
fi
FEATURES=(); SUFFIX=
if [ "$TN10" = 1 ] && [ "$MAINNET" = 1 ]; then echo "--tn10 and --mainnet are exclusive" >&2; exit 2; fi
if [ "$MAINNET" = 1 ] && [ "$TEST" = 1 ]; then
  echo "--test replays the reference golden vectors: not with --mainnet" >&2; exit 2
fi
if [ "$TN10" = 1 ]; then FEATURES=(--features deploy-tn10); SUFFIX=-tn10; fi
if [ "$MAINNET" = 1 ]; then FEATURES=(--features deploy-mainnet); SUFFIX=-mainnet; fi
if [ "$SLIM" = 1 ]; then
  if [ "$TN10" = 1 ] || [ "$MAINNET" = 1 ] || [ "$TEST" = 1 ]; then echo "--slim is the plain web variant: not with --tn10, --mainnet or --test" >&2; exit 2; fi
  FEATURES=(--no-default-features --features engine); SUFFIX=-slim
fi

TARGET_DIR=${CARGO_TARGET_DIR:-target}
want=$(awk '/^name = "wasm-bindgen"$/ {getline; gsub(/"/, "", $3); print $3; exit}' Cargo.lock)
if [ "$INSTALL" = 1 ]; then
  cargo install wasm-bindgen-cli --version "$want" --locked --root "$TARGET_DIR/tools"
fi
if [ -n "${WASM_BINDGEN:-}" ]; then
  BINDGEN=$WASM_BINDGEN
elif [ -x "$TARGET_DIR/tools/bin/wasm-bindgen" ] || [ -x "$TARGET_DIR/tools/bin/wasm-bindgen.exe" ]; then
  BINDGEN="$TARGET_DIR/tools/bin/wasm-bindgen"
else
  BINDGEN=wasm-bindgen
fi
have=$("$BINDGEN" --version | awk '{print $2}')
if [ "$want" != "$have" ]; then
  echo "wasm-bindgen CLI $have does not match the crate version $want (see --install-bindgen)" >&2
  exit 1
fi

cargo build --locked --release -p kob-wasm --target wasm32-unknown-unknown ${FEATURES[@]+"${FEATURES[@]}"}
WASM="$TARGET_DIR/wasm32-unknown-unknown/release/kob_wasm.wasm"
"$BINDGEN" --target nodejs --out-dir "crates/kob-wasm/pkg-node$SUFFIX" "$WASM"
if [ "$WEB" = 1 ]; then
  "$BINDGEN" --target web --out-dir "crates/kob-wasm/pkg$SUFFIX" "$WASM"
fi
ls -l "crates/kob-wasm/pkg-node$SUFFIX"/*.wasm
if [ "$TEST" = 1 ]; then
  node --test crates/kob-wasm/tests/node/*.test.mjs
  (cd packages/kob-x402 && { [ -d node_modules ] || npm ci; } && KOB_REQUIRE_WASM=1 npm test)
fi
