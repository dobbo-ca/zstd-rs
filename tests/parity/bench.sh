#!/bin/sh
# Decode time of the tiles and of the Huffman-heavy frames in $BENCH_HUF (default /bench/huf, which
# the bench image makes; skipped if absent), Go and Rust on the same inputs, as a markdown table:
# tests/parity/bench.sh [N [K [N1]]]: K rounds (default 3) of Go then Rust, the minimum ns per tile of
# each, at the default parallelism (N decodes per tile) and again single-threaded (GOMAXPROCS=1,
# BENCH_MAX_THREADS=1, N1 decodes per tile, default N / 2).
# Needs `oracle` (Go) and `bench` (Rust) on PATH, or the programs named by ORACLE and BENCH (the v4
# builds: ORACLE=oracle-v4 BENCH=bench-v4); BENCH_ST=0 skips the single-threaded pass. Run from the
# repo root.
set -eu
export LC_ALL=C
n=${1:-100}
k=${2:-3}
n1=${3:-$((n / 2))}
dirs="tests/data/tiles ${BENCH_HUF:-/bench/huf}"
# K rounds of N decodes per tile, Go then Rust.
rounds() {
  rm -f /tmp/go-r*.tsv /tmp/rs-r*.tsv
  for i in $(seq "$k"); do
    : >/tmp/go-r$i.tsv
    : >/tmp/rs-r$i.tsv
    for d in $dirs; do
      [ -d "$d" ] || continue
      "${ORACLE:-oracle}" -bench "$1" "$d" >>/tmp/go-r$i.tsv
      "${BENCH:-bench}" "$d" "$1" >>/tmp/rs-r$i.tsv
    done
  done
}
# name, bytes, ns: the fastest round of each name.
fastest() { cat "$@" | awk -F'\t' '!($1 in m) || $3 + 0 < m[$1] + 0 { m[$1] = $3; b[$1] = $2 } END { for (x in m) printf "%s\t%s\t%s\n", x, b[x], m[x] }' | sort; }
# name, Go out bytes, Go ns, Rust out bytes, Rust ns. ns per byte is ms per MB. Classes: format
# and content, the hi/lo planar tiles, the same hi/lo tiles through the czi decoder, and the
# Huffman-heavy frames by decoder (not in `all`, which stays the tiles).
table() { join -t "$(printf '\t')" /tmp/go.tsv "$1" | awk -F'\t' '
function add(k) { b[k] += $2; g[k] += $3; r[k] += $5; c[k]++ }
{
  if ($2 != $4) { printf "%s: Go decoded %s bytes, Rust %s\n", $1, $2, $4 > "/dev/stderr"; bad = 1 }
  k = "other"
  if ($1 ~ /^mf-/) k = "multi-frame"
  else if ($1 ~ /^huf_x1_/) k = "huf-x1"
  else if ($1 ~ /^huf_x2_/) k = "huf-x2"
  else if ($1 ~ /^czi-/) k = "czi-hilo"
  else if ($1 ~ /_hilo$/) k = "gray16-hilo"
  else if (match($1, /^(gray16|bgr24)_[0-9]+_(noise|blobs|gradient)/)) {
    split($1, p, "_"); k = p[1] "-" p[3]
  }
  add(k); if (k !~ /^huf-/ && k != "multi-frame") add("all")
}
END {
  if (bad) exit 1
  print "| input | files | MB out | Go ms/MB | Rust ms/MB | Rust / Go |"
  print "|---|---|---|---|---|---|"
  m = split("bgr24-noise bgr24-blobs bgr24-gradient gray16-noise gray16-blobs gray16-gradient gray16-hilo czi-hilo other all huf-x1 huf-x2 multi-frame", ks, " ")
  for (i = 1; i <= m; i++) {
    k = ks[i]
    if (b[k]) printf "| %s | %d | %.2f | %.3f | %.3f | %.2f |\n", k, c[k], b[k] / 1e6, g[k] / b[k], r[k] / b[k], r[k] / g[k]
  }
}' ; }
# Both tables of one setting.
report() {
  rounds "$1"
  fastest /tmp/go-r*.tsv >/tmp/go.tsv
  fastest /tmp/rs-r*.tsv >/tmp/rs-all.tsv
  grep -v '^vec-' /tmp/rs-all.tsv | sort >/tmp/rs.tsv
  # The same tiles through decode_all_into (a reused Vec), compared with Go's DecodeAll(dst[:0]).
  grep '^vec-' /tmp/rs-all.tsv | sed 's/^vec-//' | sort >/tmp/rsvec.tsv
  table /tmp/rs.tsv
  echo
  echo 'Rust through decode_all_into (a cleared, reused Vec: zero-filled on each call), no czi:'
  echo
  table /tmp/rsvec.tsv
}
echo "Go at its default GOMAXPROCS, Rust at the default max_threads ($n decodes per tile, best of $k):"
echo
report "$n"
[ "${BENCH_ST:-1}" = 0 ] && exit 0
echo
echo "Single-threaded: GOMAXPROCS=1, max_threads = 1 ($n1 decodes per tile, best of $k):"
echo
export GOMAXPROCS=1 BENCH_MAX_THREADS=1
report "$n1"
