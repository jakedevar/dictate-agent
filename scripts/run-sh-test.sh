#!/usr/bin/env bash
# scripts/run.sh starts the enabled service without launching the June binary,
# even when starting the service fails. Disabled units keep the legacy path.
# Hermetic: temp HOME, stub systemctl and stub dictate-agent; never the real ones.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"; trap 'rm -rf -- "$tmp"' EXIT
mkdir -p "$tmp/home/.local/bin" "$tmp/bin"
printf '#!/bin/sh\necho started >"%s/started"\n' "$tmp" >"$tmp/home/.local/bin/dictate-agent"
chmod +x "$tmp/home/.local/bin/dictate-agent"
check() { # $1 = is-enabled exit, $2 = service start exit, $3 = legacy expected
    rm -f "$tmp/started" "$tmp/service-start"
    cat >"$tmp/bin/systemctl" <<EOF
#!/bin/sh
[ "\$1" = --user ] || exit 98
case "\$2" in
    is-enabled) exit $1 ;;
    start)
        [ "\$3" = dictated ] || exit 97
        echo start >"$tmp/service-start"
        exit $2 ;;
    *) exit 96 ;;
esac
EOF
    chmod +x "$tmp/bin/systemctl"
    local rc=0
    HOME="$tmp/home" PATH="$tmp/bin:/usr/bin:/bin" bash "$here/run.sh" || rc=$?
    if [ "$3" = yes ]; then
        [ -f "$tmp/started" ] && [ ! -e "$tmp/service-start" ] && [ "$rc" -eq 0 ] || {
            echo "FAIL: disabled service did not use only the legacy launcher"; exit 1;
        }
    else
        [ ! -e "$tmp/started" ] && [ -f "$tmp/service-start" ] && [ "$rc" -eq "$2" ] || {
            echo "FAIL: enabled service was not started or legacy fallback ran"; exit 1;
        }
    fi
}
check 0 0 no   # enabled: start service; also safe when already active
check 0 5 no   # failed service start must stay visible, never start old daemon
check 1 0 yes  # pre-cutover or rollback: June binary starts as before
echo "run.sh guard: OK"
