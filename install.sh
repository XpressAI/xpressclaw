#!/bin/sh
set -eu

releases_url="${XPRESSCLAW_RELEASES_URL:-https://github.com/XpressAI/xpressclaw/releases}"
requested_version="${XPRESSCLAW_VERSION:-}"

say() {
    printf '%s\n' "$*"
}

fail() {
    printf 'xpressclaw installer: %s\n' "$*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

require_command curl
require_command uname
require_command tar
require_command awk
require_command mktemp

if [ -z "${HOME:-}" ] && [ -z "${XPRESSCLAW_INSTALL_DIR:-}" ]; then
    fail 'HOME is not set; set XPRESSCLAW_INSTALL_DIR to choose an installation directory'
fi
install_dir="${XPRESSCLAW_INSTALL_DIR:-${HOME}/.local/bin}"

system_name=$(uname -s)
machine_name=$(uname -m)
case "${system_name}:${machine_name}" in
    Darwin:arm64|Darwin:aarch64)
        target="aarch64-apple-darwin"
        ;;
    Darwin:x86_64|Darwin:amd64)
        target="x86_64-apple-darwin"
        ;;
    Linux:x86_64|Linux:amd64)
        target="x86_64-unknown-linux-gnu"
        ;;
    *)
        fail "no prebuilt CLI is available for ${system_name} ${machine_name}; see the developer guide for source-build instructions"
        ;;
esac

archive="xpressclaw-cli-${target}.tar.gz"

if [ -n "$requested_version" ]; then
    case "$requested_version" in
        v*) release_tag="$requested_version" ;;
        *) release_tag="v${requested_version}" ;;
    esac
else
    say "Finding the latest stable XpressClaw release..."
    if ! latest_url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "${releases_url}/latest"); then
        fail 'no stable release is available yet; prereleases are intentionally ignored'
    fi
    latest_url=${latest_url%/}
    release_tag=${latest_url##*/}
fi

case "$release_tag" in
    v[0-9]*) ;;
    *) fail "could not determine a valid release tag (received: ${release_tag})" ;;
esac
case "$release_tag" in
    *[!A-Za-z0-9._-]*) fail "release tag contains unsupported characters: ${release_tag}" ;;
esac

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/xpressclaw-install.XXXXXX")
temporary_install=""
cleanup() {
    rm -rf "$temporary_dir"
    if [ -n "$temporary_install" ]; then
        rm -f "$temporary_install"
    fi
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

archive_path="${temporary_dir}/${archive}"
checksums_path="${temporary_dir}/SHA256SUMS"
download_root="${releases_url}/download/${release_tag}"

say "Downloading XpressClaw ${release_tag} for ${target}..."
curl -fsSL "${download_root}/${archive}" -o "$archive_path" ||
    fail "release ${release_tag} does not provide ${archive}"
curl -fsSL "${download_root}/SHA256SUMS" -o "$checksums_path" ||
    fail "release ${release_tag} does not provide SHA256SUMS"

expected_checksum=$(awk -v archive="$archive" '$2 == archive || $2 == "*" archive { print $1; exit }' "$checksums_path")
[ -n "$expected_checksum" ] || fail "SHA256SUMS has no entry for ${archive}"

if command -v sha256sum >/dev/null 2>&1; then
    actual_checksum=$(sha256sum "$archive_path" | awk '{ print $1 }')
elif command -v shasum >/dev/null 2>&1; then
    actual_checksum=$(shasum -a 256 "$archive_path" | awk '{ print $1 }')
elif command -v openssl >/dev/null 2>&1; then
    actual_checksum=$(openssl dgst -sha256 "$archive_path" | awk '{ print $NF }')
else
    fail 'sha256sum, shasum, or openssl is required to verify the download'
fi

[ "$actual_checksum" = "$expected_checksum" ] || fail "checksum verification failed for ${archive}"

tar -xzf "$archive_path" -C "$temporary_dir"
[ -f "${temporary_dir}/xpressclaw" ] || fail "${archive} does not contain the xpressclaw binary"

mkdir -p "$install_dir"
temporary_install=$(mktemp "${install_dir}/.xpressclaw.XXXXXX")
cp "${temporary_dir}/xpressclaw" "$temporary_install"
chmod 0755 "$temporary_install"
mv -f "$temporary_install" "${install_dir}/xpressclaw"
temporary_install=""

say "Installed XpressClaw ${release_tag} to ${install_dir}/xpressclaw"
case ":${PATH:-}:" in
    *":${install_dir}:"*)
        say 'Next: xpressclaw up'
        ;;
    *)
        say "Add ${install_dir} to PATH, then run: xpressclaw up"
        ;;
esac
