#!/usr/bin/env bash
# User-local installer; run this from a Hyprland terminal for immediate capture.
set -euo pipefail
umask 077

usage() {
  cat <<'HELP'
Usage: ./install.sh [--hyprland-config FILE] [--no-autostart] [--no-start]
  --hyprland-config FILE  Use a specific hyprland.lua or hyprland.conf
  --no-autostart           Do not change the Hyprland configuration
  --no-start               Wait for the next Hyprland login to start tracking
  IRMINSUL_BIN_DIR        Override the absolute install path (default ~/.local/bin)
HELP
}
fail() { printf 'irminsul: %s\n' "$*" >&2; exit 1; }

source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
config=''
autostart=1
start_now=1
while (( $# )); do
  case "$1" in
    --hyprland-config) (( $# >= 2 )) || fail 'missing --hyprland-config value'; config=$2; shift 2 ;;
    --no-autostart) autostart=0; shift ;;
    --no-start) start_now=0; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; fail "unknown argument: $1" ;;
  esac
done
[[ ${EUID} -ne 0 ]] || fail 'run as your desktop user, not with sudo'
[[ ${HOME:-} = /* ]] || fail 'HOME must be set to an absolute path'
command -v cargo >/dev/null || fail 'install Rust/Cargo 1.85 or newer first'
command -v cc >/dev/null || fail 'install a C compiler (e.g. gcc); SQLite is bundled but must be compiled'

config_home=${XDG_CONFIG_HOME:-$HOME/.config}
[[ $config_home = /* ]] || config_home=$HOME/.config
state_home=${XDG_STATE_HOME:-$HOME/.local/state}
[[ $state_home = /* ]] || state_home=$HOME/.local/state
bin_dir=${IRMINSUL_BIN_DIR:-$HOME/.local/bin}
[[ $bin_dir = /* ]] || fail 'IRMINSUL_BIN_DIR must be absolute'
bin=${bin_dir%/}/irminsul
[[ $bin != *$'\n'* && $bin != *$'\r'* ]] || fail 'unsupported newline in installation path'
format=''
if (( autostart )); then
  if [[ -z $config ]]; then
    lua=$config_home/hypr/hyprland.lua
    conf=$config_home/hypr/hyprland.conf
    if [[ -f $lua ]]; then
      config=$lua
      if [[ -f $conf ]]; then
        printf 'Both Lua and Hyprlang configs exist; using %s. Override with --hyprland-config FILE.\n' "$lua"
      fi
    elif [[ -f $conf ]]; then config=$conf
    fi
  fi
  [[ -n $config && -f $config && -w $config ]] || fail 'could not find a writable Hyprland config; pass --hyprland-config FILE or --no-autostart'
  case "$config" in
    *.lua) format=lua ;;
    *.conf) format=conf ;;
    *) fail 'the Hyprland config filename must end in .lua or .conf' ;;
  esac
  if [[ $format == conf && ( $bin == *'#'* || $bin == *'$'* ) ]]; then
    fail 'Hyprlang treats # and $ specially; choose a simpler IRMINSUL_BIN_DIR'
  fi
fi

cargo build --locked --release --manifest-path "$source_dir/Cargo.toml" --target-dir "$source_dir/target"
built=$source_dir/target/release/irminsul
"$built" migrate
"$built" init
mkdir -p -- "$bin_dir"
tmp=''
cfg_tmp=''
block_tmp=''
trap 'rm -f -- "$tmp" "$cfg_tmp" "$block_tmp"' EXIT
tmp=$(mktemp -- "$bin_dir/.irminsul.XXXXXXXX")
install -m 0755 -- "$built" "$tmp"
mv -fT -- "$tmp" "$bin"

if (( autostart )); then
  prefix='#'
  [[ $format != lua ]] || prefix='--'
  begin="$prefix BEGIN IRMINSUL AUTOSTART"
  finish="$prefix END IRMINSUL AUTOSTART"
  old_begin="$prefix BEGIN STILLTIME AUTOSTART"
  old_finish="$prefix END STILLTIME AUTOSTART"
  # Quote for the /bin/sh command that Hyprland starts.
  shell_bin="'${bin//\'/\'\\\'\'}'"
  cmd="$shell_bin run"
  if [[ $format == lua ]]; then
    lua_cmd=${cmd//\\/\\\\}
    lua_cmd=${lua_cmd//\"/\\\"}
    snippet=$(printf 'hl.on("hyprland.start", function()\n    hl.exec_cmd("%s")\nend)' "$lua_cmd")
  else
    snippet="exec-once = $cmd"
  fi

  real_config=$(realpath -e -- "$config")
  cfg_tmp=$(mktemp -- "$(dirname -- "$real_config")/.irminsul-autostart.XXXXXXXX")
  block_tmp=$(mktemp -- "$(dirname -- "$real_config")/.irminsul-block.XXXXXXXX")
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
    grep -Fqx -- "$old_begin" "$real_config" && had_old=1
    if [[ ! -e $backup && ! -L $backup ]]; then cp -p -- "$real_config" "$backup"; fi
    mv -fT -- "$cfg_tmp" "$real_config"
    printf 'Enabled Irminsul autostart in %s\nBackup: %s\n' "$config" "$backup"
    if (( had_old )); then
      old_bin=${bin_dir%/}/stilltime
      backup_bin=$state_home/irminsul/previous-executable
      if [[ -f $old_bin && -x $old_bin && ! -e $backup_bin && ! -L $backup_bin ]]; then
        mkdir -p -- "$state_home/irminsul"
        mv -nT -- "$old_bin" "$backup_bin"
        printf 'Saved the previous executable outside PATH: %s\n' "$backup_bin"
      fi
    fi
  else
    printf 'Kept existing Irminsul autostart in %s\n' "$config"
  fi
fi

if (( start_now )) && [[ -n ${HYPRLAND_INSTANCE_SIGNATURE:-} && -n ${XDG_RUNTIME_DIR:-} && -S $XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock ]]; then
  mkdir -p -- "$state_home/irminsul"
  log=$state_home/irminsul/daemon.log
  nohup "$bin" run </dev/null >>"$log" 2>&1 &
  printf 'Start requested. Log: %s\n' "$log"
elif (( autostart )); then
  printf 'Irminsul will start on the next Hyprland login. To run manually: %q run\n' "$bin"
else
  printf 'To start tracking, run in a Hyprland terminal: %q run\n' "$bin"
fi
printf 'Installed: %s\n' "$bin"
