#!/usr/bin/env bash
# Prove the section-P gate can BOTH pass on clean code AND fire on a planted
# leak. A gate that only ever passes is indistinguishable from one whose regex
# has rotted — section P was itself dead once. Run by CI and by hand.
set -uo pipefail
cd -- "$(dirname -- "$0")/.." || exit 3
fail() { printf 'audit-selftest: %s\n' "$1" >&2; exit 1; }

# 1. Positive — real src/ must pass the gate.
scripts/audit-bad-patterns.sh --strict --section P --strict-sections P \
  || fail "section P is RED on src/ — a new Debug-over-credential struct needs review (or a real leak)"

# 2. Negative — a planted Debug-over-secret struct must be caught (exit 1).
list=$(mktemp) || fail "mktemp failed"
trap 'rm -f "$list"' EXIT
echo scripts/audit-selftest/leaky_signer.rs > "$list"
scripts/audit-bad-patterns.sh --files "$list" --strict --section P --strict-sections P >/dev/null 2>&1
rc=$?
[ "$rc" -eq 0 ] && fail "gate did NOT fire on the planted leak fixture — section P is broken"
[ "$rc" -ne 1 ] && fail "self-test inconclusive: audit exited $rc on the fixture, expected 1"

echo "audit-selftest: ok — gate passes on src/ and fires on the planted fixture"
