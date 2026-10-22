# Sourced by repo shell scripts: `devtool SUBCOMMAND ...` runs the dev-only
# helper binary (crates/dictate-devtools) in place of the inline interpreter
# snippets operator directive 2b22862d forbids. Std-only crate: the build is
# a couple of seconds and needs no network.
_devtool_repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
devtool_path() {
    cargo build -q --manifest-path "$_devtool_repo/Cargo.toml" -p dictate-devtools --bin devtool \
        --message-format=json | jq -r 'select(.reason == "compiler-artifact" and .target.name == "devtool" and .executable != null) | .executable' | tail -1
}
DEVTOOL_BIN="$(devtool_path)"
[[ -x $DEVTOOL_BIN ]] || { echo "could not build dictate-devtools" >&2; exit 1; }
devtool() { "$DEVTOOL_BIN" "$@"; }
