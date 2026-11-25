#!/bin/sh
# Instruction profile (callgrind) of the Rust bench and the Go oracle on a few tiles per class:
# tests/parity/profile.sh <out-dir>. Needs valgrind, `oracle` and a debug-info `bench` on PATH.
set -u
out=$1
mkdir -p "$out"
# The disassembly of the hot kernels, for comparison with Go's asm.
objdump -d -C --no-show-raw-insn "$(command -v bench)" |
  awk '/^[0-9a-f]+ <zstd::(seqloop::seq_loop_bmi2|huf::huf4x[12]_loop[^>]*|hufloop::x[12]_bmi2)>:$/,/^$/' >"$out/rs-disasm.txt"
for class in gray16_1024_gradient_z3_hilo bgr24_1024_gradient_z3 bgr24_1024_blobs_z3 gray16_512_blobs_z3_hilo; do
  d=/tmp/prof-$class
  mkdir -p $d && cp tests/data/tiles/$class.zst $d/
  valgrind --tool=callgrind --dump-instr=yes --callgrind-out-file=$out/rs-$class.cg bench $d 20 >/dev/null 2>$out/rs-$class-log.txt
  GODEBUG=asyncpreemptoff=1 valgrind --tool=callgrind --dump-instr=yes --callgrind-out-file=$out/go-$class.cg oracle -bench 20 $d >/dev/null 2>$out/go-$class.txt
  echo "#### $class"
  echo '```'
  echo "Rust total: $(grep -m1 '^summary\|^totals' $out/rs-$class.cg)"
  echo "Go total:   $(grep -m1 '^summary\|^totals' $out/go-$class.cg)"
  echo '--- Rust top functions'
  callgrind_annotate --inclusive=no $out/rs-$class.cg 2>/dev/null | sed -n '/^Ir/,$p' | head -25
  echo '--- Go top functions'
  callgrind_annotate --inclusive=no $out/go-$class.cg 2>/dev/null | sed -n '/^Ir/,$p' | head -25
  echo '```'
  callgrind_annotate --auto=yes --threshold=99 $out/rs-$class.cg >$out/rs-$class-annotated.txt 2>&1
  callgrind_annotate --auto=yes --threshold=99 $out/go-$class.cg >$out/go-$class-annotated.txt 2>&1
done
