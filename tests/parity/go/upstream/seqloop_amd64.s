//go:build !purego

#include "textflag.h"
#include "go_asm.h"

// Port of seqLoopGo. Register map:
//	AX bits, BX 64-consumed, CX &in[ptr]
//	DX, R8, R9 current ll/ml/of symbols
//	SI &out[op], DI &lits[lp]
//	R10 rep[0], equal to off
// Frame: 0/8/16 llT/mlT/ofT, 24/32 rep[1]/rep[2],
// 40 nb, 48 &lits[litEnd], 56 &out[len-32],
// 64 &out[base], 72 &in[16], 80/88/96 states,
// 104/112 ll/ml.
// BX stays 0-64, so byte subtracts never borrow.

// Go's full reload.
#define RELOAD \
	MOVL  $64, R12; \
	SUBQ  BX, R12; \
	ANDQ  $-8, R12; \
	ADDQ  R12, BX; \
	SHRQ  $3, R12; \
	SUBQ  R12, CX; \
	MOVQ  (CX), AX

// func seqLoopAsm(s *seqState) int
TEXT ·seqLoopAsm(SB), NOSPLIT, $120-16
	MOVQ s+0(FP), R12
	MOVQ seqState_nbSeq(R12), AX
	TESTQ AX, AX
	JLE  none
	MOVQ AX, 40(SP)
	MOVQ seqState_llT(R12), AX
	MOVQ AX, 0(SP)
	MOVQ seqState_mlT(R12), AX
	MOVQ AX, 8(SP)
	MOVQ seqState_ofT(R12), AX
	MOVQ AX, 16(SP)
	MOVQ seqState_rep+8(R12), AX
	MOVQ AX, 24(SP)
	MOVQ seqState_rep+16(R12), AX
	MOVQ AX, 32(SP)
	MOVQ seqState_rep(R12), R10
	MOVQ seqState_lits(R12), AX
	MOVQ seqState_lp(R12), DI
	ADDQ AX, DI
	ADDQ seqState_litEnd(R12), AX
	MOVQ AX, 48(SP)
	MOVQ seqState_out(R12), AX
	MOVQ seqState_op(R12), SI
	ADDQ AX, SI
	MOVQ seqState_base(R12), DX
	ADDQ AX, DX
	MOVQ DX, 64(SP)
	ADDQ seqState_out+8(R12), AX
	SUBQ $const_wildcopySlack, AX
	MOVQ AX, 56(SP)
	MOVQ seqState_br+bitReader_in(R12), AX
	MOVQ seqState_br+bitReader_ptr(R12), CX
	ADDQ AX, CX
	ADDQ $16, AX
	MOVQ AX, 72(SP)
	MOVQ seqState_br+bitReader_bits(R12), AX
	MOVL $64, BX
	SUBQ seqState_br+bitReader_consumed(R12), BX
	MOVQ seqState_llS(R12), DX
	MOVQ DX, 80(SP)
	MOVQ seqState_mlS(R12), R8
	MOVQ R8, 88(SP)
	MOVQ seqState_ofS(R12), R9
	MOVQ R9, 96(SP)
	MOVQ 0(SP), R11
	MOVQ (R11)(DX*8), DX
	MOVQ 8(SP), R11
	MOVQ (R11)(R8*8), R8
	MOVQ 16(SP), R11
	MOVQ (R11)(R9*8), R9
	JMP  next

loop:
	// Offset.
	RORXQ   $16, R9, R11
	CMPB    R11, $1
	JLS     ofsmall
	SUBB    R11, BX
	SHRXQ   BX, AX, R12
	BZHIQ   R11, R12, R12
	MOVQ    24(SP), R13
	MOVQ    R13, 32(SP)
	MOVQ    R10, 24(SP)
	RORXQ   $32, R9, R10
	ADDL    R12, R10 // sums fit 32 bits

ofdone:
	// Match length into BP.
	RORXQ   $16, R8, R11
	SUBB    R11, BX
	SHRXQ   BX, AX, R12
	BZHIQ   R11, R12, R12
	RORXQ   $32, R8, BP
	ADDL    R12, BP
	MOVQ    BP, 112(SP)

	// Byte 2: extra bits+1; byte 3: state bits.
	LEAQ  0x10000(DX)(R8*1), R13
	ADDQ  R9, R13
	TESTL $0x600000, R13 // extra bits >= 31
	JNZ   midreload

llread:
	// Literal length into R11.
	RORXQ   $16, DX, R11
	SUBB    R11, BX
	SHRXQ   BX, AX, R12
	BZHIQ   R11, R12, R12
	RORXQ   $32, DX, R11
	ADDL    R12, R11
	MOVQ    R11, 104(SP)

	DECQ 40(SP)
	JZ   last

	// Unchained positions shorten the critical path.
	RORXQ   $24, R13, R13 // all state bits
	LEAQ    (DX)(R8*1), R12
	RORXQ   $24, R12, R12 // ll+ml state bits

	RORXQ   $24, DX, R11
	MOVQ    BX, BP
	SUBB    R11, BP
	SHRXQ   BP, AX, BP
	BZHIQ   R11, BP, BP // BZHI gives 0 for 0 bits
	MOVWQZX DX, DX
	ADDQ    BP, DX
	MOVQ    DX, 80(SP)
	MOVQ    0(SP), R11
	MOVQ    (R11)(DX*8), DX

	RORXQ   $24, R8, R11
	MOVQ    BX, BP
	SUBB    R12, BP
	SHRXQ   BP, AX, BP
	BZHIQ   R11, BP, BP
	MOVWQZX R8, R8
	ADDQ    BP, R8
	MOVQ    R8, 88(SP)
	MOVQ    8(SP), R11
	MOVQ    (R11)(R8*8), R8

	RORXQ   $24, R9, R11
	SUBB    R13, BX
	SHRXQ   BX, AX, BP
	BZHIQ   R11, BP, BP
	MOVWQZX R9, R9
	ADDQ    BP, R9
	MOVQ    R9, 96(SP)
	MOVQ    16(SP), R11
	MOVQ    (R11)(R9*8), R9
	RELOAD

exec:
	MOVQ 104(SP), R11
	MOVQ 112(SP), BP
	LEAQ (SI)(R11*1), R12 // oLitEnd
	LEAQ (R12)(BP*1), R13 // oMatchEnd
	LEAQ (DI)(R11*1), BP  // lp + ll
	CMPQ BP, 48(SP)
	JHI  pending
	CMPQ R13, 56(SP)
	JHI  pending

	MOVUPS (DI), X0
	MOVUPS X0, (SI)
	CMPQ   R11, $16
	JHI    litlong // rare, so out of line

litdone:
	MOVQ BP, DI
	MOVQ R12, BP
	SUBQ R10, BP // match source
	JCS  badoff
	CMPQ BP, 64(SP)
	JCS  badoff
	CMPQ R10, $16
	JCS  short

	// off may be under 32: load, store, repeat.
	MOVUPS (BP), X0
	MOVUPS X0, (R12)
	LEAQ   16(R12), R11
	CMPQ   R11, R13
	JCS    matchlong

matchdone:
	MOVQ R13, SI

next:
	CMPQ CX, 72(SP)
	JCC  loop
	CMPQ 40(SP), $0
	JNE  slowbits
	MOVQ $const_seqDone, ret+8(FP)
	JMP  exit

litlong:
	ADDQ $16, SI
	ADDQ $16, DI

litloop:
	MOVUPS (DI), X0
	MOVUPS X0, (SI)
	MOVUPS 16(DI), X1
	MOVUPS X1, 16(SI)
	ADDQ   $32, SI
	ADDQ   $32, DI
	CMPQ   SI, R12
	JCS    litloop
	JMP    litdone

matchlong:
	MOVQ R11, R12
	ADDQ $16, BP

longloop:
	MOVUPS (BP), X0
	MOVUPS X0, (R12)
	MOVUPS 16(BP), X1
	MOVUPS X1, 16(R12)
	ADDQ   $32, BP
	ADDQ   $32, R12
	CMPQ   R12, R13
	JCS    longloop
	JMP    matchdone

short:
	CMPQ R10, $8
	JCC  short8

	// overlapCopy8: multiply spreads the pattern.
	LEAQ  ovTab<>(SB), R11
	MOVQ  (R11)(R10*8), R11
	MOVL  (BP), SI
	BZHIQ R11, SI, SI
	SHRQ  $32, R11
	IMULL R11, SI
	MOVL  SI, (R12)
	LEAQ  ovTab<>(SB), R11
	ADDQ  64(R11)(R10*8), BP
	MOVL  (BP), SI
	MOVL  SI, 4(R12)
	ADDQ  128(R11)(R10*8), BP
	JMP   shortrest

short8:
	MOVQ (BP), SI
	MOVQ SI, (R12)
	ADDQ $8, BP

shortrest:
	// Distance now 8-15: Go's 16-byte branch unreachable.
	ADDQ $8, R12
	CMPQ R12, R13
	JCC  matchdone

shortloop:
	MOVQ (BP), SI
	MOVQ SI, (R12)
	ADDQ $8, BP
	ADDQ $8, R12
	CMPQ R12, R13
	JCS  shortloop
	JMP  matchdone

midreload:
	RELOAD
	JMP llread

last:
	// Makes the next ptr check fail.
	MOVQ $-1, 72(SP)
	JMP  exec

ofsmall:
	// ofBits 0 or 1.
	MOVQ  DX, R12
	SHRQ  $32, R12
	TESTB R11, R11
	JNZ   of1
	TESTQ R12, R12
	JNZ   ofdone
	MOVQ  24(SP), R12
	MOVQ  R10, 24(SP)
	MOVQ  R12, R10
	JMP   ofdone

of1:
	SUBQ  $1, BX
	SHRXQ BX, AX, R13
	ANDQ  $1, R13
	CMPQ  R12, $1
	ADCQ  $0, R13 // + (ll == 0)
	MOVQ  R9, R12
	SHRQ  $32, R12
	ADDQ  R12, R13
	MOVQ  24(SP), R12
	CMPQ  R13, $1
	JNE   of1not1
	MOVQ  R10, 24(SP)
	MOVQ  R12, R10
	JMP   of1fix

of1not1:
	CMPQ R13, $3
	JNE  of1r2
	LEAQ -1(R10), R13
	JMP  of1set

of1r2:
	MOVQ 32(SP), R13

of1set:
	MOVQ R12, 32(SP)
	MOVQ R10, 24(SP)
	MOVQ R13, R10

of1fix:
	// Offset 0 is corrupt: -1.
	CMPQ R10, $1
	SBBQ $0, R10
	JMP  ofdone

slowbits:
	MOVQ $const_seqSlowBits, ret+8(FP)
	JMP  exit

pending:
	SUBQ R12, R13
	MOVQ s+0(FP), R12
	MOVQ R11, seqState_ll(R12)
	MOVQ R13, seqState_ml(R12)
	MOVQ R10, seqState_off(R12)
	MOVQ $const_seqPending, ret+8(FP)
	JMP  exit

badoff:
	MOVQ R12, SI
	SUBQ R11, SI
	MOVQ $const_seqBadOffset, ret+8(FP)

exit:
	MOVQ s+0(FP), R12
	SUBQ seqState_br+bitReader_in(R12), CX
	MOVQ CX, seqState_br+bitReader_ptr(R12)
	MOVQ AX, seqState_br+bitReader_bits(R12)
	MOVL $64, R11
	SUBQ BX, R11
	MOVQ R11, seqState_br+bitReader_consumed(R12)
	MOVQ 80(SP), R11
	MOVQ R11, seqState_llS(R12)
	MOVQ 88(SP), R11
	MOVQ R11, seqState_mlS(R12)
	MOVQ 96(SP), R11
	MOVQ R11, seqState_ofS(R12)
	MOVQ R10, seqState_rep(R12)
	MOVQ 24(SP), R11
	MOVQ R11, seqState_rep+8(R12)
	MOVQ 32(SP), R11
	MOVQ R11, seqState_rep+16(R12)
	MOVQ 40(SP), R11
	MOVQ R11, seqState_nbSeq(R12)
	SUBQ seqState_out(R12), SI
	MOVQ SI, seqState_op(R12)
	SUBQ seqState_lits(R12), DI
	MOVQ DI, seqState_lp(R12)
	RET

none:
	MOVQ $const_seqDone, ret+8(FP)
	RET

// By offset 0-7: multiplier<<32 | bits kept,
// then dec32, then 8-dec64.
DATA ovTab<>+0(SB)/8, $0
DATA ovTab<>+8(SB)/8, $0x0101010100000008
DATA ovTab<>+16(SB)/8, $0x0001000100000010
DATA ovTab<>+24(SB)/8, $0x0100000100000018
DATA ovTab<>+32(SB)/8, $0x0000000100000020
DATA ovTab<>+40(SB)/8, $0x0000000100000020
DATA ovTab<>+48(SB)/8, $0x0000000100000020
DATA ovTab<>+56(SB)/8, $0x0000000100000020
DATA ovTab<>+64(SB)/8, $0
DATA ovTab<>+72(SB)/8, $1
DATA ovTab<>+80(SB)/8, $2
DATA ovTab<>+88(SB)/8, $1
DATA ovTab<>+96(SB)/8, $4
DATA ovTab<>+104(SB)/8, $4
DATA ovTab<>+112(SB)/8, $4
DATA ovTab<>+120(SB)/8, $4
DATA ovTab<>+128(SB)/8, $0
DATA ovTab<>+136(SB)/8, $0
DATA ovTab<>+144(SB)/8, $0
DATA ovTab<>+152(SB)/8, $1
DATA ovTab<>+160(SB)/8, $0
DATA ovTab<>+168(SB)/8, $-1
DATA ovTab<>+176(SB)/8, $-2
DATA ovTab<>+184(SB)/8, $-3
GLOBL ovTab<>(SB), RODATA|NOPTR, $192
