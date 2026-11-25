package zstd

// decodeBlock ports ZSTD_decompressBlock_internal for a compressed block
// written to out[op:]. base is the start of the frame in out, bsm the
// frame's block size limit and maxOut the most bytes the block may decode
// to. It returns the decoded size.
func (d *Decoder) decodeBlock(out []byte, op, base, bsm, maxOut int, blk []byte) (int, error) {
	lits, litSize, n, err := d.decodeLiterals(blk, bsm, len(out)-op)
	if err != nil {
		return 0, err
	}
	blk = blk[n:]
	nbSeq, n, err := d.decodeSeqHeaders(blk)
	if err != nil {
		return 0, err
	}
	s := seqState{
		out: out, op: op, base: base,
		lits: lits, litEnd: litSize,
	}
	if nbSeq > 0 {
		d.fseOK = true
		t := &d.seq
		s.llT, s.mlT, s.ofT = t.llT, t.mlT, t.ofT
		s.rep, s.nbSeq = d.rep, nbSeq
		if !s.br.init(blk[n:]) {
			return 0, errSequences
		}
		s.llS = uint(s.br.read(t.llLog))
		s.br.reload()
		s.ofS = uint(s.br.read(t.ofLog))
		s.br.reload()
		s.mlS = uint(s.br.read(t.mlLog))
		s.br.reload()
		if err := s.run(); err != nil {
			return 0, err
		}
		if !s.br.finished() {
			return 0, errSequences
		}
		d.rep = s.rep
	}
	last := litSize - s.lp
	if last > len(out)-s.op {
		return 0, errSequences
	}
	s.op += copy(out[s.op:], lits[s.lp:litSize])
	// Checked once per block, as the streaming libzstd decoder does.
	if s.op-op > maxOut {
		return 0, errBlock
	}
	return s.op - op, nil
}
