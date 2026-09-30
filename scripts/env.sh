# Source this file to use the emde development toolchain:
#
#     source scripts/env.sh
#
# The toolchain, cargo home and build output live outside the repository
# (by default on the root disk under /var/tmp/emde-rust) so builds never
# fill up the home volume. Override the location with EMDE_RUST_ROOT.
#
# The directory is disposable: if it disappears, recreate it with
#
#     curl -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path \
#         --profile minimal --default-toolchain 1.98.1 -c clippy,rustfmt
#
# after sourcing this file (rust-toolchain.toml pins the exact version).

: "${EMDE_RUST_ROOT:=/var/tmp/emde-rust}"
export EMDE_RUST_ROOT
export RUSTUP_HOME="$EMDE_RUST_ROOT/rustup"
export CARGO_HOME="$EMDE_RUST_ROOT/cargo"
export CARGO_TARGET_DIR="$EMDE_RUST_ROOT/target"
case ":$PATH:" in
  *":$CARGO_HOME/bin:"*) ;;
  *) export PATH="$CARGO_HOME/bin:$PATH" ;;
esac
