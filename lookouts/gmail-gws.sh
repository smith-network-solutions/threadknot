#!/usr/bin/env bash
# gmail-gws.sh — Bosun lookout: new Gmail messages via the `gws` CLI.
#
# Contract (docs/BOSUN.md §4): reads BOSUN_WATERMARK (a Gmail historyId),
# prints one NDJSON signal per new message, then {"watermark": "<historyId>"}.
#
#   First run (empty BOSUN_WATERMARK): prints only the current historyId as
#   the watermark. No signals, so switching it on never floods the Bosun.
#   Later runs: users.history.list(startHistoryId=watermark,
#   historyTypes=messageAdded, labelId=$GMAIL_LABEL), one signal per kept
#   message. If the historyId has expired (Gmail keeps about a week), it
#   re-seeds from the profile and says so on stderr.
#
# Env:
#   GOOGLE_WORKSPACE_CLI_CONFIG_DIR  gws config dir for the account (default ~/.config/gws)
#   GMAIL_LABEL                      label to watch (default INBOX)
#   GMAIL_ALLOW_DOMAINS_FILE         optional; one domain per line, '#' comments.
#                                    Bulk mail (List-Unsubscribe header, or
#                                    Precedence: bulk/list/junk) is skipped
#                                    unless the sender's domain (or a parent
#                                    domain) is listed here.
#   GMAIL_FULL_BODY                  1 = include the decoded text/plain part
#                                    (capped at 8000 chars) instead of the snippet
#   GMAIL_MAX_MESSAGES               max messages per run (default 100); the
#                                    watermark stops at the last one handled
#   BOSUN_WATERMARK, BOSUN_STATE_DIR set by Threadknot
#
# Read-only: uses users.getProfile, users.history.list, users.messages.get.
# Exit codes: 0 ok (including "nothing new"); 2 auth failure; 1 other
# API/network failure (no watermark is printed, so the next run retries).
#
# By hand:   BOSUN_WATERMARK= ./lookouts/gmail-gws.sh | jq
# Self-test: ./lookouts/gmail-gws.sh --selftest   (offline, no gws calls)

set -uo pipefail

LABEL="${GMAIL_LABEL:-INBOX}"
MAX_MESSAGES="${GMAIL_MAX_MESSAGES:-100}"
BODY_CAP=8000

log() { printf 'gmail-gws: %s\n' "$*" >&2; }

# ---------- pure helpers (no network; covered by --selftest) ----------

# Lowercased bare address from a From/To header value.
addr_of() {
  local v="$1"
  if [[ "$v" =~ \<([^>]+)\> ]]; then v="${BASH_REMATCH[1]}"; fi
  v="${v//[[:space:]]/}"
  printf '%s' "${v,,}"
}

# Display name from a From header, falling back to the address.
name_of() {
  local v="$1" n=""
  if [[ "$v" =~ ^[[:space:]]*\"?([^\"<]*[^\"<[:space:]])\"?[[:space:]]*\< ]]; then n="${BASH_REMATCH[1]}"; fi
  if [[ -n "$n" ]]; then printf '%s' "$n"; else addr_of "$v"; fi
}

domain_of() { local a; a="$(addr_of "$1")"; printf '%s' "${a##*@}"; }

# 0 if $1 (a domain) or any parent domain is in allow file $2.
domain_allowed() {
  local d="${1,,}" f="$2" line
  [[ -n "$f" && -r "$f" && -n "$d" ]] || return 1
  while :; do
    while IFS= read -r line || [[ -n "$line" ]]; do
      line="${line%%#*}"; line="${line//[[:space:]]/}"; line="${line,,}"
      [[ -n "$line" && "$line" == "$d" ]] && return 0
    done < "$f"
    [[ "$d" == *.*.* ]] || return 1
    d="${d#*.}"
  done
}

# 0 = skip as bulk. Args: from, list-unsubscribe, precedence, allow file.
is_bulk_skip() {
  local from="$1" unsub="$2" prec="${3,,}" allow="$4"
  prec="${prec//[[:space:]]/}"
  if [[ -z "$unsub" && "$prec" != "bulk" && "$prec" != "list" && "$prec" != "junk" ]]; then
    return 1
  fi
  domain_allowed "$(domain_of "$from")" "$allow" && return 1
  return 0
}

# RFC 2822 Date header -> RFC 3339. Falls back to $2 (epoch ms) then now.
rfc3339_of() {
  local d="$1" ms="${2:-}" out=""
  d="$(printf '%s' "$d" | sed -E 's/\([^)]*\)//g; s/[[:space:]]+$//')"
  if [[ -n "$d" ]]; then out="$(date -d "$d" '+%Y-%m-%dT%H:%M:%S%:z' 2>/dev/null)"; fi
  if [[ -z "$out" && "$ms" =~ ^[0-9]+$ ]]; then out="$(date -d "@$((ms / 1000))" '+%Y-%m-%dT%H:%M:%S%:z' 2>/dev/null)"; fi
  [[ -n "$out" ]] || out="$(date '+%Y-%m-%dT%H:%M:%S%:z')"
  printf '%s' "$out"
}

selftest() {
  local pass=0 fail=0 allow
  allow="$(mktemp)"; trap 'rm -f "$allow"' RETURN
  printf '# clients\nacme.com\n  example.org  # trailing comment\n\nClient.IO\n' > "$allow"
  check() { # desc expected(0|1) cmd...
    local desc="$1" want="$2"; shift 2
    "$@"; local got=$?
    if [[ "$got" == "$want" ]]; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL: $desc (want $want got $got)"; fi
  }
  eq() { if [[ "$2" == "$3" ]]; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL: $1: want '$3' got '$2'"; fi; }

  eq "addr plain"   "$(addr_of 'Bob@Acme.com')" "bob@acme.com"
  eq "addr angle"   "$(addr_of '"Bob Jones" <Bob@Acme.com>')" "bob@acme.com"
  eq "name quoted"  "$(name_of '"Bob Jones" <bob@acme.com>')" "Bob Jones"
  eq "name bare"    "$(name_of 'Railway <hello@notify.railway.app>')" "Railway"
  eq "name none"    "$(name_of '<x@y.com>')" "x@y.com"
  eq "name addr"    "$(name_of 'x@y.com')" "x@y.com"
  eq "domain"       "$(domain_of 'A <a@Mail.Acme.com>')" "mail.acme.com"
  eq "date"         "$(TZ=UTC rfc3339_of 'Mon, 28 Sep 2026 19:17:27 +0000 (UTC)')" "2026-09-28T19:17:27+00:00"
  eq "date fallback" "$(TZ=UTC rfc3339_of 'garbage' 1790000000000)" "2026-09-21T14:13:20+00:00"

  check "plain mail kept"              1 is_bulk_skip 'a@random.com' '' '' "$allow"
  check "list-unsubscribe skipped"     0 is_bulk_skip 'News <n@shop.com>' '<mailto:u@shop.com>' '' "$allow"
  check "precedence bulk skipped"      0 is_bulk_skip 'n@shop.com' '' 'Bulk' "$allow"
  check "precedence list skipped"      0 is_bulk_skip 'n@shop.com' '' ' list ' "$allow"
  check "precedence first-class kept"  1 is_bulk_skip 'n@shop.com' '' 'first-class' "$allow"
  check "allowlisted bulk kept"        1 is_bulk_skip 'Acme <billing@acme.com>' '<https://u>' '' "$allow"
  check "allowlist subdomain kept"     1 is_bulk_skip 'x@mail.acme.com' '' 'bulk' "$allow"
  check "allowlist case-insensitive"   1 is_bulk_skip 'x@client.io' '<u>' '' "$allow"
  check "allowlist w/ comment kept"    1 is_bulk_skip 'x@example.org' '<u>' '' "$allow"
  check "lookalike not allowlisted"    0 is_bulk_skip 'x@notacme.com' '<u>' '' "$allow"
  check "no allow file -> skipped"     0 is_bulk_skip 'x@acme.com' '<u>' '' ''
  check "missing allow file -> skipped" 0 is_bulk_skip 'x@acme.com' '<u>' '' '/nonexistent'

  echo "gmail-gws selftest: $pass passed, $fail failed"
  [[ $fail -eq 0 ]]
}

if [[ "${1:-}" == "--selftest" ]]; then selftest; exit $?; fi

# ---------- gws wrapper ----------

command -v gws >/dev/null || { log "gws not on PATH"; exit 1; }
command -v jq  >/dev/null || { log "jq not on PATH"; exit 1; }

GWS_OUT=""; GWS_RC=0; GWS_ERR=""
# gws_call <resource words...> -- <params json>. Sets GWS_OUT/GWS_RC/GWS_ERR.
gws_call() {
  local params="${*: -1}" errf
  set -- "${@:1:$#-1}"
  errf="$(mktemp)"
  GWS_OUT="$(gws gmail users "$@" --params "$params" 2>"$errf")"; GWS_RC=$?
  GWS_ERR="$(grep -v '^Using keyring backend' "$errf" | head -c 500)"; rm -f "$errf"
  return $GWS_RC
}
api_code() { jq -r '.error.code // empty' <<<"$GWS_OUT" 2>/dev/null; }

# Fatal unless the failure is one we handle; 2 for auth, 1 otherwise.
die_gws() {
  local what="$1" code; code="$(api_code)"
  if [[ $GWS_RC -eq 2 || "$code" == "401" || "$code" == "403" ]]; then
    log "$what: auth failed (gws rc=$GWS_RC code=${code:-?}) ${GWS_ERR}"; exit 2
  fi
  log "$what: failed (gws rc=$GWS_RC code=${code:-?}) ${GWS_ERR}"; exit 1
}

gws_call getProfile '{"userId":"me"}' || die_gws "getProfile"
ME="$(jq -r '.emailAddress // empty' <<<"$GWS_OUT")"; ME="${ME,,}"
PROFILE_HID="$(jq -r '.historyId // empty' <<<"$GWS_OUT")"
[[ -n "$PROFILE_HID" ]] || { log "getProfile returned no historyId"; exit 1; }

emit_watermark() { jq -nc --arg w "$1" '{watermark: $w}'; }

START="${BOSUN_WATERMARK:-}"
if [[ -z "$START" ]]; then
  emit_watermark "$PROFILE_HID"; exit 0
fi
if ! [[ "$START" =~ ^[0-9]+$ ]]; then
  log "watermark '$START' is not a historyId; re-seeding to $PROFILE_HID"
  emit_watermark "$PROFILE_HID"; exit 0
fi

# ---------- walk history ----------

# Lines of "<historyRecordId> <messageId>" for messages added under LABEL.
RECORDS=()
NEW_HID=""
page=""
while :; do
  params="$(jq -nc --arg s "$START" --arg l "$LABEL" --arg p "$page" \
    '{userId:"me", startHistoryId:$s, historyTypes:"messageAdded", labelId:$l, maxResults:500}
     + (if $p == "" then {} else {pageToken:$p} end)')"
  if ! gws_call history list "$params"; then
    if [[ "$(api_code)" == "404" ]]; then
      log "historyId $START is too old or invalid (404); re-seeding to $PROFILE_HID. Mail between the two was not signalled."
      emit_watermark "$PROFILE_HID"; exit 0
    fi
    die_gws "history.list"
  fi
  NEW_HID="$(jq -r '.historyId // empty' <<<"$GWS_OUT")"
  while IFS= read -r rec; do [[ -n "$rec" ]] && RECORDS+=("$rec"); done < <(
    jq -r --arg l "$LABEL" '.history[]? | .id as $h | .messagesAdded[]?
      | select((.message.labelIds // []) | index($l))
      | select((.message.labelIds // []) | (index("DRAFT") or index("SENT") or index("SPAM") or index("TRASH")) | not)
      | "\($h) \(.message.id)"' <<<"$GWS_OUT")
  page="$(jq -r '.nextPageToken // empty' <<<"$GWS_OUT")"
  [[ -n "$page" ]] || break
done
[[ -n "$NEW_HID" ]] || NEW_HID="$PROFILE_HID"

# ---------- fetch + emit ----------

declare -A SEEN=()
handled=0 emitted=0 skipped_bulk=0 skipped_self=0 gone=0
WATERMARK="$NEW_HID"
META_HEADERS='["From","To","Subject","Date","List-Unsubscribe","Precedence"]'

hdr() { jq -r --arg n "${1,,}" '[.payload.headers[]? | select((.name|ascii_downcase) == $n) | .value][0] // ""' <<<"$MSG"; }

for rec in "${RECORDS[@]}"; do
  hid="${rec%% *}"; mid="${rec#* }"
  [[ -n "${SEEN[$mid]:-}" ]] && continue
  SEEN[$mid]=1
  if (( handled >= MAX_MESSAGES )); then
    # Stop here. Restart just before the last record handled (the engine dedupes ids).
    WATERMARK="$((LAST_HID - 1))"
    log "hit GMAIL_MAX_MESSAGES=$MAX_MESSAGES; watermark held at $WATERMARK"
    break
  fi
  handled=$((handled+1)); LAST_HID="$hid"

  params="$(jq -nc --arg id "$mid" --argjson h "$META_HEADERS" '{userId:"me", id:$id, format:"metadata", metadataHeaders:$h}')"
  if ! gws_call messages get "$params"; then
    if [[ "$(api_code)" == "404" ]]; then gone=$((gone+1)); continue; fi
    die_gws "messages.get $mid"
  fi
  MSG="$GWS_OUT"
  # Removed from the label since it arrived (archived by a filter, etc.)? Skip.
  jq -e --arg l "$LABEL" '(.labelIds // []) | index($l)' <<<"$MSG" >/dev/null || { gone=$((gone+1)); continue; }

  from="$(hdr From)"; to="$(hdr To)"; subject="$(hdr Subject)"; date_h="$(hdr Date)"
  unsub="$(hdr List-Unsubscribe)"; prec="$(hdr Precedence)"
  from_addr="$(addr_of "$from")"
  if [[ -n "$ME" && "$from_addr" == "$ME" ]]; then skipped_self=$((skipped_self+1)); continue; fi
  if is_bulk_skip "$from" "$unsub" "$prec" "${GMAIL_ALLOW_DOMAINS_FILE:-}"; then skipped_bulk=$((skipped_bulk+1)); continue; fi

  thread="$(jq -r '.threadId // ""' <<<"$MSG")"
  observed="$(rfc3339_of "$date_h" "$(jq -r '.internalDate // ""' <<<"$MSG")")"
  text="$(jq -r '.snippet // ""' <<<"$MSG")"
  if [[ "${GMAIL_FULL_BODY:-}" == "1" ]]; then
    params="$(jq -nc --arg id "$mid" '{userId:"me", id:$id, format:"full"}')"
    if gws_call messages get "$params"; then
      full="$(jq -r --argjson cap "$BODY_CAP" '
        def b64url: gsub("-";"+") | gsub("_";"/") | . + ("=" * ((4 - (length % 4)) % 4)) | @base64d;
        [.payload | .. | objects | select(.mimeType? == "text/plain") | .body.data? // empty][0]
        // empty | b64url | gsub("\r";"") | .[0:$cap]' <<<"$GWS_OUT" 2>/dev/null)"
      [[ -n "$full" ]] && text="$full"
    else
      log "full body fetch failed for $mid; using snippet"
    fi
  fi

  jq -nc \
    --arg id "gmail:$mid" --arg subject "$subject" --arg who "$(name_of "$from")" \
    --arg from "$from" --arg to "$to" --arg date "$date_h" --arg text "$text" \
    --arg observed "$observed" --arg thread "$thread" --arg mid "$mid" --arg fromAddr "$from_addr" '
    {
      id: $id,
      kind: "email",
      title: ((if $subject == "" then "(no subject)" else $subject end) + " — " + $who),
      body: ("From: " + $from + "\nTo: " + $to + "\nDate: " + $date + "\n\n" + $text),
      observedAt: $observed,
      refs: {
        url: ("https://mail.google.com/mail/u/0/#inbox/" + $thread),
        from: $fromAddr, threadId: $thread, messageId: $mid
      }
    }'
  emitted=$((emitted+1))
done

log "history $START..$NEW_HID: ${#RECORDS[@]} added, $handled examined, $emitted signalled, $skipped_bulk bulk, $skipped_self from self, $gone gone"
emit_watermark "$WATERMARK"
