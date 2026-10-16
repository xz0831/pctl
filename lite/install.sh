#!/bin/sh
# Offline install: check the files, then copy bin/pctl into a bin directory. No network, no root.
# Usage: sh install.sh [bin-dir]   (default: $HOME/.local/bin)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
dest=${1:-$HOME/.local/bin}
if command -v sha256sum >/dev/null; then sum="sha256sum"; else sum="shasum -a 256"; fi
(cd "$here" && $sum -c SHA256SUMS >/dev/null) || { echo "install: checksum mismatch; download the ZIP again" >&2; exit 1; }
mkdir -p "$dest"
cp "$here/bin/pctl" "$dest/pctl.new" && chmod 755 "$dest/pctl.new" && mv -f "$dest/pctl.new" "$dest/pctl"
echo "installed: $dest/pctl ($("$dest/pctl" --version))"
t=${PCTL_TMUX:-$(command -v tmux || true)}
if [ -z "$t" ]; then
  echo "warning: tmux not found; install tmux 3.8 or newer, or set PCTL_TMUX" >&2
else
  v=$("$t" -V | sed 's/^tmux //; s/^next-//')
  case $v in [0-2].* | 3.[0-7] | 3.[0-7][a-z]) echo "warning: $t is tmux $v; pctl needs 3.8 or newer" >&2 ;; esac
fi
case :$PATH: in *:"$dest":*) ;; *) echo "note: add $dest to PATH" ;; esac
echo "next: export PCTL_STANDALONE=1   (see pctl.env.example), then: bash $here/smoke.sh $dest/pctl"
