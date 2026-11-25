//go:build (amd64 || arm64) && !purego && kernelcheck

package zstd

import (
	"bytes"
	"fmt"
)

// Under -tags kernelcheck every kernel call also runs the Go kernel on a
// copy; the first difference in state or output lands in kernelDiff.
const kernelCheck = true

var (
	kernelDiff  string
	kernelCalls [3]int
)

func kernelFail(format string, args ...any) {
	if kernelDiff == "" {
		kernelDiff = fmt.Sprintf(format, args...)
	}
}

func seqCheck(s *seqState) int {
	kernelCalls[0]++
	g := *s
	g.out = bytes.Clone(s.out)
	want := seqLoopGo(&g)
	got := seqLoopAsm(s)
	state := func(ret int, s *seqState) string {
		return fmt.Sprintf("ret %d br %d/%#x/%d states %d/%d/%d rep %v nb %d op %d lp %d",
			ret, s.br.ptr, s.br.bits, s.br.consumed, s.llS, s.mlS, s.ofS, s.rep, s.nbSeq, s.op, s.lp)
	}
	if sa, ga := state(got, s), state(want, &g); sa != ga {
		kernelFail("seq state: asm %s, Go %s", sa, ga)
	} else if want == seqPending && (s.ll != g.ll || s.ml != g.ml || s.off != g.off) {
		kernelFail("pending sequence: asm %d/%d/%d, Go %d/%d/%d", s.ll, s.ml, s.off, g.ll, g.ml, g.off)
	} else if !bytes.Equal(s.out[:s.op], g.out[:g.op]) {
		kernelFail("seq output differs")
	}
	return got
}

func huf4X1Check(a *hufArgs, src, dst []byte, dt *[1 << 12]uint16) {
	kernelCalls[1]++
	g, gdst := *a, bytes.Clone(dst)
	huf4X1LoopGo(&g, src, gdst, dt)
	huf4X1LoopAsm(a, &src[0], &dst[0], dt)
	if *a != g || !bytes.Equal(dst, gdst) {
		kernelFail("huf4X1: asm %+v, Go %+v, output equal %v", *a, g, bytes.Equal(dst, gdst))
	}
}

func huf4X2Check(a *hufArgs, src, dst []byte, dt *[1 << 12]uint32) {
	kernelCalls[2]++
	g, gdst := *a, bytes.Clone(dst)
	huf4X2LoopGo(&g, src, gdst, dt)
	huf4X2LoopAsm(a, &src[0], &dst[0], dt)
	if *a != g || !bytes.Equal(dst, gdst) {
		kernelFail("huf4X2: asm %+v, Go %+v, output equal %v", *a, g, bytes.Equal(dst, gdst))
	}
}
