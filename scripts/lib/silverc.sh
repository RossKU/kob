# Shared by scripts/build-contracts.sh and scripts/build-deploy.sh (sourced, run from the repo root).
#
#   resolve_silverc <upstream 0|1>   prints the pinned silverc to compile with:
#                                    $SILVERC as-is if set; with 1 the official v1.0.0 release binary
#                                    (downloaded, sha256-verified against contracts/silverc.lock);
#                                    else silverc built from vendor/silverscript
#   sha256 <file>                    hex digest

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
lock_get() { awk -v k="$1" '$1==k {print $2}' contracts/silverc.lock; }

fetch_upstream_silverc() {
  local os arch asset exe dir expected got
  os=$(uname -s); arch=$(uname -m)
  case "$os-$arch" in
    Linux-x86_64) asset=silverc-linux-x86_64.tar.gz ;;
    Linux-aarch64|Linux-arm64) asset=silverc-linux-arm64.tar.gz ;;
    Darwin-x86_64) asset=silverc-darwin-x86_64.tar.gz ;;
    Darwin-arm64) asset=silverc-darwin-arm64.tar.gz ;;
    MINGW*|MSYS*|CYGWIN*) asset=silverc-windows-x86_64.zip ;;
    *) echo "no upstream silverc release for $os-$arch" >&2; exit 2 ;;
  esac
  dir="$PWD/target/silverc-release"
  mkdir -p "$dir"
  if [ ! -f "$dir/$asset" ]; then
    curl -fsSL -o "$dir/$asset.part" "$(lock_get base_url)/$asset"
    mv "$dir/$asset.part" "$dir/$asset"
  fi
  expected=$(lock_get "$asset")
  got=$(sha256 "$dir/$asset")
  if [ "$got" != "$expected" ]; then
    echo "sha256 mismatch for $asset: expected $expected, got $got" >&2
    exit 1
  fi
  case "$asset" in
    *.zip) unzip -oq "$dir/$asset" -d "$dir/bin"; exe="$dir/bin/silverc.exe"
           got=$(sha256 "$exe"); [ "$got" = "$(lock_get silverc-windows-x86_64.exe)" ] \
             || { echo "sha256 mismatch for silverc.exe: $got" >&2; exit 1; } ;;
    *) mkdir -p "$dir/bin"; tar -xzf "$dir/$asset" -C "$dir/bin"; exe=$(find "$dir/bin" -type f -name silverc | head -n1) ;;
  esac
  chmod +x "$exe"
  echo "$exe"
}

resolve_silverc() {
  local exe
  if [ -n "${SILVERC:-}" ]; then
    echo "$SILVERC"
  elif [ "$1" = 1 ]; then
    fetch_upstream_silverc
  else
    cargo build --locked -q -p silverscript-lang --bin silverc >&2
    exe="${CARGO_TARGET_DIR:-$PWD/target}/debug/silverc"
    [ -x "$exe" ] || exe="$exe.exe"
    echo "$exe"
  fi
}
