//go:build (amd64 || arm64) && !purego && !kernelcheck

package zstd

// kernelCheck is true under -tags kernelcheck; see kernels_check.go.
const kernelCheck = false

func seqCheck(*seqState) int                                 { panic("kernelcheck") }
func huf4X1Check(*hufArgs, []byte, []byte, *[1 << 12]uint16) { panic("kernelcheck") }
func huf4X2Check(*hufArgs, []byte, []byte, *[1 << 12]uint32) { panic("kernelcheck") }
