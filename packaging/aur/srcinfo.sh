#!/usr/bin/env bash
# Print .SRCINFO for a PKGBUILD without makepkg (CI runners aren't Arch), by sourcing it.
# Covers the fields our PKGBUILD uses. Usage: srcinfo.sh path/to/PKGBUILD > .SRCINFO
set -euo pipefail
# shellcheck disable=SC1090
source "$1"

field() { local k=$1; shift; for v in "$@"; do [ -n "$v" ] && printf '\t%s = %s\n' "$k" "$v"; done; return 0; }

echo "pkgbase = $pkgname"
field pkgdesc "$pkgdesc"
field pkgver "$pkgver"
field pkgrel "$pkgrel"
field url "$url"
field arch "${arch[@]}"
field license "${license[@]}"
field depends "${depends[@]}"
field optdepends "${optdepends[@]}"
field provides "${provides[@]}"
field conflicts "${conflicts[@]}"
field options "${options[@]}"
for a in "${arch[@]}"; do
  src="source_$a[@]"; sums="sha256sums_$a[@]"
  field "source_$a" "${!src}"
  field "sha256sums_$a" "${!sums}"
done
echo
echo "pkgname = $pkgname"
