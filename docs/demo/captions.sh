#!/bin/bash
# Feed a short scene of invented captions to a running ReadSubtitles bridge.
#
# This exists so the demo recording shows the real pipeline - real timings,
# real speaker separation - without putting someone's actual viewing, or
# anyone's copyrighted dialogue, into a public repository. The lines below are
# written for the purpose and deliberately exercise each signal the app reads:
# a speaker label, a dash-alternated cue, and an italic aside.
set -euo pipefail

PORT="${1:-53999}"
TOKEN="$(cat "$HOME/Library/Application Support/readsubtitles/bridge-token")"
ITALIC=$''

# Wait for the reader to be listening rather than guessing how long it takes
# to start. A recording that depends on a guessed delay silently produces an
# empty scene the moment anything upstream gets slower or faster, which is
# exactly how the first attempt at this came out blank.
for _ in $(seq 1 150); do
  (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null && break
  sleep 0.2
done
sleep 1.2

cue() {
  curl -s -o /dev/null --max-time 2 \
    -H "X-ReadSubtitles-Token: $TOKEN" \
    -H 'Content-Type: application/json' \
    --data-binary @- "http://127.0.0.1:$PORT/cue" <<JSON
{"text": $(python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$1")}
JSON
  sleep "${2:-1.6}"
}

cue "[CARTOGRAPHER] The island had not been mapped in a hundred years." 2.0
cue "- Did you hear that?
- I heard nothing at all." 2.0
cue "${ITALIC}She had heard it twice already.${ITALIC}" 2.0
cue "[CARTOGRAPHER] Then we go at first light." 2.0
cue "- And if the charts are wrong?
- Then we draw better ones." 2.2
