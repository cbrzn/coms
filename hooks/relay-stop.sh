#!/usr/bin/env bash
# Stop hook. Queues this agent's final message for human review instead of
# relaying it to a teammate. Always exits 0 so the agent just goes idle while
# the broker waits on you.
set -uo pipefail

payload=$(cat)

# Not running under tmuxor: do nothing.
[ -n "${TMUXOR_HOME:-}" ] || exit 0

# Subagent turns surface here too. Only the main agent's final message is a
# message to a teammate; a subagent's is an internal report.
[ -z "$(jq -r '.agent_id // empty' <<<"$payload")" ] || exit 0

msg=$(jq -r '.last_assistant_message // empty' <<<"$payload")
[ -n "$msg" ] || exit 0

# The agent addresses a teammate with a routing line on a line of its own.
#
# Insisting it be the FIRST line does not survive contact with a real model —
# they routinely open with a sentence of preamble and put the tag after it. So
# the tag is accepted anywhere, provided there is exactly one: zero means the
# agent did not address anyone, and two or more means the intent is ambiguous.
# Either way "to" is left empty and the broker asks you rather than guessing.
TAG='^[[:space:]]*\**\[TO:?[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*\]\**[[:space:]]*$'
mapfile -t tags < <(grep -nE "$TAG" <<<"$msg")

to=""; body=$msg
if [ "${#tags[@]}" -eq 1 ]; then
  lineno=${tags[0]%%:*}
  to=$(sed -nE 's/^[^:]*:[[:space:]]*\**\[TO:?[[:space:]]*([A-Za-z0-9_-]+)[[:space:]]*\]\**[[:space:]]*$/\1/p' \
       <<<"${tags[0]}" | tr '[:upper:]' '[:lower:]')
  # Drop the routing line itself — it is addressing metadata, not message text.
  body=$(sed "${lineno}d" <<<"$msg" | sed '/./,$!d')
fi

mkdir -p "$TMUXOR_HOME/queue"
f="$TMUXOR_HOME/queue/$(date +%s%N)"
# Write then rename, so the broker never picks up a half-written message.
jq -n --arg from "${TMUXOR_ROLE:-unknown}" --arg to "$to" --arg text "$body" \
   '{from:$from, to:$to, text:$text}' >"$f.part" && mv "$f.part" "$f.json"

exit 0
