#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT

fixture_dir="$test_dir/releases"
fake_bin="$test_dir/bin"
install_dir="$test_dir/install"
curl_log="$test_dir/curl.log"
mkdir -p "$fixture_dir" "$fake_bin"

printf '#!/bin/sh\nprintf "xpressclaw fixture 0.3.0\\n"\n' > "$fixture_dir/xpressclaw"
chmod +x "$fixture_dir/xpressclaw"
tar -C "$fixture_dir" -czf "$fixture_dir/xpressclaw-cli-x86_64-unknown-linux-gnu.tar.gz" xpressclaw
(
    cd "$fixture_dir"
    sha256sum xpressclaw-cli-x86_64-unknown-linux-gnu.tar.gz > SHA256SUMS
)

cat > "$fake_bin/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
    -s) printf '%s\n' "${FAKE_UNAME_SYSTEM:-Linux}" ;;
    -m) printf '%s\n' "${FAKE_UNAME_MACHINE:-x86_64}" ;;
    *) exit 2 ;;
esac
EOF

cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
output=""
url=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o)
            output=$2
            shift 2
            ;;
        -w)
            shift 2
            ;;
        -*)
            shift
            ;;
        *)
            url=$1
            shift
            ;;
    esac
done

printf '%s\n' "$url" >> "$FAKE_CURL_LOG"
case "$url" in
    */latest)
        [ "${FAKE_LATEST_FAILURE:-0}" = 0 ] || exit 22
        printf '%s/tag/v0.3.0' "$XPRESSCLAW_RELEASES_URL"
        ;;
    */xpressclaw-cli-x86_64-unknown-linux-gnu.tar.gz)
        cp "$FIXTURE_RELEASE_DIR/xpressclaw-cli-x86_64-unknown-linux-gnu.tar.gz" "$output"
        ;;
    */SHA256SUMS)
        if [ "${FAKE_BAD_CHECKSUM:-0}" = 1 ]; then
            printf '%064d  xpressclaw-cli-x86_64-unknown-linux-gnu.tar.gz\n' 0 > "$output"
        else
            cp "$FIXTURE_RELEASE_DIR/SHA256SUMS" "$output"
        fi
        ;;
    *)
        exit 22
        ;;
esac
EOF
chmod +x "$fake_bin/uname" "$fake_bin/curl"

output=$(
    PATH="$fake_bin:$PATH" \
    FIXTURE_RELEASE_DIR="$fixture_dir" \
    FAKE_CURL_LOG="$curl_log" \
    XPRESSCLAW_RELEASES_URL="https://example.test/xpressclaw/releases" \
    XPRESSCLAW_INSTALL_DIR="$install_dir" \
    sh "$repo_dir/install.sh"
)

grep -Fq 'Finding the latest stable XpressClaw release...' <<< "$output"
grep -Fq "Installed XpressClaw v0.3.0 to $install_dir/xpressclaw" <<< "$output"
grep -Fxq 'https://example.test/xpressclaw/releases/latest' "$curl_log"
grep -Fxq 'https://example.test/xpressclaw/releases/download/v0.3.0/xpressclaw-cli-x86_64-unknown-linux-gnu.tar.gz' "$curl_log"
test -x "$install_dir/xpressclaw"
installed_output=$("$install_dir/xpressclaw")
test "$installed_output" = 'xpressclaw fixture 0.3.0'

: > "$curl_log"
PATH="$fake_bin:$PATH" \
    FIXTURE_RELEASE_DIR="$fixture_dir" \
    FAKE_CURL_LOG="$curl_log" \
    XPRESSCLAW_RELEASES_URL="https://example.test/xpressclaw/releases" \
    XPRESSCLAW_INSTALL_DIR="$test_dir/prerelease" \
    XPRESSCLAW_VERSION=v0.3.0-rc.1 \
    sh "$repo_dir/install.sh" > "$test_dir/prerelease.out"
if grep -Fq '/latest' "$curl_log"; then
    echo 'version-pinned installer unexpectedly queried the latest release' >&2
    exit 1
fi
grep -Fxq 'https://example.test/xpressclaw/releases/download/v0.3.0-rc.1/xpressclaw-cli-x86_64-unknown-linux-gnu.tar.gz' "$curl_log"

if PATH="$fake_bin:$PATH" \
    FIXTURE_RELEASE_DIR="$fixture_dir" \
    FAKE_CURL_LOG="$curl_log" \
    FAKE_BAD_CHECKSUM=1 \
    XPRESSCLAW_RELEASES_URL="https://example.test/xpressclaw/releases" \
    XPRESSCLAW_INSTALL_DIR="$test_dir/bad-checksum" \
    XPRESSCLAW_VERSION=v0.3.0 \
    sh "$repo_dir/install.sh" > "$test_dir/bad-checksum.out" 2>&1; then
    echo 'installer unexpectedly accepted a bad checksum' >&2
    exit 1
fi
grep -Fq 'checksum verification failed' "$test_dir/bad-checksum.out"

if PATH="$fake_bin:$PATH" \
    FIXTURE_RELEASE_DIR="$fixture_dir" \
    FAKE_CURL_LOG="$curl_log" \
    FAKE_LATEST_FAILURE=1 \
    XPRESSCLAW_RELEASES_URL="https://example.test/xpressclaw/releases" \
    XPRESSCLAW_INSTALL_DIR="$test_dir/no-release" \
    sh "$repo_dir/install.sh" > "$test_dir/no-release.out" 2>&1; then
    echo 'installer unexpectedly accepted a missing stable release' >&2
    exit 1
fi
grep -Fq 'no stable release is available yet; prereleases are intentionally ignored' "$test_dir/no-release.out"

if PATH="$fake_bin:$PATH" \
    FIXTURE_RELEASE_DIR="$fixture_dir" \
    FAKE_CURL_LOG="$curl_log" \
    FAKE_UNAME_MACHINE=riscv64 \
    XPRESSCLAW_RELEASES_URL="https://example.test/xpressclaw/releases" \
    XPRESSCLAW_INSTALL_DIR="$test_dir/unsupported" \
    sh "$repo_dir/install.sh" > "$test_dir/unsupported.out" 2>&1; then
    echo 'installer unexpectedly accepted an unsupported platform' >&2
    exit 1
fi
grep -Fq 'no prebuilt CLI is available for Linux riscv64' "$test_dir/unsupported.out"

echo 'installer tests passed'
