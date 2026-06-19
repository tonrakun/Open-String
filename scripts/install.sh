#!/usr/bin/env sh
# First-run installer for macOS/Linux (4.8). Run from inside an extracted
# release archive that contains the open-string binary alongside this
# script. Copies the binary into a per-user install directory, creates
# the config directory Core writes to on first launch, and adds the
# install directory to PATH via the user's shell rc file if it isn't
# there already.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
source_bin="$script_dir/open-string"

if [ ! -f "$source_bin" ]; then
    echo "error: open-string was not found next to this script ($script_dir). Run this installer from inside the extracted release archive." >&2
    exit 1
fi

install_dir="$HOME/.local/share/open-string/bin"

case "$(uname -s)" in
    Darwin) config_dir="$HOME/Library/Application Support/open-string" ;;
    *) config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/open-string" ;;
esac

mkdir -p "$install_dir" "$config_dir"
cp "$source_bin" "$install_dir/open-string"
chmod +x "$install_dir/open-string"

path_line="export PATH=\"\$PATH:$install_dir\""

rc_file=""
case "${SHELL:-}" in
    */zsh) rc_file="$HOME/.zshrc" ;;
    */bash) rc_file="$HOME/.bashrc" ;;
    *) rc_file="$HOME/.profile" ;;
esac

if [ -f "$rc_file" ] && grep -qF "$install_dir" "$rc_file" 2>/dev/null; then
    echo "$install_dir is already configured in $rc_file."
else
    printf '\n# Added by the Open String installer\n%s\n' "$path_line" >> "$rc_file"
    echo "Added $install_dir to PATH in $rc_file. Restart your shell (or 'source $rc_file') for it to take effect."
fi

echo "Open String installed to $install_dir/open-string"
echo "Config/audit log directory: $config_dir"
echo "Run 'open-string auth login' in a new shell to get started."
