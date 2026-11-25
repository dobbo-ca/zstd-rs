// Package simd holds the switch that forces the Go kernels.
package simd

import "os"

// Off is set by ZSTD_NOSIMD=1.
var Off = os.Getenv("ZSTD_NOSIMD") == "1"
