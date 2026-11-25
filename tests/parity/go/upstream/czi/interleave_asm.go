//go:build (amd64 || arm64) && !purego

package czi

import "github.com/dobbo-ca/zstd-go/internal/simd"

// interleaveAsm handles n 16-byte chunks of lo and hi.
//
//go:noescape
func interleaveAsm(dst, lo, hi *byte, n int)

func interleave(dst, lo, hi []byte) {
	n := len(lo) &^ 15
	if n == 0 || simd.Off {
		interleaveGo(dst, lo, hi)
		return
	}
	_, _ = dst[2*n-1], hi[n-1] // asm does no bounds checks
	interleaveAsm(&dst[0], &lo[0], &hi[0], n/16)
	interleaveGo(dst[2*n:], lo[n:], hi[n:])
}
