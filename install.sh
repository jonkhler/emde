#!/bin/sh
# Install emde, the terminal Markdown reader, from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/jonkhler/emde/main/install.sh | sh
#
# Releases are downloaded anonymously from github.com. For a private fork
# (EMDE_REPO), downloads are authenticated with the GitHub CLI (`gh auth
# login`) or with $GITHUB_TOKEN. Without a release for this platform, emde is
# built from source with cargo instead.
#
# Environment:
#   EMDE_VERSION       release tag to install (default: the latest, e.g. v0.1.0)
#   EMDE_INSTALL_DIR   where the binary goes (default: ~/.local/bin)
#   EMDE_DATA_DIR      man page and completions (default: ~/.local/share)
#   EMDE_REPO          owner/name of the repository (default: jonkhler/emde)
#   EMDE_ARCHIVE       install this local release archive instead of downloading
#   GITHUB_TOKEN       token for private repositories when gh is not available
#
# Options:
#   --local            build from this checkout (cargo xtask dist) and install it
#   --from-source      build with `cargo install --git` instead of downloading
#   --uninstall        remove what this script installed

set -eu

REPO=${EMDE_REPO:-jonkhler/emde}
BIN_DIR=${EMDE_INSTALL_DIR:-$HOME/.local/bin}
DATA_DIR=${EMDE_DATA_DIR:-$HOME/.local/share}
VERSION=${EMDE_VERSION:-}

say() { printf 'emde-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$arch" in
        x86_64 | amd64) arch=x86_64 ;;
        arm64 | aarch64) arch=aarch64 ;;
        *) die "unsupported CPU architecture: $arch" ;;
    esac
    case "$os" in
        Linux) echo "$arch-unknown-linux-gnu" ;;
        Darwin) echo "$arch-apple-darwin" ;;
        *) die "unsupported operating system: $os" ;;
    esac
}

sha256_of() {
    if have sha256sum; then
        sha256sum "$1" | cut -d' ' -f1
    elif have shasum; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        die "need sha256sum or shasum to verify the download"
    fi
}

# GitHub REST API GET with $GITHUB_TOKEN; extra curl arguments follow.
api() {
    path=$1
    shift
    curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" \
        -H 'X-GitHub-Api-Version: 2022-11-28' "$@" "https://api.github.com/$path"
}

# Download release asset $1 of tag $VERSION into directory $2. (POSIX sh has
# no local variables: these names must not clash with the callers'.)
download() {
    dl_asset=$1
    dl_dir=$2
    # A public repository: no login needed.
    if have curl && curl -fsSL -o "$dl_dir/$dl_asset" \
        "https://github.com/$REPO/releases/download/$VERSION/$dl_asset" 2>/dev/null; then
        return
    fi
    rm -f "$dl_dir/$dl_asset"
    if have gh && gh auth status >/dev/null 2>&1; then
        gh release download "$VERSION" --repo "$REPO" --pattern "$dl_asset" --dir "$dl_dir" --clobber
        return
    fi
    [ -n "${GITHUB_TOKEN:-}" ] || return 1
    have curl || die "need curl to download"
    dl_release=$(api "repos/$REPO/releases/tags/$VERSION") || die "no release $VERSION in $REPO"
    # The asset's numeric id: the last "id" before its "name" (GitHub lists
    # url, id, node_id, name for each asset), whether the JSON is pretty or not.
    dl_id=$(printf '%s' "$dl_release" | tr ',{' '\n\n' | sed 's/^[[:space:]]*//; s/": */":/' |
        awk -v want="\"name\":\"$dl_asset\"" '
            /^"id":[0-9]/ { split($0, kv, ":"); id = kv[2] }
            $0 == want { print id; exit }')
    [ -n "$dl_id" ] || return 1
    api "repos/$REPO/releases/assets/$dl_id" -H 'Accept: application/octet-stream' -o "$dl_dir/$dl_asset"
}

latest_tag() {
    # A public repository: github.com redirects /releases/latest to the tag.
    if have curl; then
        tag=$(curl -fsSIL -o /dev/null -w '%{url_effective}' \
            "https://github.com/$REPO/releases/latest" 2>/dev/null | sed -n 's#.*/releases/tag/##p')
        if [ -n "$tag" ]; then
            echo "$tag"
            return
        fi
    fi
    if have gh && gh auth status >/dev/null 2>&1; then
        gh release view --repo "$REPO" --json tagName --jq .tagName
    elif [ -n "${GITHUB_TOKEN:-}" ]; then
        api "repos/$REPO/releases/latest" | tr ',' '\n' |
            sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1
    fi
}

# Unpack and install the release archive $1.
install_archive() {
    archive=$1
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    tar -xzf "$archive" -C "$work"
    root=$(find "$work" -mindepth 1 -maxdepth 1 -type d -name 'emde-*' | head -n 1)
    [ -n "$root" ] && [ -x "$root/emde" ] || die "$archive is not an emde release archive"
    mkdir -p "$BIN_DIR" "$DATA_DIR/man/man1" "$DATA_DIR/bash-completion/completions" \
        "$DATA_DIR/zsh/site-functions" "$DATA_DIR/fish/vendor_completions.d"
    # Replace the binary atomically, so a running emde keeps working.
    cp "$root/emde" "$BIN_DIR/.emde.new"
    chmod 755 "$BIN_DIR/.emde.new"
    mv -f "$BIN_DIR/.emde.new" "$BIN_DIR/emde"
    cp "$root/man/emde.1" "$DATA_DIR/man/man1/emde.1"
    cp "$root/completions/emde.bash" "$DATA_DIR/bash-completion/completions/emde"
    cp "$root/completions/_emde" "$DATA_DIR/zsh/site-functions/_emde"
    cp "$root/completions/emde.fish" "$DATA_DIR/fish/vendor_completions.d/emde.fish"
    say "installed $("$BIN_DIR/emde" --version) to $BIN_DIR/emde"
}

from_source() {
    have cargo || die "no release for $(target), and cargo is not installed to build from source
  (install Rust from https://rustup.rs, then run this again)"
    say "building from source with cargo (this takes a minute)"
    tag_arg=
    [ -n "$VERSION" ] && tag_arg="--tag $VERSION"
    root=$(mktemp -d)
    # git on the command line uses your GitHub credentials (gh auth setup-git).
    # shellcheck disable=SC2086
    CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --locked --root "$root" \
        --git "https://github.com/$REPO" $tag_arg emde
    mkdir -p "$BIN_DIR"
    mv -f "$root/bin/emde" "$BIN_DIR/emde"
    rm -rf "$root"
    say "installed $("$BIN_DIR/emde" --version) to $BIN_DIR/emde (no man page or completions)"
}

# Build a release archive from the checkout this script is in, and install it.
from_checkout() {
    here=$(cd "$(dirname "$0")" && pwd)
    [ -f "$here/Cargo.toml" ] && [ -d "$here/xtask" ] || die "--local must be run from an emde checkout (./install.sh --local)"
    # The project's toolchain lives outside the checkout (scripts/env.sh).
    if ! have cargo && [ -f "$here/scripts/env.sh" ]; then
        # shellcheck disable=SC1091
        . "$here/scripts/env.sh"
    fi
    have cargo || die "cargo not found (source scripts/env.sh, or install Rust from https://rustup.rs)"
    say "building a release archive from $here"
    log=$(cd "$here" && cargo xtask dist 2>&1) || { printf '%s\n' "$log" >&2; die "build failed"; }
    archive=$(printf '%s\n' "$log" | sed -n 's/^xtask: == dist: //p' | tail -n 1)
    [ -n "$archive" ] && [ -f "$archive" ] || die "cargo xtask dist made no archive"
    install_archive "$archive"
}

uninstall() {
    rm -f "$BIN_DIR/emde" "$DATA_DIR/man/man1/emde.1" \
        "$DATA_DIR/bash-completion/completions/emde" "$DATA_DIR/zsh/site-functions/_emde" \
        "$DATA_DIR/fish/vendor_completions.d/emde.fish"
    say "removed emde from $BIN_DIR and $DATA_DIR"
}

path_hint() {
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) say "note: $BIN_DIR is not on your PATH; add it, e.g.
  echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.bashrc" ;;
    esac
}

main() {
    case "${1:-}" in
        --uninstall) uninstall; return ;;
        --local) from_checkout; path_hint; return ;;
        --from-source) from_source; path_hint; return ;;
        "") ;;
        *) die "unknown option: $1 (use --local, --from-source or --uninstall)" ;;
    esac

    if [ -n "${EMDE_ARCHIVE:-}" ]; then
        install_archive "$EMDE_ARCHIVE"
        path_hint
        return
    fi

    triple=$(target)
    [ -n "$VERSION" ] || VERSION=$(latest_tag 2>/dev/null) || true
    if [ -z "$VERSION" ]; then
        die "cannot find a release of $REPO (no release yet, no network, or a private
  repository: then log in with gh auth login or set GITHUB_TOKEN); try --from-source"
    fi
    asset="emde-${VERSION#v}-$triple.tar.gz"
    dl=$(mktemp -d)
    say "downloading $asset ($VERSION)"
    if ! download "$asset" "$dl" 2>/dev/null || [ ! -s "$dl/$asset" ]; then
        rm -rf "$dl"
        say "no release archive for $triple in $VERSION"
        from_source
        path_hint
        return
    fi
    download "$asset.sha256" "$dl" || die "the release has no checksum for $asset"
    want=$(cut -d' ' -f1 <"$dl/$asset.sha256")
    got=$(sha256_of "$dl/$asset")
    [ "$want" = "$got" ] || die "checksum mismatch for $asset (expected $want, got $got)"
    install_archive "$dl/$asset"
    rm -rf "$dl"
    path_hint
}

main "$@"
