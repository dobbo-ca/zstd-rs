//go:build !purego

#include "textflag.h"
#include "go_asm.h"

#define RELOAD(t) \
	LSR	$3, R4, t; \
	SUB	t, R2; \
	AND	$7, R4; \
	MOVD	(R1)(R2), R3

// func seqLoopAsm(s *seqState) int
TEXT ·seqLoopAsm(SB), NOSPLIT, $32-16
	MOVD	s+0(FP), R0
	MOVD	seqState_br+bitReader_in(R0), R1
	MOVD	seqState_br+bitReader_ptr(R0), R2
	MOVD	seqState_br+bitReader_bits(R0), R3
	MOVD	seqState_br+bitReader_consumed(R0), R4
	MOVD	seqState_llT(R0), R5
	MOVD	seqState_mlT(R0), R6
	MOVD	seqState_ofT(R0), R7
	MOVD	seqState_llS(R0), R8
	MOVD	seqState_mlS(R0), R9
	MOVD	seqState_ofS(R0), R10
	MOVD	seqState_rep(R0), R11
	MOVD	seqState_rep+8(R0), R12
	MOVD	seqState_rep+16(R0), R13
	MOVD	seqState_out(R0), R22
	MOVD	seqState_out+8(R0), R16
	MOVD	seqState_op(R0), R14
	MOVD	seqState_base(R0), R15
	ADD	R22, R14
	ADD	R22, R15
	ADD	R22, R16
	SUB	$const_wildcopySlack, R16
	MOVD	seqState_lits(R0), R22
	MOVD	seqState_lp(R0), R17
	MOVD	seqState_litEnd(R0), R19
	ADD	R22, R17
	ADD	R22, R19
	MOVD	R15, base-8(SP)
	MOVD	R16, oendW-16(SP)
	MOVD	R19, litEnd-24(SP)
	MOVD	seqState_nbSeq(R0), R20

loop:
	CMP	$1, R20
	BLT	done
	BEQ	last
	CMP	$16, R2
	BLT	slowBits

decode:
	MOVD	(R5)(R8<<3), R8
	MOVD	(R6)(R9<<3), R9
	MOVD	(R7)(R10<<3), R10
	LSL	R4, R3, R24 // every read shifts from here
	UBFX	$16, R10, $8, R22
	LSR	$32, R8, R0
	LSR	$32, R9, R21
	LSR	$32, R10, R15
	CMP	$1, R22
	BLS	ofSmall
	MOVD	R12, R13
	MOVD	R11, R12
	NEG	R22, R11
	LSR	R11, R24, R11
	ADD	R15, R11
	B	ofDone

ofSmall:
	CBNZ	R22, ofOne
	CBNZ	R0, ofDone
	MOVD	R11, R16
	MOVD	R12, R11
	MOVD	R16, R12
	B	ofDone

ofOne:
	CMP	$0, R0
	CINC	EQ, R15, R15
	ADD	R24>>63, R15, R15
	CMP	$1, R15
	CSEL	EQ, R12, R13, R16
	SUB	$1, R11, R19
	CMP	$3, R15
	CSEL	EQ, R19, R16, R16
	CMP	$0, R16
	CSINV	NE, R16, ZR, R16 // offset 0 becomes -1
	CMP	$1, R15
	CSEL	NE, R12, R13, R13
	MOVD	R11, R12
	MOVD	R16, R11

ofDone:
	UBFX	$16, R9, $8, R23
	UBFX	$16, R8, $8, R25
	ADD	R22, R23, R19 // ll bit position
	ADD	R25, R19, R16 // state bit position
	LSL	R22, R24, R15
	LSR	$1, R15
	EOR	$63, R23
	LSR	R23, R15
	ADD	R15, R21
	CMP	$31, R16
	BLO	noReload
	ADD	R19, R4
	RELOAD(R15)
	LSL	R4, R3, R24
	MOVD	ZR, R19
	MOVD	R25, R16

noReload:
	LSL	R19, R24, R15
	LSR	$1, R15
	EOR	$63, R25
	LSR	R25, R15
	ADD	R15, R0
	ADD	R16, R4
	SUB	$1, R20
	CBZ	R20, exec
	UBFX	$24, R8, $8, R22
	UBFX	$24, R9, $8, R23
	UBFX	$24, R10, $8, R25
	LSL	R16, R24, R15
	ADD	R22, R16
	LSL	R16, R24, R19
	ADD	R23, R16
	LSL	R16, R24, R24
	ADD	R22, R4
	ADD	R23, R4
	ADD	R25, R4
	LSR	$1, R15
	EOR	$63, R22
	LSR	R22, R15
	AND	$0xFFFF, R8
	ADD	R15, R8
	LSR	$1, R19
	EOR	$63, R23
	LSR	R23, R19
	AND	$0xFFFF, R9
	ADD	R19, R9
	LSR	$1, R24
	EOR	$63, R25
	LSR	R25, R24
	AND	$0xFFFF, R10
	ADD	R24, R10
	RELOAD(R25)

exec:
	ADD	R0, R14, R22 // oLitEnd
	ADD	R21, R22, R23 // oMatchEnd
	ADD	R0, R17, R24
	MOVD	litEnd-24(SP), R25
	CMP	R25, R24
	BGT	pending
	MOVD	oendW-16(SP), R25
	CMP	R25, R23
	BGT	pending

	VLD1	(R17), [V0.B16]
	VST1	[V0.B16], (R14)
	CMP	$16, R0
	BLS	litDone
	ADD	$16, R17, R25
	ADD	$16, R14, R24

litLoop:
	VLD1.P	32(R25), [V0.B16, V1.B16]
	VST1.P	[V0.B16, V1.B16], 32(R24)
	CMP	R22, R24
	BLO	litLoop

litDone:
	ADD	R0, R17
	MOVD	base-8(SP), R25
	SUB	R25, R22, R24
	CMP	R24, R11
	BHI	badOffset
	SUB	R11, R22, R24 // match source
	CMP	$16, R11
	BLO	short
	VLD1.P	16(R24), [V0.B16]
	VST1.P	[V0.B16], 16(R22)
	CMP	R23, R22
	BHS	matchDone

// Sources may overlap earlier 16-byte stores.
long:
	VLD1.P	16(R24), [V0.B16]
	VST1.P	[V0.B16], 16(R22)
	VLD1.P	16(R24), [V1.B16]
	VST1.P	[V1.B16], 16(R22)
	CMP	R23, R22
	BLO	long
	B	matchDone

short:
	CMP	$8, R11
	BHS	short8
	MOVBU	(R24), R25
	MOVB	R25, (R22)
	MOVBU	1(R24), R25
	MOVB	R25, 1(R22)
	MOVBU	2(R24), R25
	MOVB	R25, 2(R22)
	MOVBU	3(R24), R25
	MOVB	R25, 3(R22)
	MOVD	$·dec32(SB), R25
	MOVD	(R25)(R11<<3), R25
	ADD	R25, R24
	MOVWU	(R24), R25
	MOVW	R25, 4(R22)
	MOVD	$·dec64(SB), R25
	MOVD	(R25)(R11<<3), R25
	SUB	R25, R24
	B	short8Done

short8:
	MOVD	(R24), R25
	MOVD	R25, (R22)

short8Done:
	ADD	$8, R22
	ADD	$8, R24
	CMP	$8, R21
	BLS	matchDone
	SUB	R24, R22, R25
	CMP	$16, R25
	BGE	short16
	CMP	R23, R22
	BHS	matchDone

loop8:
	MOVD.P	8(R24), R25
	MOVD.P	R25, 8(R22)
	CMP	R23, R22
	BLO	loop8
	B	matchDone

short16:
	VLD1.P	16(R24), [V0.B16]
	VST1.P	[V0.B16], 16(R22)
	CMP	R23, R22
	BLO	long

matchDone:
	MOVD	R23, R14
	B	loop

// Last sequence keeps its states.
last:
	CMP	$16, R2
	BLT	slowBits
	MOVD	s+0(FP), R22
	MOVD	R8, seqState_llS(R22)
	MOVD	R9, seqState_mlS(R22)
	MOVD	R10, seqState_ofS(R22)
	B	decode

pending:
	MOVD	s+0(FP), R22
	MOVD	R0, seqState_ll(R22)
	MOVD	R21, seqState_ml(R22)
	MOVD	R11, seqState_off(R22)
	MOVD	$const_seqPending, R24
	B	exit

slowBits:
	MOVD	$const_seqSlowBits, R24
	B	exit

badOffset:
	MOVD	$const_seqBadOffset, R24
	B	exit

done:
	MOVD	$const_seqDone, R24

exit:
	MOVD	s+0(FP), R0
	MOVD	R2, seqState_br+bitReader_ptr(R0)
	MOVD	R3, seqState_br+bitReader_bits(R0)
	MOVD	R4, seqState_br+bitReader_consumed(R0)
	CMP	$0, R20
	BLE	statesSaved
	MOVD	R8, seqState_llS(R0)
	MOVD	R9, seqState_mlS(R0)
	MOVD	R10, seqState_ofS(R0)

statesSaved:
	MOVD	R11, seqState_rep(R0)
	MOVD	R12, seqState_rep+8(R0)
	MOVD	R13, seqState_rep+16(R0)
	MOVD	R20, seqState_nbSeq(R0)
	MOVD	seqState_out(R0), R22
	SUB	R22, R14
	MOVD	R14, seqState_op(R0)
	MOVD	seqState_lits(R0), R22
	SUB	R22, R17
	MOVD	R17, seqState_lp(R0)
	MOVD	R24, ret+8(FP)
	RET
