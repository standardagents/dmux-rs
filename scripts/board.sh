#!/bin/bash
# Move a dmux-rs issue's card on the org Project board.
#
#   scripts/board.sh <issue-number> <Todo|In Progress|Done>
#
# Loop etiquette: claimed issues move to "In Progress" when work starts
# (closing an issue moves it to Done via the board's built-in workflow).
# Requires the gh token to carry the `project` scope:
#   gh auth refresh -s project
# Degrades to a no-op (exit 0, message on stderr) when the scope is
# missing so an unattended loop never blocks on it.
set -euo pipefail
OWNER=standardagents
REPO=dmux-rs
ISSUE=${1:?usage: board.sh <issue-number> <status>}
STATUS=${2:?usage: board.sh <issue-number> <status>}
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

if ! gh project list --owner "$OWNER" --format json >"$TMP/projects.json" 2>/dev/null; then
  echo "board.sh: gh token lacks project scope (gh auth refresh -s project); skipping" >&2
  exit 0
fi

PROJ=$(python3 -c "
import json
d = json.load(open('$TMP/projects.json'))
p = (d.get('projects') or [])[0]
print(p['number'], p['id'])
")
PROJ_NUM=${PROJ% *}; PROJ_ID=${PROJ#* }

gh project item-list "$PROJ_NUM" --owner "$OWNER" --format json --limit 200 >"$TMP/items.json"
ITEM_ID=$(python3 -c "
import json
items = json.load(open('$TMP/items.json'))['items']
for it in items:
    c = it.get('content') or {}
    if c.get('repository','').endswith('$REPO') and c.get('number') == $ISSUE:
        print(it['id']); break
")
if [ -z "$ITEM_ID" ]; then
  echo "board.sh: issue #$ISSUE not found on the project; skipping" >&2
  exit 0
fi

gh project field-list "$PROJ_NUM" --owner "$OWNER" --format json >"$TMP/fields.json"
FIELD=$(python3 -c "
import json, sys
fields = json.load(open('$TMP/fields.json'))['fields']
for f in fields:
    if f['name'] == 'Status':
        for o in f.get('options', []):
            if o['name'].lower() == '$STATUS'.lower():
                print(f['id'], o['id']); sys.exit(0)
sys.exit(1)
")
FIELD_ID=${FIELD% *}; OPTION_ID=${FIELD#* }

gh project item-edit --project-id "$PROJ_ID" --id "$ITEM_ID" \
  --field-id "$FIELD_ID" --single-select-option-id "$OPTION_ID" >/dev/null
echo "board.sh: issue #$ISSUE → $STATUS"
