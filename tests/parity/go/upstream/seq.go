package zstd

// seqSym packs ZSTD_seqSymbol:
// nextState:0-15 | addBits:16-23 | nbBits:24-31 | base:32-63.
type seqSym uint64

func (c seqSym) next() uint    { return uint(c & 0xFFFF) }
func (c seqSym) addBits() uint { return uint(c>>16) & 0xFF }
func (c seqSym) nbBits() uint  { return uint(c>>24) & 0xFF }
func (c seqSym) base() uint64  { return uint64(c >> 32) }

type seqTables struct {
	ll, ml              [1 << 9]seqSym
	of                  [1 << 8]seqSym
	llT, ofT, mlT       []seqSym
	llLog, ofLog, mlLog uint
}

var (
	llBase = [36]uint32{0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
		16, 18, 20, 22, 24, 28, 32, 40, 48, 64, 0x80, 0x100, 0x200, 0x400, 0x800, 0x1000,
		0x2000, 0x4000, 0x8000, 0x10000}
	llBits = [36]uint8{0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
		1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16}
	mlBase = [53]uint32{3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
		19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34,
		35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 0x83, 0x103, 0x203, 0x403, 0x803,
		0x1003, 0x2003, 0x4003, 0x8003, 0x10003}
	mlBits = [53]uint8{0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
		1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16}
	ofBase [32]uint32
	ofBits [32]uint8

	llDefault [1 << 6]seqSym
	mlDefault [1 << 6]seqSym
	ofDefault [1 << 5]seqSym
)

func init() {
	for i := range ofBase {
		ofBits[i] = uint8(i)
		ofBase[i] = 1<<i - 3
	}
	ofBase[0], ofBase[1] = 0, 1
	llNorm := []int16{4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1,
		2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1, -1, -1, -1, -1}
	mlNorm := []int16{1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1,
		1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
		1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1}
	ofNorm := []int16{1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1,
		1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1}
	buildSeqTable(llDefault[:], llNorm, 35, 6, llBase[:], llBits[:])
	buildSeqTable(mlDefault[:], mlNorm, 52, 6, mlBase[:], mlBits[:])
	buildSeqTable(ofDefault[:], ofNorm, 28, 5, ofBase[:], ofBits[:])
}

// buildSeqTable ports ZSTD_buildFSETable_body.
func buildSeqTable(dt []seqSym, norm []int16, maxSV, tableLog uint, base []uint32, addBits []uint8) {
	var sym [1 << 9]uint8
	// Indexed by s&63 so the hot loop has no bounds checks.
	var next [64]uint16
	var tmpl [64]seqSym
	for s := uint(0); s <= maxSV; s++ {
		if norm[s] == -1 {
			next[s] = 1
		} else {
			next[s] = uint16(norm[s])
		}
		tmpl[s] = seqSym(uint64(addBits[s])<<16 | uint64(base[s])<<32)
	}
	spreadSymbols(sym[:], norm, maxSV, tableLog)
	size := uint(1) << tableLog
	syms := sym[:size]
	dt = dt[:len(syms)]
	for u, s := range syms {
		s &= 63
		ns := uint(next[s])
		next[s]++
		nb := tableLog - highbit(uint32(ns))
		dt[u] = tmpl[s] | seqSym(uint64(ns<<nb-size)|uint64(nb)<<24)
	}
}

// buildSeq ports ZSTD_buildSeqTable for one of the three tables.
func (d *Decoder) buildSeq(mode byte, src []byte, own []seqSym, def []seqSym, defLog, maxSV, maxLog uint,
	base []uint32, addBits []uint8, t *[]seqSym, log *uint) (int, bool) {
	switch mode {
	case 0: // predefined
		*t, *log = def, defLog
		return 0, true
	case 1: // RLE
		if len(src) == 0 || uint(src[0]) > maxSV {
			return 0, false
		}
		s := src[0]
		own[0] = seqSym(uint64(addBits[s])<<16 | uint64(base[s])<<32)
		*t, *log = own[:1], 0
		return 1, true
	case 2: // FSE compressed
		var norm [53]int16
		tableLog := uint(0)
		n, ok := readNCount(norm[:], &maxSV, &tableLog, src)
		if !ok || tableLog > maxLog {
			return 0, false
		}
		buildSeqTable(own, norm[:], maxSV, tableLog, base, addBits)
		*t, *log = own[:1<<tableLog], tableLog
		return n, true
	default: // repeat
		return 0, d.fseOK
	}
}

// decodeSeqHeaders ports ZSTD_decodeSeqHeaders.
func (d *Decoder) decodeSeqHeaders(src []byte) (nbSeq, n int, err error) {
	if len(src) < 1 {
		return 0, 0, errSequences
	}
	nbSeq = int(src[0])
	ip := 1
	if nbSeq > 0x7F {
		if nbSeq == 0xFF {
			if ip+2 > len(src) {
				return 0, 0, errSequences
			}
			nbSeq = int(src[1]) | int(src[2])<<8 + 0x7F00
			ip += 2
		} else {
			if ip >= len(src) {
				return 0, 0, errSequences
			}
			nbSeq = (nbSeq-0x80)<<8 + int(src[1])
			ip++
		}
	}
	if nbSeq == 0 {
		if ip != len(src) {
			return 0, 0, errSequences
		}
		return 0, ip, nil
	}
	if ip+1 > len(src) || src[ip]&3 != 0 {
		return 0, 0, errSequences
	}
	modes := src[ip]
	ip++
	t := &d.seq
	k, ok := d.buildSeq(modes>>6, src[ip:], t.ll[:], llDefault[:], 6, 35, 9, llBase[:], llBits[:], &t.llT, &t.llLog)
	if !ok {
		return 0, 0, errFSE
	}
	ip += k
	k, ok = d.buildSeq(modes>>4&3, src[ip:], t.of[:], ofDefault[:], 5, 31, 8, ofBase[:], ofBits[:], &t.ofT, &t.ofLog)
	if !ok {
		return 0, 0, errFSE
	}
	ip += k
	k, ok = d.buildSeq(modes>>2&3, src[ip:], t.ml[:], mlDefault[:], 6, 52, 9, mlBase[:], mlBits[:], &t.mlT, &t.mlLog)
	if !ok {
		return 0, 0, errFSE
	}
	return nbSeq, ip + k, nil
}

// seqState is the state of the fused decode+execute loop.
type seqState struct {
	br            bitReader
	llT, mlT, ofT []seqSym
	llS, mlS, ofS uint
	rep           [3]uint64
	nbSeq         int

	out    []byte // len(out) is the end of the frame's output
	op     int
	base   int // frame start; matches may not reach before it
	lits   []byte
	lp     int
	litEnd int

	ll, ml, off uint64 // decoded sequence left for execEnd
}

// Results of seqLoop.
const (
	seqDone      = iota // no sequences left
	seqSlowBits         // bitstream near its start: decode one carefully
	seqPending          // ll, ml, off need execEnd
	seqBadOffset        // match reaches before the frame
)

// run decodes and executes all sequences of a block.
func (s *seqState) run() error {
	for s.nbSeq > 0 {
		switch seqLoop(s) {
		case seqSlowBits:
			ll, ml, off := s.decodeCareful()
			if err := s.execEnd(ll, ml, off); err != nil {
				return err
			}
		case seqPending:
			if err := s.execEnd(s.ll, s.ml, s.off); err != nil {
				return err
			}
		case seqBadOffset:
			return errOffset
		}
	}
	return nil
}

// decodeCareful ports ZSTD_decodeSequence with the checked reloads.
func (s *seqState) decodeCareful() (ll, ml, off uint64) {
	br := &s.br
	lld, mld, ofd := s.llT[s.llS], s.mlT[s.mlS], s.ofT[s.ofS]
	ll, ml = lld.base(), mld.base()
	llBits, mlBits, ofBits := lld.addBits(), mld.addBits(), ofd.addBits()
	readFast := func(n uint) uint64 {
		v := br.lookFast(n)
		br.consumed += n
		return v
	}
	r0, r1, r2 := s.rep[0], s.rep[1], s.rep[2]
	if ofBits > 1 {
		off = ofd.base() + readFast(ofBits)
		r2, r1, r0 = r1, r0, off
	} else {
		ll0 := uint64(0)
		if ll == 0 {
			ll0 = 1
		}
		if ofBits == 0 {
			if ll0 == 1 {
				off = r1
				r1, r0 = r0, off
			} else {
				off = r0
			}
		} else {
			off = ofd.base() + ll0 + readFast(1)
			t := r2
			switch off {
			case 1:
				t = r1
			case 3:
				t = r0 - 1
			}
			if t == 0 {
				t-- // offset 0 is corrupt; -1 fails the range check
			}
			if off != 1 {
				r2 = r1
			}
			r1, r0 = r0, t
			off = t
		}
	}
	s.rep = [3]uint64{r0, r1, r2}
	if mlBits > 0 {
		ml += readFast(mlBits)
	}
	if llBits+mlBits+ofBits >= 57-(9+9+8) {
		br.reload()
	}
	if llBits > 0 {
		ll += readFast(llBits)
	}
	if s.nbSeq > 1 {
		s.llS = lld.next() + uint(br.read(lld.nbBits()))
		s.mlS = mld.next() + uint(br.read(mld.nbBits()))
		s.ofS = ofd.next() + uint(br.read(ofd.nbBits()))
		br.reload()
	}
	s.nbSeq--
	return ll, ml, off
}

// execEnd ports ZSTD_execSequenceEnd without dictionaries: exact copies,
// nothing written past the match end.
func (s *seqState) execEnd(ll, ml, off uint64) error {
	out, op := s.out, s.op
	if ll+ml > uint64(len(out)-op) || ll > uint64(s.litEnd-s.lp) {
		return errSequences
	}
	oLitEnd := op + int(ll)
	copy(out[op:oLitEnd], s.lits[s.lp:])
	s.lp += int(ll)
	if off == 0 || off > uint64(oLitEnd-s.base) {
		return errOffset
	}
	m := oLitEnd - int(off)
	end := oLitEnd + int(ml)
	if int(off) >= int(ml) {
		copy(out[oLitEnd:end], out[m:])
	} else {
		// out[m:i] is whole periods, so it doubles safely.
		for i := oLitEnd; i < end; {
			i += copy(out[i:end], out[m:i])
		}
	}
	s.op = end
	return nil
}
