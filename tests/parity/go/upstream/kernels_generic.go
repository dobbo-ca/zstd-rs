//go:build (!amd64 && !arm64) || purego

package zstd

const asmOn = false

func seqLoop(s *seqState) int { return seqLoopGo(s) }

func huf4X1Loop(a *hufArgs, src, dst []byte, dt *[1 << 12]uint16) { huf4X1LoopGo(a, src, dst, dt) }

func huf4X2Loop(a *hufArgs, src, dst []byte, dt *[1 << 12]uint32) { huf4X2LoopGo(a, src, dst, dt) }
