#!/usr/bin/env bash
# Stop hook. Queues this instance's final message for human review instead of
# relaying it to the peer directly. Always exits 0 so the instance just goes
# idle while the broker waits on you.
set -uo pipefail

payload=$(cat)

# Not running under duo: do nothing.
[ -n "${DUO_HOME:-}" ] || exit 0

# Subagent turns surface here too. Only the main agent's final message is a
# message to the peer; a subagent's is an internal report.
[ -z "$(jq -r '.agent_id // empty' <<<"$payload")" ] || exit 0

msg=$(jq -r '.last_assistant_message // empty' <<<"$payload")
[ -n "$msg" ] || exit 0

mkdir -p "$DUO_HOME/queue"
f="$DUO_HOME/queue/$(date +%s%N)"
# Write then rename, so the broker never picks up a half-written message.
jq -n --arg from "${DUO_ROLE:-unknown}" --arg text "$msg" \
   '{from:$from, text:$text}' >"$f.part" && mv "$f.part" "$f.json"

exit 0
