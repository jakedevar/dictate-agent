#!/bin/bash
# On the Mac: create (once) DictateHub's dedicated code-signing keychain and
# its self-signed code-signing identity. Idempotent: a second run only checks.
#
#   mac/scripts/signing-setup.sh           # create if absent, else verify
#   mac/scripts/signing-setup.sh --print   # print the identity's SHA-1 (for bundle.sh)
#   mac/scripts/signing-setup.sh --reset   # delete and recreate (NEW certificate:
#                                          # Microphone/Accessibility must be granted again)
#
# Why: codesign over SSH cannot use the login keychain (it is locked for the
# SSH security session: errSecInternalComponent, #3541), and ad-hoc signatures
# change their designated requirement (the cdhash) on every build, so macOS
# drops the privacy grants. A dedicated keychain whose password lives in a
# 0600 file can be unlocked non-interactively, and a fixed certificate gives a
# stable designated requirement:
#     identifier "dev.jakedevar.DictateHub" and certificate leaf = H"<sha1>"
#
# What it touches, and nothing else:
#   ~/Library/Keychains/dictatehub-signing.keychain-db   the keychain
#   ~/Library/Keychains/dictatehub-signing.password      its random password (0600)
# The login keychain, the default keychain and the keychain search list are
# left as they are (codesign is pointed at the keychain with --keychain, so
# it need not be in the search list, and a locked keychain in the search list
# could make other apps show unlock prompts). The certificate is not marked
# trusted (that needs an admin prompt); codesign and TCC do not need it.
# Passwords go to `security` on stdin (`security -i`), never in argv.
set -euo pipefail

KC_DIR="$HOME/Library/Keychains"
KC="$KC_DIR/dictatehub-signing.keychain-db"
PWFILE="$KC_DIR/dictatehub-signing.password"
CN="DictateHub Local Signing"
DAYS=7300  # 20 years: the certificate is the TCC identity, so it should outlive the Mac

mode=${1:-}
case $mode in "" | --print | --reset) ;; *) sed -n '2,8p' "$0" >&2; exit 2 ;; esac

say() { [[ $mode == --print ]] || echo "$@"; }

# `security -i` reads commands from stdin, so the password is never in argv.
sec_stdin() { printf '%s\n' "$1" | security -i >/dev/null; }

unlock() { sec_stdin "unlock-keychain -p $(cat "$PWFILE") $KC"; }

identity_sha1() {
    security find-identity -p codesigning "$KC" 2>/dev/null \
        | awk -v cn="\"$CN\"" 'index($0, cn) { print $2; exit }'
}

search_list() { security list-keychains -d user | sed -E 's/^[[:space:]]*"(.*)"$/\1/'; }

if [[ $mode == --reset && -e $KC ]]; then
    echo "==> --reset: deleting $KC (the new certificate needs fresh privacy grants)"
    security delete-keychain "$KC" 2>/dev/null || rm -f "$KC"
    rm -f "$PWFILE"
fi

if [[ -e $KC ]]; then
    [[ -f $PWFILE ]] || {
        echo "error: $KC exists but its password file $PWFILE is missing." >&2
        echo "       Recreate with: $0 --reset" >&2
        exit 1
    }
    chmod 600 "$PWFILE"
    unlock
    sha1=$(identity_sha1)
    [[ -n $sha1 ]] || { echo "error: no \"$CN\" identity in $KC; recreate with: $0 --reset" >&2; exit 1; }
    if ! security find-certificate -c "$CN" -p "$KC" | openssl x509 -noout -checkend $((86400 * 30)) >/dev/null; then
        echo "warning: the signing certificate expires within 30 days; run $0 --reset" >&2
    fi
    say "==> signing keychain present: $KC"
    say "    identity \"$CN\" SHA-1 $sha1"
    [[ $mode == --print ]] && echo "$sha1"
    exit 0
fi

say "==> creating $KC"
before=$(search_list)
umask 077
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

openssl rand -hex 32 > "$PWFILE.tmp"
chmod 600 "$PWFILE.tmp"
mv "$PWFILE.tmp" "$PWFILE"
sec_stdin "create-keychain -p $(cat "$PWFILE") $KC"
# Lock after 10 minutes idle and on sleep; bundle.sh unlocks it each time.
security set-keychain-settings -l -u -t 600 "$KC"

cat > "$tmp/req.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
x509_extensions = ext
[dn]
CN = $CN
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
EOF
openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days "$DAYS" -config "$tmp/req.cnf" \
    -keyout "$tmp/key.pem" -out "$tmp/cert.pem" 2>/dev/null
p12pw=$(openssl rand -hex 16)
openssl pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" -name "$CN" \
    -passout "pass:$p12pw" -out "$tmp/id.p12"
unlock
# -T: codesign may use the key; the partition list below makes that prompt-free.
security import "$tmp/id.p12" -k "$KC" -P "$p12pw" -T /usr/bin/codesign >/dev/null
sec_stdin "set-key-partition-list -S apple-tool:,apple:,codesign: -s -k $(cat "$PWFILE") $KC"

# `create-keychain` may add the new keychain to the search list; put the list
# back exactly as it was.
if [[ $(search_list) != "$before" ]]; then
    # shellcheck disable=SC2046 # one path per line, no spaces in $HOME paths here
    security list-keychains -d user -s $(printf '%s\n' "$before" | awk '!seen[$0]++')
    say "    restored the keychain search list"
fi

sha1=$(identity_sha1)
[[ -n $sha1 ]] || { echo "error: identity not found after import" >&2; exit 1; }
say "    identity \"$CN\" SHA-1 $sha1 (self-signed, $DAYS days)"
say "    password file $PWFILE (0600)"
[[ $mode == --print ]] && echo "$sha1"
exit 0
