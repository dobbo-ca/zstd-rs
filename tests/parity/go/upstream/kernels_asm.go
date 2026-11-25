//go:build (amd64 || arm64) && !purego

package zstd

import "github.com/dobbo-ca/zstd-go/internal/simd"

// The asm kernels read the Go structs through go_asm.h offsets and do
// no bounds checks; the Go callers guarantee the same limits as the Go
// kernels.

//go:noescape
func seqLoopAsm(s *seqState) int

//go:noescape
func huf4X1LoopAsm(a *hufArgs, src, dst *byte, dt *[1 << 12]uint16)

//go:noescape
func huf4X2LoopAsm(a *hufArgs, src, dst *byte, dt *[1 << 12]uint32)

// asmOn selects the asm kernels; tests clear it to compare.
var asmOn = !simd.Off && cpuOK()

func seqLoop(s *seqState) int {
	switch {
	case kernelCheck:
		return seqCheck(s)
	case asmOn:
		return seqLoopAsm(s)
	}
	return seqLoopGo(s)
}

func huf4X1Loop(a *hufArgs, src, dst []byte, dt *[1 << 12]uint16) {
	switch {
	case kernelCheck:
		huf4X1Check(a, src, dst, dt)
	case asmOn:
		huf4X1LoopAsm(a, &src[0], &dst[0], dt)
	default:
		huf4X1LoopGo(a, src, dst, dt)
	}
}

func huf4X2Loop(a *hufArgs, src, dst []byte, dt *[1 << 12]uint32) {
	switch {
	case kernelCheck:
		huf4X2Check(a, src, dst, dt)
	case asmOn:
		huf4X2LoopAsm(a, &src[0], &dst[0], dt)
	default:
		huf4X2LoopGo(a, src, dst, dt)
	}
}
