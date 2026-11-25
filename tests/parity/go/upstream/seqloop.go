package zstd

// Go version of the fused sequence decode+execute fast loop; asm replaces it
// later. It ports ZSTD_decodeSequence (aarch64 variant) and the wildcopy
// path of ZSTD_execSequence, and returns to run for everything else.

var (
	dec32 = [8]int{0, 1, 2, 1, 4, 4, 4, 4}
	dec64 = [8]int{8, 8, 8, 7, 8, 9, 10, 11}
)

func copy16(dst, src []byte) { *(*[16]byte)(dst) = *(*[16]byte)(src) }
func copy8(dst, src []byte)  { *(*[8]byte)(dst) = *(*[8]byte)(src) }

// seqLoopGo runs sequences while the bitstream has 16+ bytes left, so every
// reload is a full one, and while each copy fits the wildcopy fast path.
func seqLoopGo(s *seqState) int {
	in := s.br.in
	ptr, bits, c := s.br.ptr, s.br.bits, s.br.consumed
	llT, mlT, ofT := s.llT, s.mlT, s.ofT
	llS, mlS, ofS := s.llS, s.mlS, s.ofS
	r0, r1, r2 := s.rep[0], s.rep[1], s.rep[2]
	out, op, base := s.out, s.op, s.base
	lits, lp, litEnd := s.lits, s.lp, s.litEnd
	oendW := len(out) - wildcopySlack
	nb := s.nbSeq
	status := seqDone
	for nb > 0 {
		if ptr < 16 {
			status = seqSlowBits
			break
		}
		lld, mld, ofd := llT[llS], mlT[mlS], ofT[ofS]
		llBits, mlBits, ofBits := lld.addBits(), mld.addBits(), ofd.addBits()
		ll, ml := lld.base(), mld.base()

		// Go shifts by 64 give 0, so reading 0 bits needs no branch.
		var off uint64
		if ofBits > 1 {
			off = ofd.base() + bits<<c>>(64-ofBits)
			c += ofBits
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
				off = ofd.base() + ll0 + bits<<c>>63
				c++
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
		ml += bits << c >> (64 - mlBits)
		c += mlBits
		if llBits+mlBits+ofBits >= 57-(9+9+8) {
			ptr -= int(c >> 3)
			c &= 7
			bits = le64(in[ptr:])
		}
		ll += bits << c >> (64 - llBits)
		c += llBits
		nb--
		if nb > 0 {
			n := lld.nbBits()
			llS = lld.next() + uint(bits<<c>>(64-n))
			c += n
			n = mld.nbBits()
			mlS = mld.next() + uint(bits<<c>>(64-n))
			c += n
			n = ofd.nbBits()
			ofS = ofd.next() + uint(bits<<c>>(64-n))
			c += n
			ptr -= int(c >> 3)
			c &= 7
			bits = le64(in[ptr:])
		}

		oLitEnd := op + int(ll)
		oMatchEnd := oLitEnd + int(ml)
		if lp+int(ll) > litEnd || oMatchEnd > oendW {
			s.ll, s.ml, s.off = ll, ml, off
			status = seqPending
			break
		}

		copy16(out[op:], lits[lp:])
		if ll > 16 {
			for o, i := op+16, lp+16; o < oLitEnd; o, i = o+32, i+32 {
				copy16(out[o:], lits[i:])
				copy16(out[o+16:], lits[i+16:])
			}
		}
		lp += int(ll)

		if off > uint64(oLitEnd-base) {
			status = seqBadOffset
			break
		}
		o, m := oLitEnd, oLitEnd-int(off)
		if off >= 16 {
			copy16(out[o:], out[m:])
			for o, m = o+16, m+16; o < oMatchEnd; o, m = o+32, m+32 {
				copy16(out[o:], out[m:])
				copy16(out[o+16:], out[m+16:])
			}
		} else {
			// ZSTD_overlapCopy8: spread the offset to at least 8.
			if off < 8 {
				out[o] = out[m]
				out[o+1] = out[m+1]
				out[o+2] = out[m+2]
				out[o+3] = out[m+3]
				m += dec32[off]
				*(*[4]byte)(out[o+4:]) = *(*[4]byte)(out[m:])
				m -= dec64[off]
			} else {
				copy8(out[o:], out[m:])
			}
			o, m = o+8, m+8
			if ml > 8 {
				if o-m < 16 {
					for ; o < oMatchEnd; o, m = o+8, m+8 {
						copy8(out[o:], out[m:])
					}
				} else {
					copy16(out[o:], out[m:])
					for o, m = o+16, m+16; o < oMatchEnd; o, m = o+32, m+32 {
						copy16(out[o:], out[m:])
						copy16(out[o+16:], out[m+16:])
					}
				}
			}
		}
		op = oMatchEnd
	}
	s.br.ptr, s.br.bits, s.br.consumed = ptr, bits, c
	s.llS, s.mlS, s.ofS = llS, mlS, ofS
	s.rep = [3]uint64{r0, r1, r2}
	s.op, s.lp = op, lp
	s.nbSeq = nb
	return status
}
