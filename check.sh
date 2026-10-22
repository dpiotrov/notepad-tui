#!/usr/bin/env bash
# Quality gate (spec.md §10). A stage is not done until this exits 0.
set -uo pipefail
cd "$(dirname "$0")"

fail=0
run() {
  local name="$1"; shift
  printf '\n\036\036\036=== %s ===\n' "$name"
  if "$@"; then
    printf 'PASS: %s\n' "$name"
  else
    printf 'FAIL: %s\n' "$name"
    fail=1
  fi
}

run "fmt"        cargo fmt --all -- --check
run "clippy"     cargo clippy --workspace --all-targets -- -D warnings
run "test"       cargo test --workspace
run "release"    cargo build --release

# np-core purity: the engine must never depend on the terminal or clipboard stack.
# Uses `cargo metadata` rather than `cargo tree`: tree resolves a package in workspace
# context, so a dependency shared with np-tui can collapse out of `tree -e normal` and
# the violation goes unseen (verified). metadata reports declared kinds deterministically.
printf '\n\036\036\036=== np-core purity ===\n'
# Written to a temp file rather than piped: `cargo metadata | python3 - <<EOF` would
# hand python the heredoc as stdin and silently lose the JSON.
purity_script=$(mktemp)
trap 'rm -f "$purity_script"' EXIT
cat >"$purity_script" <<'PYEOF'
import json, sys, collections

FORBIDDEN = {"ratatui", "ratatui-core", "ratatui-widgets", "ratatui-crossterm",
             "crossterm", "arboard", "anyhow"}

try:
    with open(sys.argv[1]) as fh:
        meta = json.load(fh)
except Exception as exc:
    print("metadata-unreadable: %s" % exc)
    sys.exit(0)

by_id = {p["id"]: p for p in meta["packages"]}

# Map resolve-node id -> package name via packages[] (authoritative; ids are not
# reliably parseable, e.g. "path+file:///abs/np-core#0.1.0").
name_by_id = {p["id"]: p["name"] for p in meta["packages"]}
core_ids = [n["id"] for n in meta["resolve"]["nodes"]
            if name_by_id.get(n["id"]) == "np-core"]
if not core_ids:
    print("np-core-resolve-node-not-found")
    sys.exit(0)

# Non-dev edges only: np-core may use these crates in its own tests, but not for real.
adj = collections.defaultdict(list)
for n in meta["resolve"]["nodes"]:
    for dep in n["deps"]:
        kinds = dep.get("dep_kinds") or [{}]
        if all(k.get("kind") == "dev" for k in kinds):
            continue
        adj[n["id"]].append(dep["pkg"])

seen, queue, hits = set(), collections.deque(core_ids), []
while queue:
    cur = queue.popleft()
    for nxt in adj.get(cur, []):
        if nxt in seen:
            continue
        seen.add(nxt)
        pkg = by_id.get(nxt)
        if pkg and pkg["name"] in FORBIDDEN:
            hits.append(pkg["name"])
        queue.append(nxt)

for h in sorted(set(hits)):
    print(h)
PYEOF

meta_json=$(mktemp)
trap 'rm -f "$purity_script" "$meta_json"' EXIT
if ! cargo metadata --format-version 1 >"$meta_json" 2>/dev/null; then
  echo "FAIL: cargo metadata could not run"
  fail=1
else
  bad=$(python3 "$purity_script" "$meta_json")
  case "$bad" in
    "" ) echo "PASS: np-core free of ratatui/crossterm/arboard/anyhow (direct + transitive)" ;;
    * )  echo "FAIL: np-core depends (directly or transitively, ignoring dev-deps) on UI crates:"
         echo "$bad" | sed 's/^/  /'
         fail=1 ;;
  esac
fi

printf '\n'
if [ "$fail" -eq 0 ]; then
  echo "ALL GATES PASS"
else
  echo "GATE FAILED — fix before starting the next stage"
fi
exit "$fail"
