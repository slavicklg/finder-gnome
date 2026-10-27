#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Slavick
# SPDX-License-Identifier: MIT

set -Eeuo pipefail
export LC_ALL=C

readonly REPO_URI='https://repo.web7dev.com'
readonly GPG_KEY_URL='https://repo.web7dev.com/finder-gnome.gpg'
readonly GPG_FINGERPRINT='F550EC3355FD905A3B39D4B5679BD5860B6EECE1'
readonly OS_RELEASE_FILE='/etc/os-release'
readonly APT_DIR='/etc/apt'
readonly KEYRING="$APT_DIR/keyrings/finder-gnome.gpg"
readonly SOURCE="$APT_DIR/sources.list.d/finder-gnome.sources"

die() { printf 'Error: %s\n' "$*" >&2; exit 1; }

detect_platform() {
    local ID='' VERSION_ID=''
    [[ -r $1 ]] || die "Cannot read $1."
    # /etc/os-release is a trusted system file, not a downloaded configuration.
    # shellcheck disable=SC1090
    source "$1"
    [[ $ID == ubuntu ]] || die 'Only Ubuntu 24.04 and 26.04 are supported.'
    ubuntu_version=$VERSION_ID
    case $VERSION_ID in
        24.04) suite=noble ;;
        26.04) suite=resolute ;;
        *) die "Unsupported Ubuntu version: $VERSION_ID (expected 24.04 or 26.04)." ;;
    esac
    command -v dpkg >/dev/null || die 'dpkg is required.'
    architecture=$(dpkg --print-architecture) || die 'Cannot determine the Debian architecture.'
    [[ $architecture == amd64 ]] || die "Unsupported architecture: $architecture (expected amd64)."
}

check_sources() {
    local file uri
    [[ ! -L $SOURCE && ! -L $KEYRING ]] || die 'Refusing to replace a symlink at the installer source/keyring path.'
    # Be conservative: conflicting entries, including disabled/commented entries,
    # require manual review. Never edit someone else's APT configuration.
    for file in "$APT_DIR/sources.list" "$APT_DIR/sources.list.d/"*.list "$APT_DIR/sources.list.d/"*.sources; do
        [[ -f $file ]] || continue
        [[ -r $file ]] || die "Cannot read existing APT source: $file."
        if [[ $file == "$SOURCE" ]]; then
            uri=$(awk '/^URIs:/ { sub(/^URIs:[[:space:]]*/, ""); print }' "$file")
            [[ $uri == "$REPO_URI" ]] || die "Review $SOURCE: it does not contain only the expected URIs field."
            # Installer-owned files use single-line fields; reject extra stanzas,
            # continuations and unknown fields rather than erase unrelated data.
            awk '
                /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
                /^(Types|URIs|Suites|Components|Architectures|Signed-By):/ {
                    if (++seen[$1] > 1) exit 1
                    next
                }
                { exit 1 }
            ' "$file" || die "Review $SOURCE: refusing to overwrite additional APT settings."
        elif grep -Fq 'repo.web7dev.com' "$file"; then
            die "An existing repository entry needs manual review: $file. No duplicate source was created."
        fi
    done
}

cleanup() {
    local file
    for file in "${staged_files[@]}"; do
        "${as_root[@]}" rm -f -- "$file" || true
    done
    [[ -z $work ]] || rm -rf -- "$work"
}

atomic_install() {
    local input=$1 target=$2 directory=${2%/*} temporary
    if [[ -f $target ]] && cmp -s -- "$input" "$target" && [[ $(stat -c '%u:%g:%a' "$target") == 0:0:644 ]]; then
        printf 'Unchanged: %s\n' "$target"
        return
    fi
    [[ -d $directory ]] || "${as_root[@]}" install -d -m 0755 -- "$directory"
    temporary=$("${as_root[@]}" mktemp "$directory/.finder-gnome.XXXXXXXX")
    staged_files+=("$temporary")
    "${as_root[@]}" install -m 0644 -- "$input" "$temporary"
    "${as_root[@]}" mv -fT -- "$temporary" "$target"
}

main() {
    local dry_run=false argument key_metadata fingerprint count package
    local -a missing=()
    for argument in "$@"; do
        case $argument in
            --dry-run) dry_run=true ;;
            --help|-h) printf 'Usage: %s [--dry-run]\nSupports Ubuntu 24.04/26.04 amd64. Review this script before running it.\n' "$0"; return ;;
            *) die "Unknown argument: $argument (use --help)." ;;
        esac
    done
    detect_platform "$OS_RELEASE_FILE"
    command -v apt-get >/dev/null || die 'apt-get is required.'
    command -v dpkg-query >/dev/null || die 'dpkg-query is required.'
    check_sources
    command -v curl >/dev/null || missing+=(curl)
    command -v gpg >/dev/null || missing+=(gnupg)
    if [[ $(dpkg-query -W -f='${Status}' ca-certificates 2>/dev/null || true) != 'install ok installed' ]]; then
        missing+=(ca-certificates)
    fi
    printf 'Ubuntu %s; architecture %s; suite %s\nRepository: %s (%s)\n' "$ubuntu_version" "$architecture" "$suite" "$REPO_URI" main
    if $dry_run; then
        printf 'Dry run: no downloads, privilege requests, package commands or system writes.\n'
        if (( ${#missing[@]} )); then printf 'Would install missing prerequisites: %s\n' "${missing[*]}"; fi
        printf 'Would verify %s against fingerprint %s.\n' "$GPG_KEY_URL" "$GPG_FINGERPRINT"
        printf 'Would atomically install %s (0644) and %s (0644).\n' "$KEYRING" "$SOURCE"
        printf 'Would run: apt-get update; apt-get install -y finder-gnome\n'
        return
    fi
    [[ $GPG_FINGERPRINT =~ ^([A-F0-9]{40}|[A-F0-9]{64})$ ]] || die 'Invalid pinned GPG fingerprint in tools/install.sh.'
    as_root=()
    if (( EUID != 0 )); then
        command -v sudo >/dev/null || die 'Run as root or install sudo before running this installer.'
        sudo -v || die 'sudo authorization was denied; nothing was changed.'
        as_root=(sudo --)
    fi
    work=''
    staged_files=()
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap 'printf "Installation failed at line %s.\n" "$LINENO" >&2' ERR
    if (( ${#missing[@]} )); then
        "${as_root[@]}" apt-get update
        "${as_root[@]}" apt-get install -y "${missing[@]}"
    fi
    work=$(mktemp -d -t finder-gnome-install.XXXXXXXX)
    install -d -m 0700 "$work/gnupg"
    curl --disable --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 10 --max-time 60 --max-filesize 1048576 \
        --output "$work/download" "$GPG_KEY_URL"
    # Isolated GPG home: do not import into the user's keyring or start an agent.
    local -a gpg_command=(gpg --no-options --homedir "$work/gnupg" --batch --no-autostart)
    "${gpg_command[@]}" --with-colons --show-keys "$work/download" > "$work/key.info"
    key_metadata=$(cat "$work/key.info")
    count=$(awk -F: '$1 == "pub" {n++} END {print n+0}' <<< "$key_metadata")
    [[ $count == 1 ]] || die 'Expected exactly one public primary key.'
    ! grep -Eq '^(sec|ssb):' <<< "$key_metadata" || die 'Expected a public key, not private key material.'
    fingerprint=$(awk -F: '$1 == "pub" {primary=1; next} primary && $1 == "fpr" {print $10; exit}' <<< "$key_metadata")
    [[ $fingerprint == "$GPG_FINGERPRINT" ]] || die "GPG fingerprint mismatch: expected $GPG_FINGERPRINT, received $fingerprint."
    if grep -q '^-----BEGIN PGP PUBLIC KEY BLOCK-----' "$work/download"; then
        "${gpg_command[@]}" --dearmor --output "$work/key.binary" "$work/download"
    else
        cp -- "$work/download" "$work/key.binary"
    fi
    "${gpg_command[@]}" --import "$work/key.binary"
    "${gpg_command[@]}" --export "$GPG_FINGERPRINT" > "$work/key.gpg"
    [[ -s $work/key.gpg ]] || die 'GPG produced an empty keyring.'
    cat > "$work/repository.sources" <<EOF
Types: deb
URIs: $REPO_URI
Suites: $suite
Components: main
Architectures: amd64
Signed-By: $KEYRING
EOF
    atomic_install "$work/key.gpg" "$KEYRING"
    atomic_install "$work/repository.sources" "$SOURCE"
    "${as_root[@]}" apt-get update
    "${as_root[@]}" apt-get install -y finder-gnome
    package=$(dpkg-query -W -f='${Version}' finder-gnome)
    printf '\nInstalled finder-gnome %s on Ubuntu %s (%s).\nRepository: %s\nLaunch Finder Gnome from Applications or run: finder-gnome\n' "$package" "$ubuntu_version" "$architecture" "$REPO_URI"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi
