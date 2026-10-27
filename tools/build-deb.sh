#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Slavick
# SPDX-License-Identifier: MIT

# Native Ubuntu build. Nothing is installed on the build machine.
set -euo pipefail
export LC_ALL=C
project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$project_dir"
if [[ ${1:-} == --help ]]; then
  echo 'Build: bash tools/build-deb.sh'
  echo 'Optional environment: CARGO_NET_OFFLINE=true, FINDER_GNOME_DEB_OUTPUT_DIR=directory, SOURCE_DATE_EPOCH=timestamp'
  exit 0
fi
if (( $# != 0 )); then echo 'No positional arguments are accepted. Use --help.' >&2; exit 2; fi
for tool in cargo rustc python3 dpkg-deb dpkg-shlibdeps dpkg-architecture desktop-file-validate strip gzip; do
  command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 1; }
done
read -r distro release < <(python3 - <<'PY'
import platform
os = platform.freedesktop_os_release()
print(os['ID'], os['VERSION_ID'])
PY
)
if [[ $distro != ubuntu || ! $release =~ ^[0-9]+\.[0-9]+$ ]] || (( ${release%%.*} < 24 )); then
  echo 'Build the package natively on Ubuntu 24.04 or newer (or in a matching Ubuntu container).' >&2
  exit 1
fi
rust_host=$(rustc -vV | sed -n 's/^host: //p')
if [[ -n ${CARGO_BUILD_TARGET:-} && $CARGO_BUILD_TARGET != "$rust_host" ]]; then
  echo 'Cross-compilation is not supported by this native Debian packager.' >&2; exit 1
fi
version=$(python3 - <<'PY'
import tomllib
print(tomllib.load(open('Cargo.toml', 'rb'))['package']['version'])
PY
)
architecture=$(dpkg --print-architecture)
package_version="${version}-1ubuntu${release}"
output_dir=${FINDER_GNOME_DEB_OUTPUT_DIR:-"$project_dir/dist"}
mkdir -p -- "$output_dir"
output_dir=$(cd -- "$output_dir" && pwd)
work=$(mktemp -d -t finder-gnome-deb.XXXXXXXX)
trap 'rm -rf -- "$work"' EXIT
stage="$work/package"
mkdir -p "$stage/DEBIAN" "$stage/usr/bin" "$stage/usr/share/applications" "$stage/usr/share/doc/finder-gnome"
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(stat -c %Y Cargo.lock)}
if [[ ! $SOURCE_DATE_EPOCH =~ ^[0-9]+$ ]]; then echo 'SOURCE_DATE_EPOCH must be a non-negative integer.' >&2; exit 1; fi
cargo build --release --locked --package finder-gnome --bin finder-gnome --message-format=json-render-diagnostics > "$work/build.jsonl"
binary=$(python3 - "$work/build.jsonl" <<'PY'
import json, sys
executables = []
for line in open(sys.argv[1]):
    artifact = json.loads(line)
    if artifact.get('reason') == 'compiler-artifact' and artifact.get('target', {}).get('name') == 'finder-gnome' and artifact.get('executable'):
        executables.append(artifact['executable'])
if not executables:
    raise SystemExit('Cargo did not report the finder-gnome executable')
print(executables[-1])
PY
)
install -m 755 "$binary" "$stage/usr/bin/finder-gnome"
strip --strip-unneeded "$stage/usr/bin/finder-gnome"
desktop-file-validate resources/io.github.finder_gnome.desktop
install -m 644 resources/io.github.finder_gnome.desktop "$stage/usr/share/applications/"
for size in 32 48 64 128 256 512; do
  icon_dir="$stage/usr/share/icons/hicolor/${size}x${size}/apps"
  install -d "$icon_dir"
  install -m 644 "resources/icons/${size}x${size}/apps/io.github.finder_gnome.png" "$icon_dir/"
done
install -m 644 packaging/copyright LICENSE "$stage/usr/share/doc/finder-gnome/"
gzip -n -9 -c README.md > "$stage/usr/share/doc/finder-gnome/README.md.gz"
# Preserve upstream notices for bundled Rust dependencies, including build-only crates.
cargo metadata --locked --filter-platform "$rust_host" --format-version=1 > "$work/metadata.json"
python3 - "$work/metadata.json" "$stage/usr/share/doc/finder-gnome" <<'PY'
import json, pathlib, shutil, sys
metadata = json.load(open(sys.argv[1]))
output = pathlib.Path(sys.argv[2]) / 'third-party'
output.mkdir()
summary = []
for package in sorted(metadata['packages'], key=lambda p: (p['name'], p['version'])):
    if package['name'] == 'finder-gnome':
        continue
    summary.append(f"{package['name']} {package['version']}: {package.get('license') or 'See upstream notice'}")
    source = pathlib.Path(package['manifest_path']).parent
    notices = {p for pattern in ('LICENSE*', 'LICENCE*', 'COPYING*', 'NOTICE*') for p in source.glob(pattern) if p.is_file()}
    if package.get('license_file'):
        notices.add(source / package['license_file'])
    target = output / f"{package['name']}-{package['version']}"
    for notice in sorted(notices):
        if notice.is_file():
            target.mkdir(exist_ok=True)
            shutil.copyfile(notice, target / notice.name)
(output / 'licenses.txt').write_text('\n'.join(summary) + '\n')
PY
# Compile any completed translations; the English source UI needs no catalog.
for catalog in po/*.po; do
  [[ -e $catalog ]] || continue
  command -v msgfmt >/dev/null || { echo 'Install gettext to compile translations.' >&2; exit 1; }
  language=$(basename "$catalog" .po)
  locale_dir="$stage/usr/share/locale/$language/LC_MESSAGES"
  install -d "$locale_dir"
  msgfmt --check "$catalog" -o "$locale_dir/finder-gnome.mo"
done
# dpkg-shlibdeps reads installed library symbol metadata for this Ubuntu release.
mkdir -p "$work/debian"
cat > "$work/debian/control" <<'CONTROL'
Source: finder-gnome
Section: utils
Priority: optional
Maintainer: Slavick <slavick@localhost>

Package: finder-gnome
Architecture: any
Description: Finder-inspired file manager for GNOME
CONTROL
shlibs=$(cd "$work" && dpkg-shlibdeps -O -e"$stage/usr/bin/finder-gnome")
depends=${shlibs#shlibs:Depends=}
[[ -n $depends && $depends != "$shlibs" ]] || { echo 'Unable to determine shared-library dependencies.' >&2; exit 1; }
recommends='gvfs-fuse, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-libav'
if [[ $release == 24.04 ]]; then recommends+=', libgtk-4-media-gstreamer'; fi
installed_size=$(du -sk "$stage/usr" | cut -f1)
cat > "$stage/DEBIAN/control" <<CONTROL
Package: finder-gnome
Version: $package_version
Section: utils
Priority: optional
Architecture: $architecture
Maintainer: Slavick <slavick@localhost>
Installed-Size: $installed_size
Depends: $depends, desktop-file-utils, hicolor-icon-theme, gvfs, gvfs-backends, file-roller
Recommends: $recommends
Description: Finder-inspired file manager for GNOME
 GTK4 and libadwaita file manager with tabs, multiple views, previews,
 network folders, trash, tags and background file operations.
CONTROL
(cd "$stage" && find usr -type f -print0 | sort -z | xargs -0 md5sum > DEBIAN/md5sums)
find "$stage" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
package="$output_dir/finder-gnome_${package_version}_${architecture}.deb"
dpkg-deb --root-owner-group --build -Zxz "$stage" "$package"
printf 'Package ready: %s\n' "$package"
