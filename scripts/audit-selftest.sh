#!/usr/bin/env bash
# Prove the section-P gate can BOTH pass on clean code AND fire on every planted
# credential form. A gate that only ever passes is indistinguishable from one
# whose regex rotted — section P was itself dead once, and its compound-name
# (underscore) coverage was added later, so the fixture exercises both bare and
# compound and the count must match, or a regression in either goes unnoticed.
set -uo pipefail
cd -- "$(dirname -- "$0")/.." || exit 3
fail() { printf 'audit-selftest: %s\n' "$1" >&2; exit 1; }

# 1. Positive — real src/ must pass the gate.
scripts/audit-bad-patterns.sh --strict --section P --strict-sections P \
  || fail "section P is RED on src/ — a new Debug-over-credential struct needs review (or a real leak)"

# 2. Negative — every planted form (bare + compound) must fire; count must match.
fixture=scripts/audit-selftest/leaky_signer.rs
[ -f "$fixture" ] || fail "fixture $fixture is missing — cannot prove the gate fires"
want=$(grep -cE '^pub struct ' "$fixture")
case "$want" in ''|*[!0-9]*) fail "could not count fixtures in $fixture";; esac
[ "$want" -ge 2 ] || fail "$fixture must plant at least a bare and a compound form (found $want)"

list=$(mktemp) || fail "mktemp failed"; trap 'rm -f "$list"' EXIT
echo "$fixture" > "$list"
got=$(scripts/audit-bad-patterns.sh --files "$list" --section P 2>/dev/null \
      | grep -oE 'API hygiene +: +[0-9]+ line' | grep -oE '[0-9]+')
case "$got" in ''|*[!0-9]*) got=0;; esac
[ "$got" -eq "$want" ] \
  || fail "gate caught $got of $want planted credential forms — a bare or compound match regressed"

echo "audit-selftest: ok — src/ passes; all $want planted credential forms fire"
