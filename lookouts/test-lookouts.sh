#!/usr/bin/env bash
# test-lookouts.sh — exercise the bundled lookouts against live, read-only APIs.
#
# For each lookout: offline --selftest, then (a) empty watermark -> must print
# only a watermark, (b) an older watermark -> real signals. Every stdout line
# must be JSON with a "title" or a "watermark"; the last line must be a
# watermark. Each run gets a fresh BOSUN_STATE_DIR from mktemp.
#
# Env (defaults are Spencer's machine; override for yours):
#   GOOGLE_WORKSPACE_CLI_CONFIG_DIR   default ~/.config/gws-sns
#   GMAIL_ALLOW_DOMAINS_FILE          default ~/.claude/skills/inbox-janitor/references/client-domains.txt
#   GMAIL_TEST_BACK                   historyIds to rewind for (b) (default 6000, about a day)
#   ORBIT_ENV_FILE                    default ~/.config/orbit-mcp/service-storm.env
#   ORBIT_ORG_SLUG                    default oscar-edge-817a467
#   ORBIT_TEST_SINCE                  ISO watermark for (b) (default 2 days ago)
#   TEAMS_CDP_URL                     default http://127.0.0.1:9222
#   TEAMS_TEST_SINCE                  ISO watermark for (b) (default 2 days ago)
#   SKIP_GMAIL=1 / SKIP_ORBIT=1 / SKIP_TEAMS=1   skip one side
#     (teams live test auto-skips unless the CDP port answers)
#
# Usage: ./lookouts/test-lookouts.sh [--offline]

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FAILS=0
TMPS=()
cleanup() { rm -rf "${TMPS[@]}" 2>/dev/null; }
trap cleanup EXIT

fail() { echo "  FAIL: $*"; FAILS=$((FAILS+1)); }
ok()   { echo "  ok: $*"; }
newstate() { local d; d="$(mktemp -d)"; TMPS+=("$d"); printf '%s' "$d"; }

# validate <file> <label>: every line JSON with title or watermark; last line a watermark.
validate() {
  local f="$1" label="$2" n=0 sig=0 wm=0 bad=0 line
  while IFS= read -r line || [[ -n "$line" ]]; do
    n=$((n+1))
    if ! jq -e 'type == "object" and ((.title|type) == "string" or (.watermark|type) == "string")' <<<"$line" >/dev/null 2>&1; then
      bad=$((bad+1)); echo "    bad line $n: ${line:0:160}"; continue
    fi
    if jq -e '.watermark' <<<"$line" >/dev/null 2>&1; then wm=$((wm+1)); else
      sig=$((sig+1))
      jq -e '(.refs // {}) | to_entries | all(.value|type == "string")' <<<"$line" >/dev/null \
        || { bad=$((bad+1)); echo "    non-string ref on line $n"; }
    fi
  done < "$f"
  LAST_SIGNALS=$sig
  [[ $bad -eq 0 ]] || fail "$label: $bad invalid line(s)"
  [[ $n -gt 0 ]] && tail -n1 "$f" | jq -e '.watermark' >/dev/null 2>&1 || fail "$label: last line is not a watermark"
  [[ $bad -eq 0 ]] && ok "$label: $n lines, $sig signals, $wm watermark line(s)"
}

# run <label> <out> <cmd...> ; captures stdout to <out>, stderr shown indented.
run() {
  local label="$1" out="$2"; shift 2
  local err; err="$(mktemp)"; TMPS+=("$err")
  "$@" >"$out" 2>"$err"; local rc=$?
  sed 's/^/    stderr: /' "$err"
  [[ $rc -eq 0 ]] || fail "$label: exit $rc"
  return $rc
}

echo "== selftests (offline)"
"$HERE/gmail-gws.sh" --selftest   && ok "gmail-gws --selftest"   || fail "gmail-gws --selftest"
"$HERE/orbit-tasks.py" --selftest && ok "orbit-tasks --selftest" || fail "orbit-tasks --selftest"
"$HERE/teams-cdp.mjs" --selftest  && ok "teams-cdp --selftest"   || fail "teams-cdp --selftest"
[[ "${1:-}" == "--offline" ]] && { echo "== $FAILS failure(s)"; exit $(( FAILS > 0 )); }

if [[ "${SKIP_GMAIL:-}" != "1" ]]; then
  echo "== gmail-gws.sh (live, read-only)"
  export GOOGLE_WORKSPACE_CLI_CONFIG_DIR="${GOOGLE_WORKSPACE_CLI_CONFIG_DIR:-$HOME/.config/gws-sns}"
  export GMAIL_ALLOW_DOMAINS_FILE="${GMAIL_ALLOW_DOMAINS_FILE:-$HOME/.claude/skills/inbox-janitor/references/client-domains.txt}"
  out="$(mktemp)"; TMPS+=("$out")
  if run "gmail seed" "$out" env BOSUN_STATE_DIR="$(newstate)" BOSUN_WATERMARK= "$HERE/gmail-gws.sh"; then
    validate "$out" "gmail (a) seed"
    [[ $LAST_SIGNALS -eq 0 ]] || fail "gmail seed emitted $LAST_SIGNALS signals"
    hid="$(jq -r '.watermark' < "$out")"
    back=$(( hid - ${GMAIL_TEST_BACK:-6000} ))
    echo "  seed historyId $hid; rewinding to $back"
    if run "gmail since" "$out" env BOSUN_STATE_DIR="$(newstate)" BOSUN_WATERMARK="$back" "$HERE/gmail-gws.sh"; then
      validate "$out" "gmail (b) since $back"
      [[ $LAST_SIGNALS -gt 0 ]] || echo "  note: no new non-bulk inbox mail in that window"
      jq -c 'select(.title) | {id, title, observedAt}' "$out" | head -3 | sed 's/^/    /'
    fi
    run "gmail stale" "$out" env BOSUN_STATE_DIR="$(newstate)" BOSUN_WATERMARK=1 "$HERE/gmail-gws.sh" \
      && validate "$out" "gmail stale historyId re-seeds"
  fi
fi

if [[ "${SKIP_ORBIT:-}" != "1" ]]; then
  echo "== orbit-tasks.py (live, GET only)"
  if [[ -z "${ORBIT_API_KEY:-}" ]]; then
    set -a; source "${ORBIT_ENV_FILE:-$HOME/.config/orbit-mcp/service-storm.env}"; set +a
  fi
  export ORBIT_ORG_SLUG="${ORBIT_ORG_SLUG:-oscar-edge-817a467}" ORBIT_WORKSPACE_HINT="${ORBIT_WORKSPACE_HINT:-Service Storm}"
  out="$(mktemp)"; TMPS+=("$out")
  st="$(newstate)"
  if run "orbit seed" "$out" env BOSUN_STATE_DIR="$st" BOSUN_WATERMARK= "$HERE/orbit-tasks.py"; then
    validate "$out" "orbit (a) seed"
    [[ $LAST_SIGNALS -eq 0 ]] || fail "orbit seed emitted $LAST_SIGNALS signals"
    echo "  seed watermark $(jq -r .watermark < "$out")"
    since="${ORBIT_TEST_SINCE:-$(date -u -d '2 days ago' +%Y-%m-%dT%H:%M:%SZ)}"
    if run "orbit since" "$out" env BOSUN_STATE_DIR="$st" BOSUN_WATERMARK="$since" "$HERE/orbit-tasks.py"; then
      validate "$out" "orbit (b) since $since"
      jq -c 'select(.title) | {id, title}' "$out" | head -3 | sed 's/^/    /'
    fi
    # Expire the cached access token: the script must refresh on 401 and carry on.
    python3 - "$st/token.json" <<'PY'
import json, sys
p = sys.argv[1]; t = json.load(open(p)); t["accessToken"] = "expired.invalid.token"; json.dump(t, open(p, "w"))
PY
    run "orbit 401 refresh" "$out" env BOSUN_STATE_DIR="$st" BOSUN_WATERMARK="$since" "$HERE/orbit-tasks.py" \
      && validate "$out" "orbit recovers from 401 via refresh"
    [[ "$(python3 -c "import json;print(json.load(open('$st/token.json'))['accessToken'] != 'expired.invalid.token')")" == "True" ]] \
      && ok "token.json rewritten after refresh" || fail "token.json not refreshed"
  fi
fi

if [[ "${SKIP_TEAMS:-}" != "1" ]]; then
  echo "== teams-cdp.mjs (live, read-only via CDP)"
  TEAMS_CDP_URL="${TEAMS_CDP_URL:-http://127.0.0.1:9222}"
  # Only run live if the CDP port answers /json/version; otherwise skip cleanly.
  if curl -sf --max-time 4 "$TEAMS_CDP_URL/json/version" >/dev/null 2>&1; then
    export TEAMS_CDP_URL
    out="$(mktemp)"; TMPS+=("$out")
    if run "teams seed" "$out" env BOSUN_STATE_DIR="$(newstate)" BOSUN_WATERMARK= "$HERE/teams-cdp.mjs"; then
      validate "$out" "teams (a) seed"
      [[ $LAST_SIGNALS -eq 0 ]] || fail "teams seed emitted $LAST_SIGNALS signals"
      since="${TEAMS_TEST_SINCE:-$(date -u -d '2 days ago' +%Y-%m-%dT%H:%M:%SZ)}"
      echo "  seed watermark $(jq -r .watermark < "$out"); rewinding to $since"
      if run "teams since" "$out" env BOSUN_STATE_DIR="$(newstate)" BOSUN_WATERMARK="$since" "$HERE/teams-cdp.mjs"; then
        validate "$out" "teams (b) since $since"
        [[ $LAST_SIGNALS -gt 0 ]] || echo "  note: no new messages from the contact in that window"
        jq -c 'select(.title) | {id, title}' "$out" | head -3 | sed 's/^/    /'
        # from the fresh watermark: nothing new (watermark line only)
        wm="$(jq -r 'select(.watermark) | .watermark' < "$out" | tail -1)"
        run "teams caught-up" "$out" env BOSUN_STATE_DIR="$(newstate)" BOSUN_WATERMARK="$wm" "$HERE/teams-cdp.mjs" \
          && { validate "$out" "teams (c) caught up"; [[ $LAST_SIGNALS -eq 0 ]] || fail "teams caught-up emitted $LAST_SIGNALS signals"; }
      fi
    fi
  else
    echo "  skip: no CDP port at $TEAMS_CDP_URL (Teams not running with --remote-debugging-port)"
  fi
fi

echo "== $FAILS failure(s)"
exit $(( FAILS > 0 ))
