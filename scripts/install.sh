#!/usr/bin/env sh
# First-run installer for macOS/Linux (4.8). Run from inside an extracted
# release archive that contains the open-string binary alongside this
# script. Copies the binary into a per-user install directory, creates
# the config directory Core writes to on first launch, and adds the
# install directory to PATH via the user's shell rc file if it isn't
# there already.
set -eu

total_steps=5

if [ -t 1 ]; then
    c_cyan='\033[36m'; c_green='\033[32m'; c_yellow='\033[33m'; c_magenta='\033[35m'; c_reset='\033[0m'
else
    c_cyan=''; c_green=''; c_yellow=''; c_magenta=''; c_reset=''
fi

step() {
    printf '%b[%s/%s]%b %s\n' "$c_cyan" "$1" "$total_steps" "$c_reset" "$2"
}
info() {
    printf '%b  %s%b\n' "$c_green" "$1" "$c_reset"
}
warn() {
    printf '%b  %s%b\n' "$c_yellow" "$1" "$c_reset"
}

printf '%b== Open String installer ==%b\n' "$c_magenta" "$c_reset"

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
source_bin="$script_dir/open-string"

step 1 "Locating open-string next to this script..."
if [ ! -f "$source_bin" ]; then
    echo "error: open-string was not found next to this script ($script_dir). Run this installer from inside the extracted release archive." >&2
    exit 1
fi
info "found: $source_bin"

install_dir="$HOME/.local/share/open-string/bin"

case "$(uname -s)" in
    Darwin) config_dir="$HOME/Library/Application Support/open-string" ;;
    *) config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/open-string" ;;
esac

step 2 "Creating install and config directories..."
mkdir -p "$install_dir" "$config_dir"
info "install dir: $install_dir"
info "config dir:  $config_dir"

step 3 "Copying binary..."
cp "$source_bin" "$install_dir/open-string"
chmod +x "$install_dir/open-string"
info "copied to $install_dir/open-string"

step 4 "Updating PATH..."
path_line="export PATH=\"\$PATH:$install_dir\""

rc_file=""
case "${SHELL:-}" in
    */zsh) rc_file="$HOME/.zshrc" ;;
    */bash) rc_file="$HOME/.bashrc" ;;
    *) rc_file="$HOME/.profile" ;;
esac

if [ -f "$rc_file" ] && grep -qF "$install_dir" "$rc_file" 2>/dev/null; then
    warn "$install_dir is already configured in $rc_file"
else
    printf '\n# Added by the Open String installer\n%s\n' "$path_line" >> "$rc_file"
    info "added $install_dir to PATH in $rc_file (restart your shell, or run 'source $rc_file')"
fi

step 5 "Verifying installation..."
installed_version=$("$install_dir/open-string" --version 2>/dev/null || true)
if [ -n "$installed_version" ]; then
    info "$installed_version"
else
    warn "could not run $install_dir/open-string --version to confirm the install"
fi

printf '\n%b== Installation complete ==%b\n' "$c_magenta" "$c_reset"
echo "  binary:      $install_dir/open-string"
echo "  config/logs: $config_dir"
echo "  next step:   open a new shell and run 'open-string auth login'"
