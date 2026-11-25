//go:build (!amd64 && !arm64) || purego

package czi

func interleave(dst, lo, hi []byte) { interleaveGo(dst, lo, hi) }
