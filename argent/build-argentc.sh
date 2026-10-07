#!/usr/bin/env bash
# Reproducible build of `argentc` (the Argent compiler): the pinned upstream commit plus our patches.
#
#   argent/build-argentc.sh              clone (if needed), pin, patch, build; prints the binary path
#   argent/build-argentc.sh --verify     also check vendor/argent-artifact against the pinned upstream
#
# Inputs: argent/upstream.lock (url, rev) and argent/patches/*.patch (applied in name order).
# The checkout lives in argent/upstream/ (git-ignored). On every run its whole tree (tracked, untracked
# and ignored files) is compared with the pinned commit plus the patches; any difference (a local edit,
# an extra file, a missing patch) resets it to the pin and re-applies the patches, so local edits there
# are discarded and argentc is always built from exactly the pinned source. The build uses the KOB
# toolchain (rust-toolchain.toml) and its own target dir (target/argent, override with ARGENT_TARGET_DIR).
#   ARGENT_URL=<path or url>   clone source override (the pinned rev is still enforced)
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
ROOT=$PWD
[ -f "$ROOT/argent/upstream.lock" ] || { echo "build-argentc: not in the KOB repository ($ROOT)" >&2; exit 2; }
UP=$ROOT/argent/upstream
lock_get() { awk -v k="$1" '$1==k {print $2}' argent/upstream.lock; }
URL=${ARGENT_URL:-$(lock_get url)}
REV=$(lock_get rev)
[[ "$REV" =~ ^[0-9a-f]{40}$ ]] || { echo "argent/upstream.lock: rev must be a full commit id" >&2; exit 2; }
VERIFY=0
for arg in "$@"; do
  case "$arg" in
    --verify) VERIFY=1 ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

if [ ! -d "$UP/.git" ]; then
  git clone --quiet -c core.autocrlf=false "$URL" "$UP"
fi
# Every git command below runs on the checkout only; a missing or broken checkout must not fall back
# to an enclosing repository.
export GIT_CEILING_DIRECTORIES=$ROOT/argent
ug() { git -C "$UP" "$@"; }
[ "$(cd "$UP" && git rev-parse --show-toplevel)" -ef "$UP" ] || { echo "argent/upstream is not a git checkout: delete it" >&2; exit 1; }
ug config core.autocrlf false
if ! ug cat-file -e "$REV^{commit}" 2>/dev/null; then
  ug fetch --quiet origin "$REV" || ug fetch --quiet origin
fi
PATCHES=()
while IFS= read -r p; do PATCHES+=("$p"); done < <(ls argent/patches/*.patch | LC_ALL=C sort)

# Expected tree: the pinned commit with the patches applied, built in a scratch index (the checkout is
# not touched).
IDX=$(mktemp -d)
trap 'rm -rf "$IDX"' EXIT
GIT_INDEX_FILE=$IDX/want ug read-tree "$REV"
for p in "${PATCHES[@]}"; do
  GIT_INDEX_FILE=$IDX/want ug apply --cached --whitespace=nowarn "$ROOT/$p"
done
WANT=$(GIT_INDEX_FILE=$IDX/want ug write-tree)
# Actual tree: every file under argent/upstream except .git, ignored ones included, hashed as its bytes are on disk.
# Not `git add`: it runs the clean filters and text conversions the checkout's own .git/config, .git/info/attributes
# and .gitattributes name, which can make an edited file hash as the pinned one. `hash-object --no-filters` runs none.
tree_of_checkout() {
  rm -f "$IDX/have" "$IDX/info"
  (cd "$UP" && find . -path ./.git -prune -o \( -type f -o -type l \) -print) | sed 's|^\./||' | LC_ALL=C sort > "$IDX/paths"
  local files links
  files=$(mktemp "$IDX/files.XXXX")
  links=$(mktemp "$IDX/links.XXXX")
  while IFS= read -r p; do
    if [ -L "$UP/$p" ]; then echo "$p" >> "$links"; else echo "$p" >> "$files"; fi
  done < "$IDX/paths"
  if [ -s "$files" ]; then
    paste -d '\t' <(ug hash-object --no-filters -w --stdin-paths < "$files") "$files" | while IFS=$'\t' read -r h p; do
      if [ -x "$UP/$p" ]; then m=100755; else m=100644; fi
      printf '%s %s\t%s\n' "$m" "$h" "$p"
    done >> "$IDX/info"
  fi
  while IFS= read -r p; do
    printf '120000 %s\t%s\n' "$(printf %s "$(readlink "$UP/$p")" | ug hash-object --no-filters -w --stdin)" "$p"
  done < "$links" >> "$IDX/info"
  touch "$IDX/info"
  GIT_INDEX_FILE=$IDX/have ug update-index --add --index-info < "$IDX/info"
  GIT_INDEX_FILE=$IDX/have ug write-tree
}

# An unchanged, correct tree is left alone so it keeps its mtimes and cargo does not rebuild argentc.
HAVE=$(tree_of_checkout)
if [ "$HAVE" != "$WANT" ]; then
  [ -f "$UP/.git/kob-stamp" ] && echo "argent/upstream differs from $REV + patches: resetting it" >&2
  ug reset --quiet --hard "$REV"
  ug clean --quiet -ffdx
  [ "$(ug rev-parse HEAD)" = "$REV" ] || { echo "argent upstream is not at $REV" >&2; exit 1; }
  for p in "${PATCHES[@]}"; do
    ug apply --whitespace=nowarn "$ROOT/$p"
    echo "applied $p" >&2
  done
  echo "$WANT" > "$UP/.git/kob-stamp"
  HAVE=$(tree_of_checkout)
  [ "$HAVE" = "$WANT" ] || { echo "argent/upstream is not $REV + patches after the reset (tree $HAVE, want $WANT)" >&2; exit 1; }
fi

if [ "$VERIFY" = 1 ]; then
  # The tree check above already ran; state what was verified.
  echo "argent/upstream is $REV + ${#PATCHES[@]} patches, nothing else (tree $WANT)" >&2
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
# --locked must not have rewritten anything in the checkout.
[ "$(tree_of_checkout)" = "$WANT" ] || { echo "argent/upstream changed during the build" >&2; exit 1; }
BIN=$TARGET/release/argentc
[ -x "$BIN" ] || BIN=$BIN.exe
echo "$BIN"
