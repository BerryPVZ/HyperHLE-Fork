#!/bin/sh
# Package HyperHLE release zips and write release notes for action-gh-release.
set -eu

VERSION="$1"
CHANGELOG_FROM="${2:-}"
CHANGELOG_LIMIT="${CHANGELOG_LIMIT:-}"
if [ -z "$VERSION" ]; then
    echo "Usage: $0 <version e.g. v1.0.0> [changelog_from_ref]" >&2
    exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

windows_exe=""
for candidate in \
    artifacts/windows/touchHLE.exe \
    artifacts/windows/touchHLE_windows_bundle/touchHLE.exe
do
    if [ -e "$candidate" ]; then
        windows_exe="$candidate"
        break
    fi
done

linux_bin=""
for candidate in \
    artifacts/linux/touchHLE \
    artifacts/linux/touchHLE_linux_bundle/touchHLE
do
    if [ -e "$candidate" ]; then
        linux_bin="$candidate"
        break
    fi
done

macos_x86_dmg=""
for candidate in \
    artifacts/macos-x86_64/touchHLE_x86_64-apple-darwin.dmg \
    artifacts/macos/HyperHLE.dmg \
    artifacts/macos/touchHLE.dmg
do
    if [ -e "$candidate" ]; then
        macos_x86_dmg="$candidate"
        break
    fi
done
macos_arm_dmg="artifacts/macos-aarch64/touchHLE_aarch64-apple-darwin.dmg"

android_apk=""
for candidate in \
    artifacts/android/HyperHLE-Reborn.apk \
    artifacts/android/touchHLE.apk
do
    if [ -e "$candidate" ]; then
        android_apk="$candidate"
        break
    fi
done

for path in "$macos_x86_dmg" "$macos_arm_dmg" "$android_apk"; do
    if [ -z "$path" ] || [ ! -e "$path" ]; then
        echo "Missing build artifact (all platform builds must succeed): $path" >&2
        exit 1
    fi
done
if [ -z "$windows_exe" ]; then
    echo "Missing build artifact (all platform builds must succeed): artifacts/windows/touchHLE.exe" >&2
    exit 1
fi
windows_installer="artifacts/windows-installer/HyperHLE_Windows_x86_64_Setup.exe"
if [ ! -f "$windows_installer" ]; then
    echo "Missing build artifact (all platform builds must succeed): $windows_installer" >&2
    exit 1
fi
if [ -z "$linux_bin" ]; then
    echo "Missing build artifact (all platform builds must succeed): artifacts/linux/touchHLE" >&2
    exit 1
fi
ios_ipas="HyperHLE-iOS-unsigned.ipa HyperHLE-iOS-trollstore.ipa HyperHLE-iOS-trollstore-permanent-jit.ipa"
for ipa in $ios_ipas; do
    if [ ! -f "artifacts/ios/$ipa" ]; then
        echo "Missing build artifact (all platform builds must succeed): artifacts/ios/$ipa" >&2
        exit 1
    fi
done

if [ -z "$CHANGELOG_FROM" ]; then
    # The newest release tag older than VERSION (versions roll over at 12, see
    # hyperhle-should-release.sh, so it can't simply be computed).
    CHANGELOG_FROM="$(
        { git tag -l 'v[0-9]*' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$'; echo "$VERSION"; } |
            sort -u -V | grep -x -B 1 -F "$VERSION" | grep -v -x -F "$VERSION" || true
    )"
    if [ -z "$CHANGELOG_FROM" ]; then
        CHANGELOG_FROM="$(git rev-list -n 1 HEAD -- dev-scripts/hyperhle-should-release.sh)"
    fi
fi

rm -rf release
mkdir -p release

# GitHub username for a commit's author, as "@login", for the changelog.
# Asks the GitHub API (works in the release job, where GH_TOKEN is set), then
# falls back to the login inside a GitHub no-reply email, then to the git
# author name.
author_mention() {
    hash="$1"
    email="$2"
    name="$3"
    login=""
    if [ -n "${GH_TOKEN:-}" ] && [ -n "${GITHUB_REPOSITORY:-}" ] &&
        command -v gh >/dev/null 2>&1; then
        # On failure gh prints the error body to stdout, so only keep the
        # output of a successful call, and only if it looks like a login.
        if api_login="$(gh api "repos/${GITHUB_REPOSITORY}/commits/${hash}" \
            --jq '.author.login // empty' </dev/null 2>/dev/null)"; then
            login="$api_login"
        fi
    fi
    case "$login" in
        *[!A-Za-z0-9[\]-]*) login="" ;;
    esac
    if [ -z "$login" ]; then
        case "$email" in
            *@users.noreply.github.com)
                login="${email%@users.noreply.github.com}"
                login="${login#*+}"
                ;;
        esac
    fi
    if [ -n "$login" ]; then
        printf '@%s' "$login"
    else
        printf '%s' "$name"
    fi
}

# Changelog lines look like: - `abc1234` **@user**: Commit message
write_changelog() {
    git log "$@" --reverse --format='%H%x1f%s%x1f%an%x1f%ae' |
        while IFS="$(printf '\037')" read -r hash subject author email; do
            [ -z "$hash" ] && continue
            short_hash="$(printf '%.7s' "$hash")"
            mention="$(author_mention "$hash" "$email" "$author")"
            if [ -n "${GITHUB_SERVER_URL:-}" ] && [ -n "${GITHUB_REPOSITORY:-}" ]; then
                printf -- '- [`%s`](%s/%s/commit/%s) **%s**: %s\n' \
                    "$short_hash" "$GITHUB_SERVER_URL" "$GITHUB_REPOSITORY" "$hash" \
                    "$mention" "$subject"
            else
                printf -- '- `%s` **%s**: %s\n' "$short_hash" "$mention" "$subject"
            fi
        done
}

{
    printf '%s\n\n' "HyperHLE ${VERSION}"
    if [ "${FORCE_HYPERHLE_RELEASE:-}" = "true" ]; then
        printf '%s\n\n' "_Manual release — changelog shows the latest 6 commits._"
    fi
    printf '%s\n\n' "## Changelog"
    if [ -n "$CHANGELOG_LIMIT" ]; then
        write_changelog -n "$CHANGELOG_LIMIT" HEAD
    elif [ -n "$CHANGELOG_FROM" ]; then
        write_changelog "${CHANGELOG_FROM}..HEAD"
    else
        printf '%s\n' "_No commit range available._"
    fi
} >release/RELEASE_NOTES.md

cd "$ROOT/dev-scripts"
./prepare-release.sh --prepare-files

prefix="HyperHLE"

./prepare-release.sh --create-zip-macos "$ROOT/$macos_x86_dmg" \
    -o "$ROOT/release/${prefix}_macOS_x86_64.zip"
./prepare-release.sh --create-zip-macos "$ROOT/$macos_arm_dmg" \
    -o "$ROOT/release/${prefix}_macOS_AArch64.zip"
./prepare-release.sh --create-zip-android "$ROOT/$android_apk" \
    -o "$ROOT/release/${prefix}_Android_AArch64.zip"
./prepare-release.sh --create-zip-windows \
    "$ROOT/$windows_exe" \
    -o "$ROOT/release/${prefix}_Windows_x86_64.zip"
./prepare-release.sh --create-zip-linux \
    "$ROOT/$linux_bin" \
    -o "$ROOT/release/${prefix}_Linux_x86_64.zip"
# The Windows installer is attached as it is (see build-win).
cp "$ROOT/$windows_installer" "$ROOT/release/HyperHLE_Windows_x86_64_Setup.exe"
# The iOS IPAs are attached as they are (see build-ios in HyperHLE_release.yml).
for ipa in $ios_ipas; do
    cp "$ROOT/artifacts/ios/$ipa" "$ROOT/release/$ipa"
done
