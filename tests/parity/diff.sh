#!/bin/sh
# diff -rq of the Go and Rust output trees (one <input>.res per input); on a difference, the first
# differing files, each with the record headers ("<op> <kind> <len>") that differ and the first
# differing byte: tests/parity/diff.sh <go-out> <rust-out>
set -u
if diff -rq "$1" "$2" >/tmp/diff.txt; then exit 0; fi
wc -l </tmp/diff.txt
head -20 /tmp/diff.txt
echo "first differences:"
sed -n -E 's/^Files (.*) and .* differ$/\1/p' /tmp/diff.txt | head -12 | while read -r g; do
  f=${g#$1/}
  echo "--- $f"
  grep -a -E '^[a-z]+ (out|err|partial) [0-9]+$' "$1/$f" >/tmp/go.hdr
  grep -a -E '^[a-z]+ (out|err|partial) [0-9]+$' "$2/$f" >/tmp/rust.hdr
  diff /tmp/go.hdr /tmp/rust.hdr | head -6
  cmp "$1/$f" "$2/$f" | head -1
done
exit 1
