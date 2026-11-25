//go:build !purego

#include "textflag.h"
#include "go_asm.h"

// Ports of huf4X1LoopGo and huf4X2LoopGo. Registers:
// R1 src, R2 dst, R3 dt, R4-R7 ip, R8-R11 op (pointers),
// R12-R15 bits, R16 dst+oend, R17 olimit; R0, R19-R25 scratch.

// X1SYM decodes one symbol of a stream into byte k of its output.
#define X1SYM(b, op, k, t, c) \
	LSR	$53, b, t; \
	MOVHU	(R3)(t<<1), c; \
	LSL	c, b, b; \
	LSR	$8, c, t; \
	MOVB	t, k(op)

// X2SYM decodes one table cell of a stream.
#define X2SYM(b, op, t, c) \
	LSR	$53, b, t; \
	MOVWU	(R3)(t<<2), c; \
	MOVH	c, (op); \
	LSR	$16, c, t; \
	LSL	t, b, b; \
	ADD	c>>24, op, op

// RELOAD refills a stream; ctz 64 would give ip-8 and shift 0.
#define RELOAD(b, ip, t, c) \
	RBIT	b, t; \
	CLZ	t, t; \
	SUB	t>>3, ip, ip; \
	MOVD	(ip), c; \
	ORR	$1, c, c; \
	AND	$7, t, t; \
	LSL	t, c, b

#define LOADARGS \
	MOVD	a+0(FP), R0; \
	MOVD	src+8(FP), R1; \
	MOVD	dst+16(FP), R2; \
	MOVD	dt+24(FP), R3; \
	MOVD	hufArgs_ip+0(R0), R4; \
	MOVD	hufArgs_ip+8(R0), R5; \
	MOVD	hufArgs_ip+16(R0), R6; \
	MOVD	hufArgs_ip+24(R0), R7; \
	ADD	R1, R4; \
	ADD	R1, R5; \
	ADD	R1, R6; \
	ADD	R1, R7; \
	MOVD	hufArgs_op+0(R0), R8; \
	MOVD	hufArgs_op+8(R0), R9; \
	MOVD	hufArgs_op+16(R0), R10; \
	MOVD	hufArgs_op+24(R0), R11; \
	ADD	R2, R8; \
	ADD	R2, R9; \
	ADD	R2, R10; \
	ADD	R2, R11; \
	MOVD	hufArgs_bits+0(R0), R12; \
	MOVD	hufArgs_bits+8(R0), R13; \
	MOVD	hufArgs_bits+16(R0), R14; \
	MOVD	hufArgs_bits+24(R0), R15; \
	MOVD	hufArgs_oend(R0), R16; \
	ADD	R2, R16

#define STOREARGS \
	MOVD	a+0(FP), R0; \
	SUB	R1, R4; \
	SUB	R1, R5; \
	SUB	R1, R6; \
	SUB	R1, R7; \
	MOVD	R4, hufArgs_ip+0(R0); \
	MOVD	R5, hufArgs_ip+8(R0); \
	MOVD	R6, hufArgs_ip+16(R0); \
	MOVD	R7, hufArgs_ip+24(R0); \
	SUB	R2, R8; \
	SUB	R2, R9; \
	SUB	R2, R10; \
	SUB	R2, R11; \
	MOVD	R8, hufArgs_op+0(R0); \
	MOVD	R9, hufArgs_op+8(R0); \
	MOVD	R10, hufArgs_op+16(R0); \
	MOVD	R11, hufArgs_op+24(R0); \
	MOVD	R12, hufArgs_bits+0(R0); \
	MOVD	R13, hufArgs_bits+8(R0); \
	MOVD	R14, hufArgs_bits+16(R0); \
	MOVD	R15, hufArgs_bits+24(R0)

// func huf4X1LoopAsm(a *hufArgs, src, dst *byte, dt *[4096]uint16)
TEXT ·huf4X1LoopAsm(SB), NOSPLIT, $0-32
	LOADARGS

x1outer:
	// iters = min((oend-op[3])/5, ip[0]/7)
	SUB	R11, R16, R19
	MOVD	$5, R20
	SDIV	R20, R19, R19
	SUB	R1, R4, R21
	MOVD	$7, R20
	SDIV	R20, R21, R21
	CMP	R21, R19
	CSEL	LT, R19, R21, R19
	CBZ	R19, x1done
	ADD	R19<<2, R19, R19
	ADD	R11, R19, R17
	CMP	R4, R5
	BLO	x1done
	CMP	R5, R6
	BLO	x1done
	CMP	R6, R7
	BLO	x1done

x1inner:
	X1SYM(R12, R8, 0, R19, R20)
	X1SYM(R13, R9, 0, R21, R22)
	X1SYM(R14, R10, 0, R23, R24)
	X1SYM(R15, R11, 0, R25, R0)
	X1SYM(R12, R8, 1, R19, R20)
	X1SYM(R13, R9, 1, R21, R22)
	X1SYM(R14, R10, 1, R23, R24)
	X1SYM(R15, R11, 1, R25, R0)
	X1SYM(R12, R8, 2, R19, R20)
	X1SYM(R13, R9, 2, R21, R22)
	X1SYM(R14, R10, 2, R23, R24)
	X1SYM(R15, R11, 2, R25, R0)
	X1SYM(R12, R8, 3, R19, R20)
	X1SYM(R13, R9, 3, R21, R22)
	X1SYM(R14, R10, 3, R23, R24)
	X1SYM(R15, R11, 3, R25, R0)
	X1SYM(R12, R8, 4, R19, R20)
	X1SYM(R13, R9, 4, R21, R22)
	X1SYM(R14, R10, 4, R23, R24)
	X1SYM(R15, R11, 4, R25, R0)
	RELOAD(R12, R4, R19, R20)
	RELOAD(R13, R5, R21, R22)
	RELOAD(R14, R6, R23, R24)
	RELOAD(R15, R7, R25, R0)
	ADD	$5, R8
	ADD	$5, R9
	ADD	$5, R10
	ADD	$5, R11
	CMP	R17, R11
	BLO	x1inner
	B	x1outer

x1done:
	STOREARGS
	RET

// func huf4X2LoopAsm(a *hufArgs, src, dst *byte, dt *[4096]uint32)
TEXT ·huf4X2LoopAsm(SB), NOSPLIT, $0-32
	LOADARGS

x2outer:
	// iters = min(ip[0]/7, (oend[s]-op[s])/10); oend[s] = a.op[s+1]
	MOVD	a+0(FP), R0
	SUB	R1, R4, R19
	MOVD	$7, R20
	SDIV	R20, R19, R19
	MOVD	$10, R20
	MOVD	hufArgs_op+8(R0), R21
	ADD	R2, R21
	SUB	R8, R21, R21
	SDIV	R20, R21, R21
	CMP	R21, R19
	CSEL	LT, R19, R21, R19
	MOVD	hufArgs_op+16(R0), R21
	ADD	R2, R21
	SUB	R9, R21, R21
	SDIV	R20, R21, R21
	CMP	R21, R19
	CSEL	LT, R19, R21, R19
	MOVD	hufArgs_op+24(R0), R21
	ADD	R2, R21
	SUB	R10, R21, R21
	SDIV	R20, R21, R21
	CMP	R21, R19
	CSEL	LT, R19, R21, R19
	SUB	R11, R16, R21
	SDIV	R20, R21, R21
	CMP	R21, R19
	CSEL	LT, R19, R21, R19
	CBZ	R19, x2done
	ADD	R19<<2, R19, R19
	ADD	R11, R19, R17
	CMP	R4, R5
	BLO	x2done
	CMP	R5, R6
	BLO	x2done
	CMP	R6, R7
	BLO	x2done

x2inner:
	X2SYM(R12, R8, R19, R20)
	X2SYM(R13, R9, R21, R22)
	X2SYM(R14, R10, R23, R24)
	X2SYM(R15, R11, R25, R0)
	X2SYM(R12, R8, R19, R20)
	X2SYM(R13, R9, R21, R22)
	X2SYM(R14, R10, R23, R24)
	X2SYM(R15, R11, R25, R0)
	X2SYM(R12, R8, R19, R20)
	X2SYM(R13, R9, R21, R22)
	X2SYM(R14, R10, R23, R24)
	X2SYM(R15, R11, R25, R0)
	X2SYM(R12, R8, R19, R20)
	X2SYM(R13, R9, R21, R22)
	X2SYM(R14, R10, R23, R24)
	X2SYM(R15, R11, R25, R0)
	X2SYM(R12, R8, R19, R20)
	X2SYM(R13, R9, R21, R22)
	X2SYM(R14, R10, R23, R24)
	X2SYM(R15, R11, R25, R0)
	RELOAD(R12, R4, R19, R20)
	RELOAD(R13, R5, R21, R22)
	RELOAD(R14, R6, R23, R24)
	RELOAD(R15, R7, R25, R0)
	CMP	R17, R11
	BLO	x2inner
	B	x2outer

x2done:
	STOREARGS
	RET
