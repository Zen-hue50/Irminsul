#!/bin/sh
# Irminsul installer — private digital wellbeing tracking for Hyprland.
#
# This single script supports all three standard install patterns:
#
#   curl -fsSL https://raw.githubusercontent.com/Zen-hue50/Irminsul/main/install.sh | sh
#   git clone https://github.com/Zen-hue50/Irminsul && cd Irminsul && ./install.sh
#   ./install.sh            # from an extracted release archive or a source zip
#
# How it decides what to do:
#   1. If it sits next to an Irminsul checkout (or is run from one), it builds
#      that checkout.
#   2. Otherwise it tries to download a prebuilt binary matching your machine
#      from GitHub Releases (asset naming: irminsul-<arch>-<libc>.tar.gz).
#   3. Otherwise it downloads the source tarball (or clones with git) and
#      builds it. A missing Rust toolchain is installed user-locally via
#      rustup (no root). A missing C compiler is offered to install via the
#      distro package manager (needs sudo, with your consent).
#
# No root is needed for the normal path; the binary goes to ~/.local/bin and
# a marked autostart block is added to your Hyprland config (with a backup).
#
# To publish prebuilt binaries later, attach irminsul-x86_64-gnu.tar.gz /
# irminsul-x86_64-musl.tar.gz / irminsul-aarch64-*.tar.gz (each containing an
# `irminsul` binary at the archive root) to a GitHub Release. The installer
# then needs no Rust at all. See release.yml for a workflow that does this.

set -eu
umask 077

REPO_URL='https://github.com/Zen-hue50/Irminsul'
REF=${IRMINSUL_SOURCE_REF:-main}

usage() {
  cat <<'HELP'
Usage: install.sh [OPTIONS]

Installs Irminsul (digital wellbeing for Hyprland) into ~/.local/bin.
Works as a curl one-liner, from a git clone, or from an extracted archive.

Options:
  --hyprland-config FILE  Edit this hyprland.lua / hyprland.conf for autostart
  --no-autostart          Do not edit the Hyprland configuration
  --no-start              Do not start the daemon now
  --bin-dir DIR           Install the binary into DIR (default ~/.local/bin,
                          or set IRMINSUL_BIN_DIR)
  --build-from-source     Ignore prebuilt release binaries; compile locally
  --ref REF               Branch/tag to fetch when run via curl (default main,
                          or set IRMINSUL_SOURCE_REF)
  --yes, -y               Assume yes; install missing dependencies without asking
  --no-install-deps       Never install Rust or a C compiler; explain and exit
  --help, -h              Show this help

Environment: IRMINSUL_BIN_DIR, IRMINSUL_SOURCE_REF
HELP
}

msg()  { printf 'irminsul: %s\n' "$*"; }
fail() { printf 'irminsul: %s\n' "$*" >&2; exit 1; }

TMP_DIR=''
tmp=''
cfg_tmp=''
block_tmp=''
cleanup() {
  status=$?
  set +e
  for f in "$tmp" "$cfg_tmp" "$block_tmp"; do
    if [ -n "$f" ]; then rm -f -- "$f"; fi
  done
  if [ "$status" -ne 0 ] && [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
    printf 'irminsul: kept temporary files for inspection: %s\n' "$TMP_DIR" >&2
  elif [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
    rm -rf -- "$TMP_DIR"
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

have() { command -v "$1" >/dev/null 2>&1; }

fetch() { # fetch URL OUTFILE -> downloads, nonzero on failure
  if have curl; then curl -fsSL -o "$2" -- "$1"
  elif have wget; then wget -qO "$2" -- "$1"
  else return 2; fi
}

ver_ge() { # ver_ge A B -> true when dotted version A >= B
  awk -v a="$1" -v b="$2" 'BEGIN {
    n = split(a, A, "."); m = split(b, B, ".")
    for (i = 1; i <= 3; i++) {
      x = (i <= n ? A[i] + 0 : 0); y = (i <= m ? B[i] + 0 : 0)
      if (x > y) exit 0; if (x < y) exit 1
    }
    exit 0
  }'
}

can_prompt() { [ -r /dev/tty ] && [ -w /dev/tty ]; }

confirm() { # confirm QUESTION -> 0 when yes; assumes yes when non-interactive
  if [ "${assume_yes:-0}" -eq 1 ]; then return 0; fi
  if can_prompt; then
    printf '%s [Y/n] ' "$1" > /dev/tty
    ans=''
    IFS= read -r ans < /dev/tty || ans=''
    case "$ans" in ''|y|Y|yes|YES|Yes) return 0 ;; *) return 1 ;; esac
  fi
  msg '(non-interactive: assuming yes)'
  return 0
}

# ---------------------------------------------------------------- arguments
config=''
autostart=1
start_now=1
assume_yes=0
no_deps=0
build_src=0
bin_dir=${IRMINSUL_BIN_DIR:-}

while [ $# -gt 0 ]; do
  case "$1" in
    --hyprland-config)
      [ $# -ge 2 ] || fail 'missing --hyprland-config value'
      config=$2; shift 2 ;;
    --no-autostart) autostart=0; shift ;;
    --no-start) start_now=0; shift ;;
    --bin-dir)
      [ $# -ge 2 ] || fail 'missing --bin-dir value'
      bin_dir=$2; shift 2 ;;
    --build-from-source) build_src=1; shift ;;
    --ref)
      [ $# -ge 2 ] || fail 'missing --ref value'
      REF=$2; shift 2 ;;
    --yes|-y) assume_yes=1; shift ;;
    --no-install-deps) no_deps=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; fail "unknown argument: $1" ;;
  esac
done

# ------------------------------------------------------------ sanity checks
[ "$(id -u)" -ne 0 ] || fail 'run as your desktop user, not with sudo'
case "$HOME" in /*) ;; *) fail 'HOME must be set to an absolute path' ;; esac

config_home=${XDG_CONFIG_HOME:-$HOME/.config}
case "$config_home" in /*) ;; *) config_home=$HOME/.config ;; esac
state_home=${XDG_STATE_HOME:-$HOME/.local/state}
case "$state_home" in /*) ;; *) state_home=$HOME/.local/state ;; esac

[ -n "$bin_dir" ] || bin_dir=$HOME/.local/bin
case "$bin_dir" in /*) ;; *) fail 'the binary directory must be an absolute path' ;; esac
bin=${bin_dir%/}/irminsul
# A literal newline, so we can test for one inside $bin.
nl='
'
case "$bin" in *"$nl"*) fail 'unsupported newline in installation path' ;; esac

# Validate the Hyprland arguments up front, before anything is downloaded.
format=''
if [ "$autostart" -eq 1 ]; then
  if [ -z "$config" ]; then
    lua=$config_home/hypr/hyprland.lua
    conf=$config_home/hypr/hyprland.conf
    if [ -f "$lua" ]; then
      config=$lua
      if [ -f "$conf" ]; then
        msg "Both Lua and Hyprlang configs exist; using $lua. Override with --hyprland-config FILE."
      fi
    elif [ -f "$conf" ]; then
      config=$conf
    fi
  fi
  if [ -z "$config" ] || [ ! -f "$config" ] || [ ! -w "$config" ]; then
    fail 'could not find a writable Hyprland config; pass --hyprland-config FILE or --no-autostart'
  fi
  case "$config" in
    *.lua) format=lua ;;
    *.conf) format=conf ;;
    *) fail 'the Hyprland config filename must end in .lua or .conf' ;;
  esac
  if [ "$format" = conf ]; then
    case "$bin" in *'#'*|*'$'*)
      fail 'Hyprlang treats # and $ specially; choose a simpler IRMINSUL_BIN_DIR'
    ;; esac
  fi
fi

# ------------------------------------------------------------- platform info
arch=''
case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
esac
machine_libc=gnu
if ldd --version 2>/dev/null | grep -qi musl; then machine_libc=musl; fi

ensure_tmp() {
  if [ -z "$TMP_DIR" ]; then
    TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/irminsul-install.XXXXXXXX")
  fi
}

# ------------------------------------------------------- locate or get source
src_dir=''
if [ -f "$0" ]; then
  script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" 2>/dev/null && pwd -P) || script_dir=''
  if [ -n "$script_dir" ] && [ -f "$script_dir/Cargo.toml" ] && [ -f "$script_dir/src/main.rs" ]; then
    src_dir=$script_dir
  fi
fi
if [ -z "$src_dir" ] && [ -f "$PWD/Cargo.toml" ] && [ -f "$PWD/src/main.rs" ]; then
  src_dir=$PWD
fi

built=''
mode=''

if [ -n "$src_dir" ]; then
  mode=source
  msg "Using the Irminsul source in $src_dir"
fi

# Try a prebuilt binary when we have no checkout (curl one-liner case).
if [ -z "$mode" ] && [ "$build_src" -eq 0 ] && [ -n "$arch" ]; then
  ensure_tmp
  for libc_try in "$machine_libc" "$( [ "$machine_libc" = gnu ] && echo musl || echo gnu )"; do
    asset="irminsul-$arch-$libc_try.tar.gz"
    url="$REPO_URL/releases/latest/download/$asset"
    msg "Looking for a prebuilt binary: $asset"
    if fetch "$url" "$TMP_DIR/asset.tar.gz" 2>/dev/null && tar -xzf "$TMP_DIR/asset.tar.gz" -C "$TMP_DIR" 2>/dev/null; then
      found=$(find "$TMP_DIR" -maxdepth 2 -type f -name irminsul -print 2>/dev/null | head -n 1)
      if [ -n "$found" ]; then
        chmod 0755 -- "$found"
        built=$found
        mode=binary
        msg "Downloading a prebuilt binary (no Rust toolchain needed)."
        break
      fi
    fi
    rm -rf -- "$TMP_DIR"
    TMP_DIR=''
    ensure_tmp
  done
  if [ "$mode" != binary ]; then
    msg "No prebuilt binary available; installing from source."
  fi
fi

# Fetch the source archive when there is no checkout and no release binary.
if [ -z "$mode" ]; then
  ensure_tmp
  mkdir -p -- "$TMP_DIR/source"
  if fetch "$REPO_URL/archive/$REF.tar.gz" "$TMP_DIR/source.tgz"; then
    tar -xzf "$TMP_DIR/source.tgz" -C "$TMP_DIR/source"
    first=$(find "$TMP_DIR/source" -mindepth 2 -maxdepth 2 -name Cargo.toml -print 2>/dev/null | head -n 1)
    [ -n "$first" ] || fail "the downloaded source archive does not look like Irminsul"
    src_dir=$(dirname -- "$first")
    mode=source
    msg "Downloaded the Irminsul source (ref: $REF)."
  elif have git; then
    msg "Downloading the source archive failed; cloning with git instead."
    git clone --depth 1 "$REPO_URL" "$TMP_DIR/source/git"
    [ -f "$TMP_DIR/source/git/Cargo.toml" ] || fail "the cloned repository does not look like Irminsul"
    src_dir=$TMP_DIR/source/git
    mode=source
  else
    fail "could not download the source archive (need curl or wget for $REPO_URL)"
  fi
fi

# ---------------------------------------------------------------- toolchain
if [ "$mode" = source ]; then
  PATH="$HOME/.cargo/bin:$PATH"
  export PATH
  hash -r 2>/dev/null || :

  rust_ok=0
  if have cargo && have rustc; then
    rv=$(rustc --version 2>/dev/null | sed -e 's/^rustc \([0-9.]*\).*/\1/')
    if ver_ge "$rv" 1.85; then
      rust_ok=1
    else
      msg "Found Rust $rv, but Irminsul needs Rust 1.85+ (edition 2024)."
    fi
  fi

  if [ "$rust_ok" -eq 0 ]; then
    if [ "$no_deps" -eq 1 ]; then
      fail 'Rust 1.85+ is required but not usable. Install it from https://rustup.rs and rerun.'
    fi
    if have rustup; then
      msg 'Installing the current stable Rust toolchain with rustup (user-local).'
      rustup toolchain install stable --profile minimal || fail 'rustup could not install the stable toolchain'
      RUSTUP_TOOLCHAIN=stable
      export RUSTUP_TOOLCHAIN
    else
      if confirm 'Irminsul is written in Rust. Install a user-local Rust toolchain (~/.cargo and ~/.rustup, no root) now?'; then
        ensure_tmp
        msg 'Downloading rustup (https://rustup.rs)...'
        fetch https://sh.rustup.rs "$TMP_DIR/rustup-init.sh" || fail 'could not download rustup; install Rust 1.85+ from https://rustup.rs and rerun'
        sh "$TMP_DIR/rustup-init.sh" -y --profile minimal --default-toolchain stable || fail 'the rustup installer failed'
      else
        fail 'Rust 1.85+ is required. Install it from https://rustup.rs and rerun.'
      fi
    fi
    hash -r 2>/dev/null || :
    have cargo || fail 'cargo is still unavailable after the Rust setup; install Rust 1.85+ from https://rustup.rs and rerun'
    rv=$(rustc --version 2>/dev/null | sed -e 's/^rustc \([0-9.]*\).*/\1/')
    ver_ge "$rv" 1.85 || fail "Rust $rv is too old for Irminsul (needs 1.85+, edition 2024)."
  fi

  # A C compiler is needed because SQLite is bundled and compiled locally.
  cc_found=''
  for c in cc gcc clang; do
    if have "$c"; then cc_found=$c; break; fi
  done
  if [ -z "$cc_found" ]; then
    hint=''
    if have apt-get; then hint='sudo apt-get update && sudo apt-get install -y gcc'
    elif have dnf; then hint='sudo dnf install -y gcc'
    elif have pacman; then hint='sudo pacman -S --needed base-devel'
    elif have zypper; then hint='sudo zypper install -y gcc'
    elif have apk; then hint='sudo apk add gcc musl-dev'
    else hint='install a C compiler (gcc) with your package manager'
    fi
    if [ "$no_deps" -eq 1 ]; then
      fail "no C compiler found (cc/gcc/clang). SQLite is bundled and compiled at build time. Run: $hint"
    fi
    if ! can_prompt; then
      fail "no C compiler found (cc/gcc/clang). SQLite is compiled at build time. Run: $hint"
    fi
    if confirm "No C compiler found. Run '$hint' now (asks for your sudo password)?"; then
      msg "Running: $hint"
      sh -c "$hint" || fail "could not install a C compiler; run it manually and rerun this installer"
      for c in cc gcc clang; do
        if have "$c"; then cc_found=$c; break; fi
      done
      [ -n "$cc_found" ] || fail "no C compiler found even after installation; run it manually and rerun"
    else
      fail "a C compiler is required. Run: $hint"
    fi
  fi

  msg 'Building Irminsul from source (this can take a few minutes)...'
  cargo build --locked --release --manifest-path "$src_dir/Cargo.toml" --target-dir "$src_dir/target"
  built=$src_dir/target/release/irminsul
fi

# ------------------------------------------------------------ install binary
[ -n "$built" ] || fail 'internal error: no binary to install'
if [ "$mode" = binary ]; then
  "$built" paths >/dev/null 2>&1 || fail 'the downloaded binary does not run on this machine; rerun with --build-from-source'
fi

"$built" migrate
"$built" init

mkdir -p -- "$bin_dir"
tmp=$(mktemp "$bin_dir/.irminsul.XXXXXXXX")
install -m 0755 -- "$built" "$tmp"
mv -fT -- "$tmp" "$bin" || fail "could not place the binary at $bin"
tmp=''

# ------------------------------------------------------ Hyprland autostart
if [ "$autostart" -eq 1 ]; then
  prefix='#'
  [ "$format" != lua ] || prefix='--'
  begin="$prefix BEGIN IRMINSUL AUTOSTART"
  finish="$prefix END IRMINSUL AUTOSTART"
  old_begin="$prefix BEGIN STILLTIME AUTOSTART"
  old_finish="$prefix END STILLTIME AUTOSTART"
  # Quote for the /bin/sh command that Hyprland starts.
  shell_bin=$(printf '%s' "$bin" | sed -e "s/'/'\\\\''/g")
  cmd="'$shell_bin' run"
  if [ "$format" = lua ]; then
    lua_cmd=$(printf '%s' "$cmd" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')
    snippet=$(printf 'hl.on("hyprland.start", function()\n    hl.exec_cmd("%s")\nend)' "$lua_cmd")
  else
    snippet="exec-once = $cmd"
  fi

  real_config=$(CDPATH='' cd -- "$(dirname -- "$config")" && pwd -P)/$(basename -- "$config")
  cfg_tmp=$(mktemp "$(dirname -- "$real_config")/.irminsul-autostart.XXXXXXXX")
  block_tmp=$(mktemp "$(dirname -- "$real_config")/.irminsul-block.XXXXXXXX")
  printf '%s\n%s\n%s\n' "$begin" "$snippet" "$finish" > "$block_tmp"
  cp -p -- "$real_config" "$cfg_tmp"
  # Replace only installer-managed markers, in place. Validate before altering
  # the real file, and leave a dotfile symlink at the user's original path.
  awk -v begin="$begin" -v finish="$finish" -v old_begin="$old_begin" -v old_finish="$old_finish" -v block="$block_tmp" '
    function emit(    item) { while ((getline item < block) > 0) print item; close(block); wrote=1 }
    {
      line=$0; sub(/\r$/, "", line)
      if (line == begin || line == old_begin) {
        if (expected != "") { broken=1; exit }
        expected=(line == begin ? finish : old_finish)
        if (!wrote) emit()
        next
      }
      if (line == finish || line == old_finish) {
        if (line != expected) { broken=1; exit }
        expected=""
        next
      }
      if (expected == "") print $0
    }
    END {
      if (broken || expected != "") exit 2
      if (!wrote) { if (NR) print ""; emit() }
    }
  ' "$real_config" > "$cfg_tmp" || fail "the managed autostart block in $config is incomplete; the configuration was not edited"
  if ! cmp -s -- "$real_config" "$cfg_tmp"; then
    backup=$config.irminsul.bak
    had_old=0
    if grep -Fqx -- "$old_begin" "$real_config"; then had_old=1; fi
    if [ ! -e "$backup" ] && [ ! -L "$backup" ]; then cp -p -- "$real_config" "$backup"; fi
    mv -fT -- "$cfg_tmp" "$real_config"
    cfg_tmp=''
    msg "Enabled Irminsul autostart in $config"
    msg "Backup: $backup"
    if [ "$had_old" -eq 1 ]; then
      old_bin=${bin_dir%/}/stilltime
      backup_bin=$state_home/irminsul/previous-executable
      if [ -f "$old_bin" ] && [ -x "$old_bin" ] && [ ! -e "$backup_bin" ] && [ ! -L "$backup_bin" ]; then
        mkdir -p -- "$state_home/irminsul"
        mv -nT -- "$old_bin" "$backup_bin"
        msg "Saved the previous executable outside PATH: $backup_bin"
      fi
    fi
  else
    msg "Kept existing Irminsul autostart in $config"
  fi
fi

# ------------------------------------------------------------- start daemon
started_now=0
if [ "$start_now" -eq 1 ] && [ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ] \
   && [ -n "${XDG_RUNTIME_DIR:-}" ] \
   && [ -S "$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock" ]; then
  mkdir -p -- "$state_home/irminsul"
  log=$state_home/irminsul/daemon.log
  nohup "$bin" run </dev/null >>"$log" 2>&1 &
  started_now=1
  msg "Start requested. Log: $log"
elif [ "$autostart" -eq 1 ]; then
  msg 'Irminsul will start on the next Hyprland login.'
elif [ "$start_now" -eq 1 ]; then
  msg "To start tracking, run in a Hyprland terminal: $bin run"
fi

# A running daemon keeps the old binary until it is restarted.
if [ "$started_now" -eq 0 ] && have pgrep && pgrep -x irminsul >/dev/null 2>&1; then
  msg 'An irminsul process is still running the previous binary.'
  msg 'Restart it: pgrep -a -x irminsul, kill -TERM PID, then: '"$bin"' run'
fi

msg "Installed: $bin"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) msg "NOTE: $bin_dir is not on your PATH; add it to your shell profile."
     msg "      Until then use the full path: $bin"
     ;;
esac
msg 'Open the dashboard at http://127.0.0.1:8765/ once the daemon runs.'
