#!/bin/bash
# A second local S33 client. Audio is stdout JSON; credentials enter curl on
# stdin only. No retry: a failed response may follow accepted transcription.
set -euo pipefail
set +x
[[ $# -eq 1 && -r $1 && -f $1 ]] || { echo "usage: $0 WAV" >&2; exit 2; }
directory=${DICTATE_LOCAL_DIRECTORY:-"$HOME/Library/Application Support/dictated"}
token=$(cat "$directory/api-token") || { echo "Cannot read dictated API token" >&2; exit 1; }
# Refuse config-file metacharacters and control characters (including embedded
# newlines) rather than allowing them to inject additional curl options.
[[ -n $token && $token != *[!A-Za-z0-9_.-]* ]] || { echo "Invalid dictated API token" >&2; exit 1; }
printf 'header = "Authorization: Bearer %s"\n' "$token" |
    curl --disable --silent --show-error --fail-with-body --max-time 120 \
        --unix-socket "$directory/run/api.sock" -K - \
        --header 'Content-Type: audio/wav' --data-binary "@$1" \
        http://localhost/v1/transcribe
