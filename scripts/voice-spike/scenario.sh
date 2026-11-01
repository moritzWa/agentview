#!/bin/bash
# scenario.sh NAME SECONDS "question@s" ["question@s" ...]: one call with the real dashboard, then its log as a conversation.
# Extra environment: E2E_SELECT_DOWN=n presses Down n times first, so "this session" is the nth row.
set -u
name=$1; seconds=$2; shift 2
log=${VOICE_SCENARIO_DIR:-/tmp}/voice-$name.jsonl
rm -f "$log"
AGENTVIEW_VOICE_LOG=$log AGENTVIEW_VOICE_PORT=${AGENTVIEW_VOICE_PORT:-47699} E2E_SECONDS=$seconds \
  ~/.cache/voice-e2e-venv/bin/python "$(dirname "$0")/e2e.py" "$@" "${AGENTVIEW_BIN:-$PWD/target/debug/agentview}" >/dev/null 2>&1
python3 "$(dirname "$0")/summarize.py" "$log"
