//go:build !purego

#include "textflag.h"
#include "go_asm.h"

// Ports huf_decompress_amd64.S. ip/op are pointers inside the loop.
// op0-3 SI BX CX DI (AH stores need non-REX bases), ip0-3 R8-R11,
// bits0-3 BP DX R12 R13, dt R15, AX scratch.

// Loads hufArgs into registers; clobbers AX.
#define LOADARGS \
	MOVQ a+0(FP), AX; \
	MOVQ src+8(FP), DX; \
	MOVQ hufArgs_ip+0(AX), R8; \
	ADDQ DX, R8; \
	MOVQ hufArgs_ip+8(AX), R9; \
	ADDQ DX, R9; \
	MOVQ hufArgs_ip+16(AX), R10; \
	ADDQ DX, R10; \
	MOVQ hufArgs_ip+24(AX), R11; \
	ADDQ DX, R11; \
	MOVQ dst+16(FP), DX; \
	MOVQ hufArgs_op+0(AX), SI; \
	ADDQ DX, SI; \
	MOVQ hufArgs_op+8(AX), BX; \
	ADDQ DX, BX; \
	MOVQ hufArgs_op+16(AX), CX; \
	ADDQ DX, CX; \
	MOVQ hufArgs_op+24(AX), DI; \
	ADDQ DX, DI; \
	MOVQ hufArgs_bits+0(AX), BP; \
	MOVQ hufArgs_bits+8(AX), DX; \
	MOVQ hufArgs_bits+16(AX), R12; \
	MOVQ hufArgs_bits+24(AX), R13; \
	MOVQ dt+24(FP), R15

#define STOREARGS \
	MOVQ a+0(FP), AX; \
	MOVQ BP, hufArgs_bits+0(AX); \
	MOVQ DX, hufArgs_bits+8(AX); \
	MOVQ R12, hufArgs_bits+16(AX); \
	MOVQ R13, hufArgs_bits+24(AX); \
	MOVQ src+8(FP), DX; \
	SUBQ DX, R8; \
	MOVQ R8, hufArgs_ip+0(AX); \
	SUBQ DX, R9; \
	MOVQ R9, hufArgs_ip+8(AX); \
	SUBQ DX, R10; \
	MOVQ R10, hufArgs_ip+16(AX); \
	SUBQ DX, R11; \
	MOVQ R11, hufArgs_ip+24(AX); \
	MOVQ dst+16(FP), DX; \
	SUBQ DX, SI; \
	MOVQ SI, hufArgs_op+0(AX); \
	SUBQ DX, BX; \
	MOVQ BX, hufArgs_op+8(AX); \
	SUBQ DX, CX; \
	MOVQ CX, hufArgs_op+16(AX); \
	SUBQ DX, DI; \
	MOVQ DI, hufArgs_op+24(AX)

// Exits unless ip0 <= ip1 <= ip2 <= ip3.
#define CHECKIP(exit) \
	CMPQ R9, R8; \
	JCS  exit; \
	CMPQ R10, R9; \
	JCS  exit; \
	CMPQ R11, R10; \
	JCS  exit

// R15 = (ip0-src)/7; clobbers AX, DX.
#define IPDIV7 \
	MOVQ R8, AX; \
	SUBQ src+8(FP), AX; \
	MOVQ AX, R15; \
	MOVQ $2635249153387078803, DX; \
	MULQ DX; \
	SUBQ DX, R15; \
	SHRQ $1, R15; \
	ADDQ DX, R15; \
	SHRQ $2, R15

// ctz of the spent bits gives bytes and bits consumed.
#define RELOAD(ip, bits) \
	TZCNTQ bits, bits; \
	MOVQ   bits, AX; \
	ANDQ   $7, AX; \
	SHRQ   $3, bits; \
	SUBQ   bits, ip; \
	MOVQ   (ip), bits; \
	ORQ    $1, bits; \
	SHLXQ  AX, bits, bits

#define X1SYM(op, bits, k) \
	MOVQ    bits, AX; \
	SHRQ    $53, AX; \
	MOVWLZX (R15)(AX*2), AX; \
	SHLXQ   AX, bits, bits; \
	MOVB    AH, k(op)

#define X1ROUND(k) \
	X1SYM(SI, BP, k); \
	X1SYM(BX, DX, k); \
	X1SYM(CX, R12, k); \
	X1SYM(DI, R13, k)

// Frame: olimit 0, oend 8, spill 16.
// func huf4X1LoopAsm(a *hufArgs, src, dst *byte, dt *[4096]uint16)
TEXT ·huf4X1LoopAsm(SB), NOSPLIT, $24-32
	LOADARGS
	MOVQ a+0(FP), AX
	MOVQ hufArgs_oend(AX), AX
	ADDQ dst+16(FP), AX
	MOVQ AX, 8(SP)

x1olimit:
	MOVQ DX, 16(SP)
	IPDIV7
	MOVQ 8(SP), AX
	SUBQ DI, AX
	MOVQ $-3689348814741910323, DX
	MULQ DX
	SHRQ $2, DX
	CMPQ R15, DX
	CMOVQHI DX, R15
	LEAQ (R15)(R15*4), R15
	ADDQ DI, R15
	MOVQ R15, 0(SP)
	MOVQ 16(SP), DX
	MOVQ dt+24(FP), R15
	CMPQ DI, 0(SP)
	JEQ  x1exit
	CHECKIP(x1exit)

	PCALIGN $64
x1loop:
	X1ROUND(0)
	X1ROUND(1)
	X1ROUND(2)
	X1ROUND(3)
	X1SYM(SI, BP, 4)
	RELOAD(R8, BP)
	X1SYM(BX, DX, 4)
	RELOAD(R9, DX)
	X1SYM(CX, R12, 4)
	RELOAD(R10, R12)
	X1SYM(DI, R13, 4)
	RELOAD(R11, R13)
	ADDQ $5, SI
	ADDQ $5, BX
	ADDQ $5, CX
	ADDQ $5, DI
	CMPQ DI, 0(SP)
	JCS  x1loop
	JMP  x1olimit

x1exit:
	STOREARGS
	RET

// Entry is c: two bytes, then nbBits, then length.
#define X2SYM(op, bits) \
	MOVQ    bits, AX; \
	SHRQ    $53, AX; \
	MOVL    (R15)(AX*4), AX; \
	MOVW    AX, (op); \
	SHRL    $16, AX; \
	SHLXQ   AX, bits, bits; \
	MOVBLZX AH, AX; \
	ADDQ    AX, op

#define X2ROUND \
	X2SYM(SI, BP); \
	X2SYM(BX, DX); \
	X2SYM(CX, R12); \
	X2SYM(DI, R13)

// Frame: olimit 0, oend0-3 8-32, spill 40.
// func huf4X2LoopAsm(a *hufArgs, src, dst *byte, dt *[4096]uint32)
TEXT ·huf4X2LoopAsm(SB), NOSPLIT, $48-32
	LOADARGS
	MOVQ BX, 8(SP)
	MOVQ CX, 16(SP)
	MOVQ DI, 24(SP)
	MOVQ a+0(FP), AX
	MOVQ hufArgs_oend(AX), AX
	ADDQ dst+16(FP), AX
	MOVQ AX, 32(SP)

x2olimit:
	MOVQ DX, 40(SP)
	IPDIV7
	MOVQ 8(SP), AX
	SUBQ SI, AX
	MOVQ 16(SP), DX
	SUBQ BX, DX
	CMPQ DX, AX
	CMOVQHI AX, DX
	MOVQ 24(SP), AX
	SUBQ CX, AX
	CMPQ DX, AX
	CMOVQHI AX, DX
	MOVQ 32(SP), AX
	SUBQ DI, AX
	CMPQ DX, AX
	CMOVQHI AX, DX
	MOVQ $-3689348814741910323, AX
	MULQ DX
	SHRQ $3, DX
	CMPQ R15, DX
	CMOVQHI DX, R15
	LEAQ (R15)(R15*4), R15
	ADDQ DI, R15
	MOVQ R15, 0(SP)
	MOVQ 40(SP), DX
	MOVQ dt+24(FP), R15
	CMPQ DI, 0(SP)
	JEQ  x2exit
	CHECKIP(x2exit)

	PCALIGN $64
x2loop:
	X2ROUND
	X2ROUND
	X2ROUND
	X2ROUND
	X2ROUND
	RELOAD(R8, BP)
	RELOAD(R9, DX)
	RELOAD(R10, R12)
	RELOAD(R11, R13)
	CMPQ DI, 0(SP)
	JCS  x2loop
	JMP  x2olimit

x2exit:
	STOREARGS
	RET
