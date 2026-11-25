#!/bin/sh
# tests/parity/mf.sh <dir>: writes inputs of several frames, mf-<name>.zst, made of the tiles
# (tests/data/tiles) one after the other, as a czi file or a stream of tiles holds many frames: the
# bench decodes them, so the threaded path of decode_frames is timed and
# compared with Go on real data. Run from the repo root.
set -eu
t=tests/data/tiles
cat $t/gray16_1024_blobs_z1_hilo.zst $t/gray16_1024_gradient_z3_hilo.zst $t/gray16_1024_blobs_z3_hilo.zst $t/gray16_1024_gradient_z1_hilo.zst >"$1/mf-gray16-hilo-4.zst"
cat $t/bgr24_1024_blobs_z1.zst $t/bgr24_1024_gradient_z3.zst >"$1/mf-bgr24-2.zst"
cat $t/bgr24_1024_blobs_z3.zst $t/bgr24_1024_gradient_z1.zst $t/bgr24_1024_blobs_z1.zst $t/bgr24_1024_gradient_z3.zst \
  $t/bgr24_512_blobs_z1.zst $t/bgr24_512_gradient_z3.zst $t/bgr24_512_blobs_z3.zst $t/bgr24_512_gradient_z1.zst >"$1/mf-bgr24-8.zst"
cat $t/gray16_512_blobs_z1.zst $t/gray16_512_gradient_z3.zst $t/gray16_512_blobs_z3.zst >"$1/mf-gray16-3.zst"
