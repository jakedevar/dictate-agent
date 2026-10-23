#!/bin/bash
# Real per-user dictated service; no sudo and no changes to DictateHub or hub.
set -euo pipefail
umask 077
LABEL=dev.jakedevar.dictated
DATA="$HOME/Library/Application Support/dictated"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOGS="$HOME/Library/Logs/dictated"
DOMAIN="gui/$(id -u)"
secure_dir() {
    [[ ! -L $1 ]] || { echo "refusing symlink directory: $1" >&2; exit 1; }
    mkdir -p "$1"
    [[ -d $1 && -O $1 ]] || { echo "directory not owned by us: $1" >&2; exit 1; }
    chmod 700 "$1"
}
xml() { printf '%s' "$1" | sed 's/\&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g; s/"/\&quot;/g; s/\x27/\&apos;/g'; }
toml_path() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }
case ${1:-} in
install)
    : "${DICTATED_BINARY:?set DICTATED_BINARY to the built Metal binary}"
    : "${DICTATE_CLI:?set DICTATE_CLI to the built dictate CLI (pinned model pull)}"
    for dir in "$DATA" "$DATA/bin" "$DATA/run" "$DATA/models" "$LOGS" "$HOME/Library/LaunchAgents"; do secure_dir "$dir"; done
    [[ ! -L $DATA/config.toml && ! -L $PLIST ]] || { echo 'refusing symlink config/plist' >&2; exit 1; }
    # ModelManager verifies the pinned catalog SHA even if the model exists.
    "$DICTATE_CLI" model pull large-v3-turbo
    stage=$(mktemp "$DATA/bin/.dictated.XXXXXX")
    trap 'rm -f "${stage:-}" "${plist_stage:-}"' EXIT
    cp "$DICTATED_BINARY" "$stage"
    chmod 700 "$stage"
    mv -f "$stage" "$DATA/bin/dictated"
    if [[ ! -e $DATA/config.toml ]]; then
        stage=$(mktemp "$DATA/.config.XXXXXX")
        cat > "$stage" <<CONFIG
[audio]
capture = false
[audio.earcons]
enabled = false
[whisper]
model = "large-v3-turbo"
model_path = "$(toml_path "$DATA/models/ggml-large-v3-turbo.bin")"
device = "metal"
[output]
auto_type = false
policy = "off"
[hotkey]
enabled = false
[notifications]
enabled = false
[context]
enabled = false
[dictionary]
enabled = false
[format.llm]
enabled = false
[api]
enabled = true
bind = ""
socket = "$(toml_path "$DATA/run/api.sock")"
CONFIG
        mv -f "$stage" "$DATA/config.toml"
    fi
    "$DATA/bin/dictated" --config "$DATA/config.toml" --check-config
    "$DATA/bin/dictated" --config "$DATA/config.toml" --api-token >/dev/null
    plist_stage=$(mktemp "$HOME/Library/LaunchAgents/.dictated.XXXXXX")
    cat > "$plist_stage" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$LABEL</string>
<key>ProgramArguments</key><array><string>$(xml "$DATA/bin/dictated")</string><string>--config</string><string>$(xml "$DATA/config.toml")</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>10</integer>
<key>ProcessType</key><string>Background</string>
<key>StandardOutPath</key><string>$(xml "$LOGS/stdout.log")</string>
<key>StandardErrorPath</key><string>$(xml "$LOGS/stderr.log")</string>
</dict></plist>
PLIST
    mv -f "$plist_stage" "$PLIST"
    launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
    launchctl bootstrap "$DOMAIN" "$PLIST"
    launchctl kickstart -k "$DOMAIN/$LABEL"
    ;;
uninstall)
    launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
    [[ ! -L $PLIST ]] || { echo 'refusing symlink plist' >&2; exit 1; }
    rm -f "$PLIST"
    echo 'LaunchAgent removed; binary, config, models and state retained.'
    ;;
restart) launchctl kickstart -k "$DOMAIN/$LABEL" ;;
status) launchctl print "$DOMAIN/$LABEL" ;;
*) echo "usage: $0 install|uninstall|restart|status" >&2; exit 2 ;;
esac
