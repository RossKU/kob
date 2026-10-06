#!/usr/bin/env bash
# Reproducible build of `argentc` (the Argent compiler): the pinned upstream commit plus our patches.
#
#   argent/build-argentc.sh              clone (if needed), pin, patch, build; prints the binary path
#   argent/build-argentc.sh --verify     also check vendor/argent-artifact against the pinned upstream
#
# Inputs: argent/upstream.lock (url, rev) and argent/patches/*.patch (applied in name order).
# The checkout lives in argent/upstream/ (git-ignored) and is reset to the pinned commit on every
# run, so local edits there are discarded. The build uses the KOB toolchain (rust-toolchain.toml)
# and its own target dir (target/argent, override with ARGENT_TARGET_DIR).
#   ARGENT_URL=<path or url>   clone source override (the pinned rev is still enforced)
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
ROOT=$PWD
UP=$ROOT/argent/upstream
lock_get() { awk -v k="$1" '$1==k {print $2}' argent/upstream.lock; }
URL=${ARGENT_URL:-$(lock_get url)}
REV=$(lock_get rev)
VERIFY=0
for arg in "$@"; do
  case "$arg" in
    --verify) VERIFY=1 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

if [ ! -d "$UP/.git" ]; then
  git clone --quiet -c core.autocrlf=false "$URL" "$UP"
fi
git -C "$UP" config core.autocrlf false
if ! git -C "$UP" cat-file -e "$REV^{commit}" 2>/dev/null; then
  git -C "$UP" fetch --quiet origin "$REV" || git -C "$UP" fetch --quiet origin
fi
# The checkout is reset and patched only when the pin or a patch changed (a stamp records what was
# applied), so an unchanged tree keeps its mtimes and cargo does not rebuild argentc. Delete
# argent/upstream to force a fresh clone.
sha256() { if command -v sha256sum >/dev/null; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi; }
STAMP=$({ echo "$REV"; cat $(ls argent/patches/*.patch | LC_ALL=C sort); } | tr -d '\015' | sha256)
if [ "$(cat "$UP/.git/kob-stamp" 2>/dev/null || true)" != "$STAMP" ]; then
  git -C "$UP" reset --quiet --hard "$REV"
  git -C "$UP" clean --quiet -fdx
  [ "$(git -C "$UP" rev-parse HEAD)" = "$REV" ] || { echo "argent upstream is not at $REV" >&2; exit 1; }
  for p in $(ls argent/patches/*.patch | LC_ALL=C sort); do
    git -C "$UP" apply --whitespace=nowarn "$ROOT/$p"
    echo "applied $p" >&2
  done
  echo "$STAMP" > "$UP/.git/kob-stamp"
fi

if [ "$VERIFY" = 1 ]; then
  # vendor/argent-artifact must be the pinned upstream crate: src/ identical, Cargo.toml only wired.
  if ! diff -r "$UP/crates/argent-artifact/src" vendor/argent-artifact/src >/dev/null; then
    echo "vendor/argent-artifact/src differs from argent upstream $REV" >&2
    diff -r "$UP/crates/argent-artifact/src" vendor/argent-artifact/src >&2 || true
    exit 1
  fi
  echo "vendor/argent-artifact matches upstream $REV" >&2
fi

TARGET=${ARGENT_TARGET_DIR:-$ROOT/target/argent}
cargo build --release --locked -q --manifest-path "$UP/Cargo.toml" --bin argentc --target-dir "$TARGET"
BIN=$TARGET/release/argentc
[ -x "$BIN" ] || BIN=$BIN.exe
echo "$BIN"
