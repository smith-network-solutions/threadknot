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
# Sweep mode (--sweep): no watermark. Lists inbox threads (in:inbox
# newer_than:GMAIL_SWEEP_DAYS d) whose newest message is from someone else and
# older than GMAIL_SWEEP_MIN_AGE_HOURS, grouped "Waiting on you" (allow-listed
# domain, or a person with the account in To/Cc) and "Probably a task"
# (notifications about a failed build, form submission, security alert,
# signature request, ...). Bulk, auto-replies and calendar mail are skipped.
# Prints ONE signal (kind "sweep", id gmail:sweep:<account>:<YYYY-MM-DDTHH>
# local) per run inside the window, even when nothing is listed; nothing
# outside the window. Besides the human body it carries a top-level `items`
# array (every listed thread, uncapped: key, title, client, detail, url,
# group) and refs.scope = gmail:<account>: plus refs.openKeys = keys of every
# thread still waiting on you (before the snooze and min-age filters), so a
# thread you answered drops out and the engine closes its task.
# Task keys (docs/BOSUN.md §10.2): poll signals carry
# refs.key = gmail:<account>:<threadId>.
#   SWEEP_HOURS                local window, default 08:00-18:00 (end exclusive)
#   SWEEP_DAYS                 default mon-fri (also "mon,wed", "all")
#   SWEEP_ALWAYS=1             ignore the window (--sweep --now sets it)
#   GMAIL_SWEEP_MIN_AGE_HOURS  default 20
#   GMAIL_SWEEP_DAYS           default 14
#   GMAIL_WORKSPACE_HINT       optional; copied to hints.workspace
#   Snooze: BOSUN_STATE_DIR/snooze.json, {"<threadId>": "<ISO date|datetime>"};
#   hidden until then, expired entries pruned on read. Sweep uses
#   users.threads.list and users.threads.get (metadata) only.
#
# By hand:   BOSUN_WATERMARK= ./lookouts/gmail-gws.sh | jq
# Sweep:     BOSUN_STATE_DIR=$(mktemp -d) ./lookouts/gmail-gws.sh --sweep --now | jq -r .body
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

# ---------- sweep helpers (pure; covered by --selftest) ----------

SWEEP_BODY_CAP=32768

# "08:00" -> minutes since midnight. Nonzero on junk.
hhmm_min() {
  [[ "$1" =~ ^([0-9]{1,2}):([0-9]{2})$ ]] || return 1
  local h=$((10#${BASH_REMATCH[1]})) m=$((10#${BASH_REMATCH[2]}))
  (( h <= 24 && m < 60 )) || return 1
  printf '%s' $((h * 60 + m))
}

# Day spec ("mon-fri", "mon,wed", "fri-mon", "all") -> space list of ISO weekdays (Mon=1).
parse_days() {
  local spec="${1,,}" part a b i out=" " names=(mon tue wed thu fri sat sun)
  spec="${spec//[[:space:]]/}"
  case "$spec" in ""|all|"*"|daily) printf ' 1 2 3 4 5 6 7 '; return 0 ;; esac
  day_num() { local n; for n in 0 1 2 3 4 5 6; do [[ "${1:0:3}" == "${names[$n]}" ]] && { printf '%s' $((n + 1)); return 0; }; done; return 1; }
  IFS=, read -ra parts <<<"$spec"
  for part in "${parts[@]}"; do
    if [[ "$part" == *-* ]]; then
      a="$(day_num "${part%%-*}")" || return 1; b="$(day_num "${part#*-}")" || return 1
      i=$a
      while :; do out+="$i "; [[ $i -eq $b ]] && break; i=$(( i % 7 + 1 )); done
    elif [[ -n "$part" ]]; then
      a="$(day_num "$part")" || return 1; out+="$a "
    fi
  done
  printf '%s' "$out"
}

# in_window <hours "HH:MM-HH:MM"> <days spec> <now "HH:MM"> <ISO weekday 1-7>
# 0 inside, 1 outside, 2 bad spec. End exclusive; end < start wraps midnight.
in_window() {
  local s e m days
  s="$(hhmm_min "${1%%-*}")" || return 2; e="$(hhmm_min "${1#*-}")" || return 2
  m="$(hhmm_min "$3")" || return 2
  days="$(parse_days "$2")" || return 2
  [[ "$days" == *" $4 "* ]] || return 1
  if (( s <= e )); then (( s <= m && m < e )) && return 0; return 1; fi
  (( m >= s || m < e )) && return 0; return 1
}

# snooze_active <file> <now epoch>: prints active snoozed ids, one per line.
# Expired or unparseable entries are pruned from the file (rewritten only if
# something changed). Bare dates mean local midnight of that day.
snooze_active() {
  local f="$1" now="$2" k v until keep="{}" pruned=0
  [[ -r "$f" ]] || return 0
  if ! jq -e 'type == "object"' "$f" >/dev/null 2>&1; then log "snooze file is not a JSON object; ignoring it"; return 0; fi
  while IFS=$'\t' read -r k v; do
    until="$(date -d "$v" +%s 2>/dev/null)"
    if [[ -n "$until" && "$until" -gt "$now" ]]; then
      printf '%s\n' "$k"
      keep="$(jq -c --arg k "$k" --arg v "$v" '. + {($k): $v}' <<<"$keep")"
    else
      pruned=$((pruned + 1))
    fi
  done < <(jq -r 'to_entries[] | [.key, (.value | tostring)] | @tsv' "$f")
  if (( pruned > 0 )); then
    jq . <<<"$keep" > "$f.tmp" && mv "$f.tmp" "$f" && log "snooze: pruned $pruned expired entr$([[ $pruned -eq 1 ]] && echo y || echo ies)"
  fi
}

# Machine sender: bulk-ish local part, a sending subdomain, or Gmail's non-personal category.
is_machine() { # from category autosubmitted
  local a d local_part; a="$(addr_of "$1")"; local_part="${a%@*}"; d="${a##*@}"
  [[ "$local_part" =~ ^(.*[-_.])?(no-?reply|do-?not-?reply|donotreply|notifications?|notify|alerts?|mailer-daemon|postmaster|bounces?|automated|system|forms?|builds?|calendar-notification|dse|security|accounts?|updates?|news|newsletter|info|hello|team|support|billing|receipts?|invoices?|reports?|leads?)([-_.+].*)?$ ]] && return 0
  [[ "$d" =~ ^(notify|notifications?|mail|email|send|bounces?|mailer|em|e|news|mg|sg)\. ]] && return 0
  [[ -n "$2" && "$2" != "CATEGORY_PERSONAL" ]] && return 0
  [[ "${3,,}" == auto-generated* ]] && return 0
  return 1
}

TASK_RE='(new|form|contact|web(site)?) (form )?(submission|entry|lead|inquiry|enquiry|request)|contact form|submitted|(build|deploy(ment)?|job|workflow|run|crawl|backup|sync|payment|charge|renewal) (has )?(failed|failure|error)|failed|failing|security alert|sign-?in|login attempt|suspicious|password (reset|changed)|verify your|docusign|e-?signature|please (sign|review)|signature (requested|required|needed)|awaiting your signature|waiting for (your )?signature|action (required|requested|needed)|requires your (attention|action)|past due|overdue|payment (declined|due)|expir(es|ing|ed)|run payroll|approval (needed|required)|needs your approval|disk (space|full)|is down|outage'

# sweep_class <from> <to+cc> <subject> <list-unsubscribe> <precedence> <auto-submitted>
#             <x-autoreply> <category> <content-class/type> <snippet> <allow file> <me>
# -> waiting | task | skip-self | skip-auto | skip-calendar | skip-bulk | skip-other
sweep_class() {
  local from="$1" to="${2,,}" subj="${3,,}" unsub="$4" prec="$5" autos="${6,,}" xauto="$7" cat="$8"
  local ctype="${9,,}" snip="${10,,}" allow="${11}" me="${12,,}" fa
  fa="$(addr_of "$from")"
  [[ -n "$me" && "$fa" == "$me" ]] && { echo skip-self; return; }
  if [[ "$autos" == auto-replied* || -n "$xauto" \
     || "$subj" =~ ^(automatic\ reply|auto(matic)?[\ -]?reply|autoreply|auto:|out\ of\ (the\ )?office|ooo[:\ ]) ]]; then
    echo skip-auto; return
  fi
  if [[ "$fa" == calendar-notification@google.com || "$ctype" == *calendar* \
     || "$subj" =~ ^(invitation|updated\ invitation|accepted|declined|tentative(ly\ accepted)?|canceled|cancelled|new\ event|event\ canceled|invitation\ from\ google\ calendar)[:\ ] ]]; then
    echo skip-calendar; return
  fi
  is_bulk_skip "$from" "$unsub" "$prec" "$allow" && { echo skip-bulk; return; }
  # A person: allow-listed domain, or addressed to the account. Allow-listed
  # machine senders (a bank's alerts) fall through to the task check. Gmail's
  # category is not trusted for client domains (it files some clients under Updates).
  if domain_allowed "$(domain_of "$from")" "$allow"; then
    is_machine "$from" "" "$autos" || { echo waiting; return; }
  elif ! is_machine "$from" "$cat" "$autos" && [[ -n "$me" && "$to" == *"$me"* ]]; then
    echo waiting; return
  fi
  local text="$subj $snip"
  text="${text//no action needed/}"; text="${text//no action required/}"
  if [[ "$text" =~ $TASK_RE ]]; then echo task; return; fi
  echo skip-other
}

# Snippet tidy: entities, zero-width padding, whitespace; capped.
tidy_snippet() {
  jq -rn --arg s "$1" '$s | gsub("&#39;"; [39] | implode) | gsub("&quot;"; [34] | implode) | gsub("&lt;";"<")
    | gsub("&gt;";">") | gsub("&amp;";"&")
    | gsub("[" + ([8203, 8204, 8205, 8206, 8207, 8288, 6158, 847, 173, 65279, 8199, 160] | implode) + "]"; " ") | gsub("\\s+";" ")
    | ltrimstr(" ") | rtrimstr(" ") | if length > 140 then .[0:140] + "..." else . end'
}

# sweep_line <age h> <from header> <subject> <snippet> <threadId>
sweep_line() {
  local who addr subj="${3:-(no subject)}"
  who="$(name_of "$2")"; addr="$(addr_of "$2")"
  [[ "$who" == "$addr" ]] && who="" || who="$who "
  printf -- '- %sh · %s<%s> · %s — %s · https://mail.google.com/mail/u/0/#inbox/%s (id %s)' \
    "$1" "$who" "$addr" "$subj" "$(tidy_snippet "$4")" "$5" "$5"
}

# sweep_id <account> [YYYY-MM-DDTHH]: the per-hour signal id.
sweep_id() { printf 'gmail:sweep:%s:%s' "$1" "${2:-$(date +%Y-%m-%dT%H)}"; }

# thread_key <account> <threadId>: the Bosun task key (docs/BOSUN.md §10.2).
thread_key() { printf 'gmail:%s:%s' "${1,,}" "$2"; }

FREE_MAIL_RE='^(gmail|googlemail|outlook|hotmail|live|msn|yahoo|ymail|aol|icloud|me|mac|proton|protonmail|pm|gmx|zoho|comcast|att|sbcglobal|verizon|fastmail)\.'

# client_label <from header>: the sender's organisation as a short label: the
# registrable domain ("mail.acme.co.uk" -> "acme.co.uk"), or the sender's name
# for personal mailboxes (gmail.com, outlook.com, ...).
client_label() {
  local d n; d="$(domain_of "$1")"
  if [[ "$d" =~ $FREE_MAIL_RE ]]; then name_of "$1"; return; fi
  IFS=. read -ra parts <<<"$d"
  n=${#parts[@]}
  if (( n >= 3 )) && [[ ${#parts[n-1]} -eq 2 && "${parts[n-2]}" =~ ^(co|com|org|net|ac|gov|edu)$ ]]; then
    printf '%s.%s.%s' "${parts[n-3]}" "${parts[n-2]}" "${parts[n-1]}"
  elif (( n >= 2 )); then
    printf '%s.%s' "${parts[n-2]}" "${parts[n-1]}"
  else
    printf '%s' "$d"
  fi
}

# sweep_item <group waiting|task> <age h> <from header> <subject> <snippet> <threadId> <account>
# -> one compact JSON object for the sweep's top-level `items` array.
sweep_item() {
  local who addr subj="${4:-(no subject)}"
  who="$(name_of "$3")"; addr="$(addr_of "$3")"
  jq -nc --arg g "$1" --arg age "$2" --arg who "$who" --arg addr "$addr" --arg subj "$subj" \
    --arg snip "$(tidy_snippet "$5")" --arg tid "$6" --arg key "$(thread_key "$7" "$6")" --arg client "$(client_label "$3")" '
    {key: $key,
     title: (if $g == "waiting" then "Reply to " + $who + ": " + $subj else $subj + " (" + $who + ")" end),
     client: $client,
     detail: ($age + "h · " + (if $who == $addr then "" else $who + " " end) + "<" + $addr + ">"
              + (if $snip == "" then "" else " — " + $snip end)),
     url: ("https://mail.google.com/mail/u/0/#inbox/" + $tid),
     group: $g}'
}

# cap_lines <cap bytes> <footer> < body: drops trailing lines to fit; footer always kept.
cap_lines() {
  local cap="$1" footer="$2" body dropped=0
  body="$(cat)"
  local tail=$'\n\n'"$footer"
  if (( $(printf '%s%s' "$body" "$tail" | wc -c) <= cap )); then printf '%s%s' "$body" "$tail"; return; fi
  mapfile -t L <<<"$body"
  while (( ${#L[@]} > 0 )); do
    unset 'L[${#L[@]}-1]'; dropped=$((dropped + 1))
    local cand; cand="$(printf '%s\n' "${L[@]}")…$dropped more lines not shown$tail"
    (( $(printf '%s' "$cand" | wc -c) <= cap )) && { printf '%s' "$cand"; return; }
  done
  printf '%s' "…$dropped more lines not shown$tail"
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

  # -- sweep: window
  eq "hhmm"            "$(hhmm_min 08:30)" "510"
  check "hhmm junk"    1 hhmm_min 8am
  eq "days mon-fri"    "$(parse_days mon-fri)" " 1 2 3 4 5 "
  eq "days list"       "$(parse_days 'Mon, wed')" " 1 3 "
  eq "days wrap"       "$(parse_days fri-mon)" " 5 6 7 1 "
  eq "days all"        "$(parse_days all)" " 1 2 3 4 5 6 7 "
  check "in window"          0 in_window 08:00-18:00 mon-fri 09:00 1
  check "end exclusive"      1 in_window 08:00-18:00 mon-fri 18:00 1
  check "before start"       1 in_window 08:00-18:00 mon-fri 07:59 1
  check "weekend out"        1 in_window 08:00-18:00 mon-fri 12:00 7
  check "overnight wrap"     0 in_window 22:00-06:00 all 23:30 3
  check "tiny window"        1 in_window 00:00-00:01 all 09:00 1
  check "bad spec"           2 in_window 8-6 mon-fri 09:00 1

  # -- sweep: snooze filter + prune (now = 2026-09-28 09:00 local)
  local sn now9; sn="$(mktemp)"; now9="$(date -d '2026-09-28 09:00' +%s)"
  printf '{"a":"2026-09-29","b":"2026-09-28T08:00:00","c":"2026-09-28T12:00","d":"2026-09-28","e":"garbage"}' > "$sn"
  eq "snooze keeps future" "$(snooze_active "$sn" "$now9" 2>/dev/null | sort | tr '\n' ' ')" "a c "
  eq "snooze pruned on disk" "$(jq -r 'keys | join(" ")' "$sn")" "a c"
  eq "snooze missing file" "$(snooze_active /nonexistent "$now9")" ""
  rm -f "$sn"

  # -- sweep: classification  (from to subject unsub prec autos xauto category ctype snippet allow me)
  local me=spencer@example.com
  eq "person to me"      "$(sweep_class 'Bob <bob@random.com>' "Spencer <$me>, " 'Question' '' '' '' '' CATEGORY_PERSONAL 'text/plain' 'hi' "$allow" "$me")" waiting
  eq "client domain"     "$(sweep_class 'x@acme.com' 'group@acme.com, ' 'FYI' '' '' '' '' CATEGORY_UPDATES '' '' "$allow" "$me")" waiting
  eq "from me"           "$(sweep_class "Me <$me>" 'a@b.com' 'Re: x' '' '' '' '' '' '' '' "$allow" "$me")" skip-self
  eq "auto-reply hdr"    "$(sweep_class 'bob@random.com' "$me" 'Re: x' '' '' 'auto-replied' '' '' '' '' "$allow" "$me")" skip-auto
  eq "ooo subject"       "$(sweep_class 'bob@random.com' "$me" 'Out of Office: back Monday' '' '' '' '' '' '' '' "$allow" "$me")" skip-auto
  eq "calendar invite"   "$(sweep_class 'bob@random.com' "$me" 'Invitation: Sync @ Tue' '' '' '' '' '' '' '' "$allow" "$me")" skip-calendar
  eq "outlook invite"    "$(sweep_class 'bob@random.com' "$me" '3:00 meeting' '' '' '' '' '' 'urn:content-classes:calendarmessage' '' "$allow" "$me")" skip-calendar
  eq "bulk skipped"      "$(sweep_class 'News <n@shop.com>' "$me" 'Sale' '<u>' '' '' '' '' '' '' "$allow" "$me")" skip-bulk
  eq "build failed"      "$(sweep_class 'Railway <hello@notify.railway.app>' "$me" 'Build failed for g5' '' '' '' '' CATEGORY_PERSONAL '' '' "$allow" "$me")" task
  eq "docusign"          "$(sweep_class 'DocuSign <dse@docusign.net>' "$me" 'Please DocuSign: Contract' '' '' '' '' CATEGORY_UPDATES '' '' "$allow" "$me")" task
  eq "form submission"   "$(sweep_class 'forms@site.io' 'x@site.io' 'New form submission' '' '' '' '' '' '' '' "$allow" "$me")" task
  eq "security alert"    "$(sweep_class 'Google <no-reply@accounts.google.com>' "$me" 'Security alert' '' '' 'auto-generated' '' CATEGORY_UPDATES '' '' "$allow" "$me")" task
  eq "category notice"   "$(sweep_class 'Ahrefs <sa@ahrefs.com>' "$me" 'Site Audit crawl failed' '' '' '' '' CATEGORY_UPDATES '' '' "$allow" "$me")" task
  eq "allow-listed notifier" "$(sweep_class 'Mercury <hello@mercury.com>' "$me" 'Card charged' '' '' '' '' CATEGORY_PERSONAL '' '' <(echo mercury.com) "$me")" skip-other
  eq "allow-listed person"   "$(sweep_class 'Ann <ann@mercury.com>' 'x@y.com' 'Hi' '' '' '' '' CATEGORY_PERSONAL '' '' <(echo mercury.com) "$me")" waiting
  eq "no action needed"   "$(sweep_class 'Bank <no-reply@bank.com>' "$me" 'Role renamed' '' '' '' '' CATEGORY_UPDATES '' 'No action needed - nothing changed' "$allow" "$me")" skip-other
  eq "plain notification" "$(sweep_class 'Mercury <hello@mercury.com>' "$me" 'You received $10' '' '' '' '' CATEGORY_UPDATES '' 'deposit' "$allow" "$me")" skip-other

  # -- sweep: line + id
  eq "sweep line" "$(sweep_line 26 '"Bob Jones" <bob@acme.com>' 'Quote' 'Can you send it&#39;s   price?' 't1')" \
    "- 26h · Bob Jones <bob@acme.com> · Quote — Can you send it's price? · https://mail.google.com/mail/u/0/#inbox/t1 (id t1)"
  eq "sweep line bare" "$(sweep_line 3 'x@y.com' '' 's' 't2')" "- 3h · <x@y.com> · (no subject) — s · https://mail.google.com/mail/u/0/#inbox/t2 (id t2)"
  eq "sweep id"   "$(sweep_id spencer@example.com 2026-09-28T09)" "gmail:sweep:spencer@example.com:2026-09-28T09"

  # -- task keys + sweep items
  eq "thread key"        "$(thread_key Spencer@Example.com 19a0f)" "gmail:spencer@example.com:19a0f"
  eq "client domain"     "$(client_label 'Bob <bob@mail.acme.com>')" "acme.com"
  eq "client cc tld"     "$(client_label 'x@shop.acme.co.uk')" "acme.co.uk"
  eq "client bare"       "$(client_label 'x@acme.io')" "acme.io"
  eq "client free mail"  "$(client_label '"Jo Smith" <jo.smith@gmail.com>')" "Jo Smith"
  local it
  it="$(sweep_item waiting 26 '"Bob Jones" <bob@acme.com>' 'Quote' 'Can you send it&#39;s price?' t1 spencer@example.com)"
  eq "item waiting" "$it" '{"key":"gmail:spencer@example.com:t1","title":"Reply to Bob Jones: Quote","client":"acme.com","detail":"26h · Bob Jones <bob@acme.com> — Can you send it'"'"'s price?","url":"https://mail.google.com/mail/u/0/#inbox/t1","group":"waiting"}'
  it="$(sweep_item task 3 'Railway <hello@notify.railway.app>' 'Build failed for g5' '' t2 me@x.io)"
  eq "item task title"   "$(jq -r .title <<<"$it")" "Build failed for g5 (Railway)"
  eq "item task client"  "$(jq -r .client <<<"$it")" "railway.app"
  eq "item task detail"  "$(jq -r .detail <<<"$it")" "3h · Railway <hello@notify.railway.app>"
  eq "item no subject"   "$(sweep_item waiting 1 'x@y.com' '' 's' t3 me@x.io | jq -r .title)" "Reply to x@y.com: (no subject)"
  [[ "$(sweep_id a@b.c)" =~ ^gmail:sweep:a@b\.c:[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}$ ]]; check "sweep id format now" 0 test $? -eq 0
  local capped; capped="$(for i in $(seq 1 300); do printf -- '- %0200d\n' "$i"; done | cap_lines 32768 $'---\nfooter')"
  check "cap under 32 KiB"   0 test "$(printf '%s' "$capped" | wc -c)" -le 32768
  eq "cap keeps footer"      "${capped: -10}" $'---\nfooter'
  check "cap notes dropped"  0 grep -q 'more lines not shown' <<<"$capped"

  echo "gmail-gws selftest: $pass passed, $fail failed"
  [[ $fail -eq 0 ]]
}

if [[ "${1:-}" == "--selftest" ]]; then selftest; exit $?; fi

SWEEP=0
for arg in "$@"; do
  case "$arg" in
    --sweep) SWEEP=1 ;;
    --now) export SWEEP_ALWAYS=1 ;;
  esac
done
# Outside the sweep window: print nothing, touch nothing.
if (( SWEEP )) && [[ "${SWEEP_ALWAYS:-}" != "1" ]]; then
  in_window "${SWEEP_HOURS:-08:00-18:00}" "${SWEEP_DAYS:-mon-fri}" "$(date +%H:%M)" "$(date +%u)"
  case $? in
    0) ;;
    1) exit 0 ;;
    *) log "bad SWEEP_HOURS/SWEEP_DAYS ('${SWEEP_HOURS:-}' / '${SWEEP_DAYS:-}')"; exit 1 ;;
  esac
fi

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

# ---------- sweep mode ----------

SWEEP_HEADERS='["From","To","Cc","Subject","List-Unsubscribe","Precedence","Auto-Submitted","X-Autoreply","X-Autorespond","Content-Class","Content-Type"]'

# One thread -> one compact JSON line describing its newest real message
# (drafts, spam and trash ignored), or {"threadId":…,"error":…}. Runs in xargs.
sweep_fetch_one() {
  local id="$1" dir="$2" out rc params
  params="$(jq -nc --arg id "$id" --argjson h "$SWEEP_HEADERS" '{userId:"me", id:$id, format:"metadata", metadataHeaders:$h}')"
  out="$(gws gmail users threads get --params "$params" 2>/dev/null)"; rc=$?
  if [[ $rc -ne 0 ]] || ! jq -e '.messages' <<<"$out" >/dev/null 2>&1; then
    jq -nc --arg id "$id" --arg c "$(jq -r '.error.code // empty' <<<"$out" 2>/dev/null)" --arg rc "$rc" \
      '{threadId:$id, error:(if $c == "" then ("rc " + $rc) else $c end)}' > "$dir/$id"
    return 0
  fi
  jq -c --arg id "$id" '
    [.messages[]? | select((.labelIds // []) | (index("DRAFT") or index("SPAM") or index("TRASH")) | not)]
    | sort_by(.internalDate | tonumber) | last as $m
    | if $m == null then {threadId:$id, empty:true} else
      def h($n): [$m.payload.headers[]? | select((.name | ascii_downcase) == $n) | .value][0] // "";
      {threadId:$id, internalDate:$m.internalDate, from:h("from"), to:(h("to") + ", " + h("cc")),
       subject:h("subject"), unsub:h("list-unsubscribe"), prec:h("precedence"), autos:h("auto-submitted"),
       xauto:(h("x-autoreply") + h("x-autorespond")), ctype:(h("content-class") + " " + h("content-type")),
       category:([($m.labelIds // [])[] | select(startswith("CATEGORY_"))][0] // ""), snippet:($m.snippet // "")}
      end' <<<"$out" > "$dir/$id"
}

run_sweep() {
  local min_age="${GMAIL_SWEEP_MIN_AGE_HOURS:-20}" days="${GMAIL_SWEEP_DAYS:-14}" now page params tmp
  local state="${BOSUN_STATE_DIR:-${TMPDIR:-/tmp}/gmail-gws-sweep}"
  [[ "$min_age" =~ ^[0-9]+$ ]] || min_age=20
  [[ "$days" =~ ^[0-9]+$ ]] || days=14
  mkdir -p "$state"
  local snooze_file; snooze_file="$(cd "$state" && pwd)/snooze.json"
  now="$(date +%s)"
  declare -A SNOOZED=()
  local sid; while IFS= read -r sid; do [[ -n "$sid" ]] && SNOOZED[$sid]=1; done < <(snooze_active "$snooze_file" "$now")

  local ids=()
  page=""
  while :; do
    params="$(jq -nc --arg q "in:inbox newer_than:${days}d" --arg p "$page" \
      '{userId:"me", q:$q, maxResults:500} + (if $p == "" then {} else {pageToken:$p} end)')"
    gws_call threads list "$params" || die_gws "threads.list"
    while IFS= read -r sid; do [[ -n "$sid" ]] && ids+=("$sid"); done < <(jq -r '.threads[]?.id' <<<"$GWS_OUT")
    page="$(jq -r '.nextPageToken // empty' <<<"$GWS_OUT")"
    [[ -n "$page" ]] || break
  done

  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  export -f sweep_fetch_one; export SWEEP_HEADERS
  (( ${#ids[@]} )) && printf '%s\n' "${ids[@]}" | xargs -P 8 -I{} bash -c 'sweep_fetch_one "$1" "$2"' _ {} "$tmp"

  local wait_lines=() task_lines=() wait_items=() task_items=() open_keys=() snoozed=0 young=0 errs=0 authfail=0
  declare -A why=()
  local rec f tid ms age cls
  for f in "$tmp"/*; do
    [[ -e "$f" ]] || continue
    rec="$(cat "$f")"
    tid="$(jq -r '.threadId' <<<"$rec")"
    if jq -e '.error' <<<"$rec" >/dev/null; then
      case "$(jq -r '.error' <<<"$rec")" in
        404) ;;                                  # deleted since the list call
        401|403|"rc 2") authfail=1 ;;
        *) errs=$((errs + 1)) ;;
      esac
      continue
    fi
    jq -e '.empty' <<<"$rec" >/dev/null && continue
    ms="$(jq -r '.internalDate // "0"' <<<"$rec")"
    age=$(( (now - ms / 1000) / 3600 ))
    local from to subject unsub prec autos xauto category ctype snippet
    { IFS= read -r from; IFS= read -r to; IFS= read -r subject; IFS= read -r unsub; IFS= read -r prec
      IFS= read -r autos; IFS= read -r xauto; IFS= read -r category; IFS= read -r ctype; IFS= read -r snippet; } < <(
      jq -r '.from, .to, .subject, .unsub, .prec, .autos, .xauto, .category, .ctype, .snippet | gsub("[\r\n]+"; " ")' <<<"$rec")
    cls="$(sweep_class "$from" "$to" "$subject" "$unsub" "$prec" "$autos" "$xauto" "$category" "$ctype" "$snippet" \
      "${GMAIL_ALLOW_DOMAINS_FILE:-}" "$ME")"
    # Still waiting on you, listed or not (too young, snoozed): its task stays
    # open. A thread you answered, archived, or that aged out of the query is
    # absent from openKeys and the engine closes its task.
    if [[ "$cls" == waiting || "$cls" == task ]]; then open_keys+=("$(thread_key "$ME" "$tid")"); fi
    if [[ "$cls" == waiting || "$cls" == task ]] && (( age < min_age )); then young=$((young + 1)); continue; fi
    if [[ ( "$cls" == waiting || "$cls" == task ) && -n "${SNOOZED[$tid]:-}" ]]; then snoozed=$((snoozed + 1)); continue; fi
    case "$cls" in
      waiting) wait_lines+=("$(printf '%06d' "$age")"$'\t'"$(sweep_line "$age" "$from" "$subject" "$snippet" "$tid")")
               wait_items+=("$(printf '%06d' "$age")"$'\t'"$(sweep_item waiting "$age" "$from" "$subject" "$snippet" "$tid" "$ME")") ;;
      task)    task_lines+=("$(printf '%06d' "$age")"$'\t'"$(sweep_line "$age" "$from" "$subject" "$snippet" "$tid")")
               task_items+=("$(printf '%06d' "$age")"$'\t'"$(sweep_item task "$age" "$from" "$subject" "$snippet" "$tid" "$ME")") ;;
      *)       why[$cls]=$(( ${why[$cls]:-0} + 1 )) ;;
    esac
  done
  (( authfail )) && { log "threads.get: auth failed"; exit 2; }
  (( errs )) && { log "threads.get failed for $errs thread(s); not signalling a partial list"; exit 1; }

  local nw=${#wait_lines[@]} nt=${#task_lines[@]} total
  total=$((nw + nt))
  local skipped=""; for cls in "${!why[@]}"; do skipped+=" ${cls#skip-} ${why[$cls]},"; done
  log "sweep: ${#ids[@]} inbox threads, waiting $nw, task $nt, snoozed $snoozed, younger than ${min_age}h $young, skipped:${skipped%,}; ${#open_keys[@]} open keys"
  # Signal even when nothing is listed: the engine needs openKeys to close the
  # tasks of threads that stopped waiting.

  # Lines per group: triage needs the shape, the work turn can re-run the sweep.
  local per_group="${GMAIL_SWEEP_MAX_PER_GROUP:-15}"
  local body="" sorted more
  if (( nw )); then
    sorted="$(printf '%s\n' "${wait_lines[@]}" | sort -r | cut -f2- | head -n "$per_group")"
    body+="### Waiting on you ($nw)"$'\n'"$sorted"
    more=$((nw - per_group)); (( more > 0 )) && body+=$'\n'"…$more more in this group; run the sweep with --sweep --now for the full list"
  fi
  if (( nt )); then
    sorted="$(printf '%s\n' "${task_lines[@]}" | sort -r | cut -f2- | head -n "$per_group")"
    [[ -n "$body" ]] && body+=$'\n\n'
    body+="### Probably a task ($nt)"$'\n'"$sorted"
    more=$((nt - per_group)); (( more > 0 )) && body+=$'\n'"…$more more in this group; run the sweep with --sweep --now for the full list"
  fi
  local footer="---"$'\n'"waiting on you $nw · probably a task $nt · snoozed $snoozed"$'\n'
  footer+="Snooze: add \"<threadId>\": \"<YYYY-MM-DD or ISO datetime>\" to the JSON object in $snooze_file (hidden until then; a bare date means local midnight)."
  if [[ -n "$body" ]]; then body="$(printf '%s' "$body" | cap_lines "$SWEEP_BODY_CAP" "$footer")"; else body="$footer"; fi

  # Items: every listed thread, same order as the body, not capped per group.
  local items_file keys
  items_file="$tmp/.items"
  { (( nw )) && printf '%s\n' "${wait_items[@]}" | sort -r | cut -f2-
    (( nt )) && printf '%s\n' "${task_items[@]}" | sort -r | cut -f2-; } > "$items_file"
  keys="$(IFS=,; printf '%s' "${open_keys[*]}")"
  (( ${#keys} > 61440 )) && log "sweep: refs.openKeys is ${#keys} bytes (over 61440); emitting it anyway"

  jq -c --slurp --arg id "$(sweep_id "$ME")" --arg body "$body" --arg me "$ME" \
    --arg n "$total" --arg snooze "$snooze_file" --arg observed "$(date '+%Y-%m-%dT%H:%M:%S%:z')" \
    --arg hint "${GMAIL_WORKSPACE_HINT:-}" --arg scope "$(thread_key "$ME" "")" --arg keys "$keys" '
    {id: $id, kind: "sweep",
     title: (if $n == "0" then "Inbox sweep: nothing waiting on you"
             else "Inbox sweep: " + $n + " thread" + (if $n == "1" then "" else "s" end) + " waiting on you" end),
     body: $body, observedAt: $observed,
     refs: {url: "https://mail.google.com/mail/u/0/#inbox", account: $me, snoozeFile: $snooze, count: $n,
            scope: $scope, openKeys: $keys},
     items: .}
    + (if $hint == "" then {} else {hints: {workspace: $hint}} end)' "$items_file"
}

if (( SWEEP )); then run_sweep; exit 0; fi

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
    --arg observed "$observed" --arg thread "$thread" --arg mid "$mid" --arg fromAddr "$from_addr" \
    --arg key "$(thread_key "$ME" "$thread")" '
    {
      id: $id,
      kind: "email",
      title: ((if $subject == "" then "(no subject)" else $subject end) + " — " + $who),
      body: ("From: " + $from + "\nTo: " + $to + "\nDate: " + $date + "\n\n" + $text),
      observedAt: $observed,
      refs: {
        url: ("https://mail.google.com/mail/u/0/#inbox/" + $thread),
        from: $fromAddr, threadId: $thread, messageId: $mid, key: $key
      }
    }'
  emitted=$((emitted+1))
done

log "history $START..$NEW_HID: ${#RECORDS[@]} added, $handled examined, $emitted signalled, $skipped_bulk bulk, $skipped_self from self, $gone gone"
emit_watermark "$WATERMARK"
