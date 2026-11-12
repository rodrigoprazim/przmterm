#!/usr/bin/env bash
# Builds the Debian / Ubuntu package from release binaries:
#
#   scripts/linux-deb.sh VERSION BIN_DIR OUT.deb
#
# BIN_DIR holds przmterm and przm. The package puts them in /usr/bin, with
# a menu entry and the icon; PrzmTerm updates it through `pkexec dpkg -i`.
set -euo pipefail

[[ $# -eq 3 ]] || { echo "usage: $0 VERSION BIN_DIR OUT.deb" >&2; exit 2; }
version=$1
bin=$2
out=$3
root=$(cd "$(dirname "$0")/.." && pwd)
id=io.github.rodrigoprazim.przmterm

# 1.0.0-rc.3 is 1.0.0~rc.3 for dpkg, so it sorts before 1.0.0.
deb_version=${version/-/\~}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
pkg=$work/przmterm

install -Dm755 "$bin/przmterm" "$pkg/usr/bin/przmterm"
install -Dm755 "$bin/przm" "$pkg/usr/bin/przm"
install -Dm644 "$root/packaging/linux/$id.desktop" "$pkg/usr/share/applications/$id.desktop"
install -Dm644 "$root/assets/icon-256.png" "$pkg/usr/share/icons/hicolor/256x256/apps/$id.png"
install -Dm644 "$root/assets/icon.svg" "$pkg/usr/share/icons/hicolor/scalable/apps/$id.svg"
install -Dm644 "$root/packaging/linux/copyright" "$pkg/usr/share/doc/przmterm/copyright"

# The C library it needs: the newest symbol version the programs use.
glibc=$(objdump -T "$bin/przmterm" "$bin/przm" | grep -o 'GLIBC_[0-9.]*' |
  sed 's/GLIBC_//' | sort -Vu | tail -1)
size=$(du -sk "$pkg/usr" | cut -f1)

mkdir -p "$pkg/DEBIAN"
cat >"$pkg/DEBIAN/control" <<CONTROL
Package: przmterm
Version: $deb_version
Architecture: amd64
Maintainer: Rodrigo Prazim <rodrigoprazim@users.noreply.github.com>
Installed-Size: $size
Depends: libc6 (>= $glibc), libgcc-s1
Recommends: libgl1 | libvulkan1, libxkbcommon-x11-0, libwayland-client0
Section: net
Priority: optional
Homepage: https://github.com/rodrigoprazim/przmterm
Description: terminal for network and systems engineers
 SSH, Telnet and serial consoles with a session tree and inherited
 settings, logging, buttons, scripts, SFTP and a password vault.
 A native program: no Electron, no web view.
CONTROL

dpkg-deb --root-owner-group --build "$pkg" "$out" >/dev/null
echo "$out: przmterm $deb_version (libc6 >= $glibc)"
