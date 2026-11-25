//go:build !purego

#include "textflag.h"

// func interleaveAsm(dst, lo, hi *byte, n int)
TEXT ·interleaveAsm(SB), NOSPLIT, $0-32
	MOVD dst+0(FP), R0
	MOVD lo+8(FP), R1
	MOVD hi+16(FP), R2
	MOVD n+24(FP), R3

loop:
	VLD1.P 16(R1), [V0.B16]
	VLD1.P 16(R2), [V1.B16]
	VST2.P [V0.B16, V1.B16], 32(R0)
	SUBS   $1, R3, R3
	BNE    loop
	RET
