# shellcheck shell=bash
# Shared signing guard for bundle.sh, install.sh and remote.sh (#3766).
#
# A privacy grant (Accessibility, Input Monitoring) is recorded against the
# app's designated requirement. An ad-hoc signature's requirement is its
# cdhash, which changes on every build, so an install of an ad-hoc bundle
# records a grant that the next build can never match: macOS shows the switch
# on in System Settings while the app is not trusted. An install therefore
# refuses an ad-hoc bundle unless --allow-adhoc is passed.

# True when the designated-requirement line names a certificate or Apple's
# anchor (stable across builds); false for ad hoc (`cdhash H"…"`) or empty.
designated_is_stable() {
    [[ $1 == *"certificate leaf"* || $1 == *"anchor apple"* ]]
}

# True when "$@" (the arguments of `install`) contains --allow-adhoc.
args_allow_adhoc() {
    local a
    for a in "$@"; do [[ $a == --allow-adhoc ]] && return 0; done
    return 1
}

# bundle.sh: with DICTATEHUB_REQUIRE_SIGNED=1 a bundle that ended up ad hoc is
# an error, never a silent fallback. $1 = "adhoc" or "identity".
bundle_signing_guard() {
    if [[ ${DICTATEHUB_REQUIRE_SIGNED:-} == 1 && $1 == adhoc ]]; then
        echo "error: no stable signing identity; refusing an ad-hoc bundle for an install." >&2
        echo "       Run scripts/signing-setup.sh and rebundle, or pass --allow-adhoc to install." >&2
        return 1
    fi
}
