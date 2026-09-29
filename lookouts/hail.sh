#!/usr/bin/env bash
# hail.sh — post NDJSON signals from stdin to a Bosun's webhook lookout.
#
# Feed anything a lookout prints into Bosun by hand, as if it had just arrived:
#
#   ORBIT_TASK_IDS=cmu… ./lookouts/orbit-tasks.py | ./lookouts/hail.sh
#   printf '{"title":"Look at this","body":"…"}\n' | ./lookouts/hail.sh
#
# Env:
#   HAIL_URL      the lookout's URL, e.g. http://127.0.0.1:42800/api/hail/<lookoutId>
#   HAIL_SECRET   that webhook lookout's secret
#   HAIL_DATA_DIR optional; when HAIL_URL/HAIL_SECRET are unset, read them from
#                 <dir>/server.json + <dir>/bosuns.json (first webhook lookout of
#                 the bosun named HAIL_BOSUN, default: the first bosun)
#
# Lines without a "title" (the watermark line, blank lines) are skipped. Each
# accepted signal prints its id; a rejected one prints the HTTP status and body.
set -euo pipefail

if [[ -z "${HAIL_URL:-}" || -z "${HAIL_SECRET:-}" ]]; then
  dir="${HAIL_DATA_DIR:-$HOME/.threadknot}"
  [[ -f "$dir/server.json" ]] || dir="$HOME/.armada"
  eval "$(python3 - "$dir" "${HAIL_BOSUN:-}" <<'PY'
import json, sys, shlex
d, want = sys.argv[1], sys.argv[2]
srv = json.load(open(f"{d}/server.json"))
bos = json.load(open(f"{d}/bosuns.json"))
bos = bos if isinstance(bos, list) else bos.get("bosuns", [])
b = next((x for x in bos if not want or x["name"] == want), None)
if not b: sys.exit("no such bosun")
hook = next((l for l in b["lookouts"] if l["kind"]["type"] == "webhook" and l["enabled"]), None)
if not hook: sys.exit(f"bosun {b['name']} has no enabled webhook lookout")
print(f"HAIL_URL={shlex.quote(f'http://127.0.0.1:{srv['port']}/api/hail/{hook['id']}')}")
print(f"HAIL_SECRET={shlex.quote(hook['kind']['secret'])}")
PY
)"
fi

sent=0
while IFS= read -r line; do
  [[ -z "${line// }" ]] && continue
  echo "$line" | jq -e 'type == "object" and has("title")' >/dev/null 2>&1 || continue
  code=$(curl -sS -o /tmp/hail.$$ -w '%{http_code}' -X POST "$HAIL_URL" \
    -H "Authorization: Bearer $HAIL_SECRET" -H 'Content-Type: application/json' \
    --data-binary "$line")
  if [[ "$code" == "202" ]]; then
    echo "hailed $(jq -r '.signalId' /tmp/hail.$$)"
    sent=$((sent + 1))
  else
    echo "HTTP $code: $(cat /tmp/hail.$$)" >&2
  fi
done
rm -f /tmp/hail.$$
echo "$sent signal(s) hailed" >&2
