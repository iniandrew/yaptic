#!/usr/bin/env bash
# Regenerates the Rust crate table in THIRD_PARTY_NOTICES.md from Cargo.lock:
# the crates linked into the macOS app (runtime dependencies, no proc-macros).
# Run from anywhere after changing dependencies.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
notices="$root/THIRD_PARTY_NOTICES.md"
table="$(mktemp)"
trap 'rm -f "$table"' EXIT

{
  echo "| Crate | Version | License |"
  echo "|---|---|---|"
  (cd "$root/src-tauri" &&
    cargo tree -e normal --target aarch64-apple-darwin --prefix none --format '{p}|{l}') |
    grep -v '(proc-macro)' |
    sed -e 's/ (\*)$//' |
    grep -v '^yaptic ' |
    sort -u |
    awk -F'|' '{
      split($1, id, " ")
      version = id[2]; sub(/^v/, "", version)
      license = ($2 == "") ? "see crate" : $2
      printf "| %s | %s | %s |\n", id[1], version, license
    }'
} > "$table"

awk -v table="$table" '
  /<!-- crates:start -->/ {
    print
    while ((getline line < table) > 0) print line
    skip = 1
    next
  }
  /<!-- crates:end -->/ { skip = 0 }
  !skip { print }
' "$notices" > "$notices.tmp"
mv "$notices.tmp" "$notices"

echo "Updated $(($(wc -l < "$table") - 2)) crates in THIRD_PARTY_NOTICES.md"
