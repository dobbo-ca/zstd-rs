//go:build !purego

#include "textflag.h"

// func interleaveAsm(dst, lo, hi *byte, n int)
TEXT ·interleaveAsm(SB), NOSPLIT, $0-32
	MOVQ dst+0(FP), DI
	MOVQ lo+8(FP), SI
	MOVQ hi+16(FP), DX
	MOVQ n+24(FP), CX

loop:
	MOVOU     (SI), X0
	MOVOU     (DX), X1
	MOVOU     X0, X2
	PUNPCKLBW X1, X0
	PUNPCKHBW X1, X2
	MOVOU     X0, (DI)
	MOVOU     X2, 16(DI)
	ADDQ      $16, SI
	ADDQ      $16, DX
	ADDQ      $32, DI
	DECQ      CX
	JNZ       loop
	RET
