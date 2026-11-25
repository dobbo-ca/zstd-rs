// Command oracle is the Go side of the parity check (see ../../../../docs/parity.md).
//
//	oracle -gen <testdata> <inputs>   write the test inputs
//	oracle <inputs> <outputs>         decode every input
//	oracle -bench N <tiles>           time the decode of every tile
//
// Decode mode writes one file per input, <name>.res: a record per operation, in the order the
// operations run, each a header line "<op> <kind> <len>\n" and then <len> bytes. The kind is out
// (the output bytes), or, for a failed operation, err (the error class, no newline) and partial
// (the bytes returned with the error). ORACLE_MAX_INPUT (bytes) skips larger inputs.
// examples/oracle.rs does the same with the Rust crate.
package main

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"flag"
	"fmt"
	"hash/fnv"
	"io/fs"
	"math/bits"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	zstd "github.com/dobbo-ca/zstd-go"
	"github.com/dobbo-ca/zstd-go/czi"
)

// memLimit bounds what a decode may allocate, so a hostile input cannot exhaust the memory of the machine. The
// Rust oracle sets the same number as Limits::max_alloc. ORACLE_MEM_LIMIT (bytes) overrides it,
// on both sides: parity also runs at a small limit, where the charging of every growth shows.
var memLimit int64 = 256 << 20

// maxInput skips the inputs over this size in decode mode (ORACLE_MAX_INPUT, bytes; 0 is no limit).
var maxInput int64

// maxDst is the largest czi destination an input may ask for.
const maxDst = 4 << 20

var errBadInput = errors.New("bad input")

func main() {
	gen := flag.Bool("gen", false, "write the inputs: -gen <testdata> <inputs>")
	bench := flag.Int("bench", 0, "decode every tile N times: -bench N <tiles>")
	flag.Parse()
	if v := os.Getenv("ORACLE_MEM_LIMIT"); v != "" {
		n, err := strconv.ParseInt(v, 10, 64)
		if err != nil || n <= 0 {
			check(fmt.Errorf("bad ORACLE_MEM_LIMIT %q", v))
		}
		memLimit = n
	}
	if v := os.Getenv("ORACLE_MAX_INPUT"); v != "" {
		n, err := strconv.ParseInt(v, 10, 64)
		if err != nil || n <= 0 {
			check(fmt.Errorf("bad ORACLE_MAX_INPUT %q", v))
		}
		maxInput = n
	}
	a := flag.Args()
	switch {
	case *bench > 0 && len(a) == 1:
		check(benchDir(a[0], *bench))
	case *gen && len(a) == 2:
		check(generate(a[0], a[1]))
	case !*gen && *bench == 0 && len(a) == 2:
		check(decodeDir(a[0], a[1]))
	default:
		flag.Usage()
		os.Exit(2)
	}
}

func check(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

// class is the stable token of an error; docs/parity.md has the table.
func class(err error) string {
	switch {
	case errors.Is(err, zstd.ErrChecksum):
		return "checksum"
	case errors.Is(err, zstd.ErrDictionary):
		return "dictionary"
	case errors.Is(err, zstd.ErrMemoryLimit):
		return "memory-limit"
	case errors.Is(err, zstd.ErrWindowTooLarge):
		return "window-too-large"
	case errors.Is(err, context.Canceled):
		return "cancelled"
	case errors.Is(err, zstd.ErrCorrupt):
		return "corrupt"
	case errors.Is(err, errBadInput):
		return "bad-input"
	case err.Error() == "czi: compression mode is not zstd0 or zstd1":
		return "mode"
	}
	return "other:" + err.Error()
}

type emitFn func(op string, b []byte, err error)

func decodeDir(in, out string) error {
	zstd.SetMemoryLimit(memLimit)
	if err := os.MkdirAll(out, 0o755); err != nil {
		return err
	}
	files, err := os.ReadDir(in)
	if err != nil {
		return err
	}
	cancelled, cancel := context.WithCancel(context.Background())
	cancel()
	prev := map[string][][]byte{} // the last reuseDepth inputs of each kind, in name order
	for _, f := range files {
		if f.IsDir() {
			continue
		}
		name := f.Name()
		if info, err := f.Info(); err != nil {
			return err
		} else if maxInput > 0 && info.Size() > maxInput {
			continue
		}
		data, err := os.ReadFile(filepath.Join(in, name))
		if err != nil {
			return err
		}
		var res bytes.Buffer
		record := func(op, kind string, b []byte) {
			fmt.Fprintf(&res, "%s %s %d\n", op, kind, len(b))
			res.Write(b)
		}
		emit := func(op string, b []byte, err error) {
			if err != nil {
				record(op, "err", []byte(class(err)))
				record(op, "partial", b) // what came before the error
				return
			}
			record(op, "out", b)
		}
		ext := filepath.Ext(name)
		switch ext {
		case ".zst":
			runFrames(emit, cancelled, data)
			runReuse(emit, prev[ext], data, func(d *zstd.Decoder, src []byte) ([]byte, error) {
				return d.DecodeAll(nil, src)
			}, zstd.NewDecoder)
		case ".czi":
			runCZI(emit, cancelled, data)
			runReuse(emit, prev[ext], data, cziOnce, czi.NewDecoder)
		default:
			continue
		}
		check(os.WriteFile(filepath.Join(out, name+".res"), res.Bytes(), 0o644))
		prev[ext] = append(prev[ext], data)
		if len(prev[ext]) > reuseDepth {
			prev[ext] = prev[ext][1:]
		}
	}
	return nil
}

// reuseDepth is how many earlier inputs of the same kind one reused decoder decodes before an
// input in the `reuse` operation. Neighbours in name order are mutations of one fixture, so a
// decoder that failed or grew on one meets the next.
const reuseDepth = 2

// runReuse decodes the earlier inputs and then data with one decoder (Go and Rust both do, and
// the results of the last must agree): the state a decode leaves behind must not show.
func runReuse[D any](emit emitFn, prev [][]byte, data []byte, decode func(D, []byte) ([]byte, error), newDecoder func() D) {
	d := newDecoder()
	for _, p := range prev {
		decode(d, p)
	}
	b, err := decode(d, data)
	emit("reuse", b, err)
}

// runFrames runs every operation on one input: DecodeAll into a nil dst, into a dst that holds
// "pre" (a failed decode must leave it as it was), into an empty dst with a third of the output as
// capacity and again into the dst that returned ("again"), ContentSize, and DecodeAllContext with
// a cancelled context.
func runFrames(emit emitFn, cancelled context.Context, data []byte) {
	d := zstd.NewDecoder()
	b, err := d.DecodeAll(nil, data)
	emit("dec", b, err)
	roomN := len(b) + 128<<10
	// A dst with spare capacity: growth starts from cap(dst), so a tight limit is charged
	// differently than from nil.
	capN := 64 << 10
	if err == nil {
		capN = len(b) / 3
	}
	b, err = d.DecodeAll(make([]byte, 0, capN), data)
	emit("cap", b, err)
	// The dst that decode returned, again: after a failure it has the capacity the failed decode
	// left it (the original, or what the frames before the failing one grew).
	b, err = d.DecodeAll(b[:0], data)
	emit("again", b, err)
	// A dst with room for the whole output: it never grows.
	b, err = d.DecodeAll(make([]byte, 0, roomN), data)
	emit("room", b, err)
	// The Rust oracle's `slice` (decode_to_slice into a buffer of that size) must give the same.
	emit("slice", b, err)
	b, err = d.DecodeAll([]byte("pre"), data) // the same Decoder, after the first decode
	emit("pre", b, err)
	n, ok, err := zstd.ContentSize(data)
	var text []byte
	if err == nil {
		text = []byte("none")
		if ok {
			text = strconv.AppendUint(nil, n, 10)
		}
	}
	emit("size", text, err)
	b, err = zstd.NewDecoder().DecodeAllContext(cancelled, nil, data)
	emit("cancel", b, err)
}

// runCZI runs one czi input: [mode][pixel type][dst length u32 LE][payload]. The output is the dst
// (filled with 0xAA first), or nothing on an error: Go leaves dst undefined then.
func runCZI(emit emitFn, cancelled context.Context, data []byte) {
	b, err := cziRun(czi.NewDecoder(), context.Background(), data)
	emit("run", b, err)
	if errors.Is(err, errBadInput) {
		return
	}
	b, err = cziRun(czi.NewDecoder(), cancelled, data)
	emit("cancel", b, err)
}

func cziOnce(d *czi.Decoder, data []byte) ([]byte, error) {
	return cziRun(d, context.Background(), data)
}

func cziRun(d *czi.Decoder, ctx context.Context, data []byte) ([]byte, error) {
	if len(data) < 6 || binary.LittleEndian.Uint32(data[2:]) > maxDst {
		return nil, errBadInput
	}
	mode, pt, n := int(data[0]), int(data[1]), int(binary.LittleEndian.Uint32(data[2:]))
	dst := bytes.Repeat([]byte{0xAA}, n)
	if err := d.Decode(ctx, dst, data[6:], mode, pt); err != nil {
		return nil, err
	}
	return dst, nil
}

// rng is xorshift64: the inputs must not depend on the Go version.
type rng uint64

func (r *rng) next() uint64 {
	x := uint64(*r)
	x ^= x << 13
	x ^= x >> 7
	x ^= x << 17
	*r = rng(x)
	return x
}

func (r *rng) intn(n int) int { return int(r.next() >> 1 % uint64(n)) }

func (r *rng) bytes(n int) []byte {
	b := make([]byte, n)
	for i := range b {
		b[i] = byte(r.next() >> 24)
	}
	return b
}

func seed(name string) *rng {
	h := fnv.New64a()
	h.Write([]byte(name))
	r := rng(h.Sum64() | 1)
	return &r
}

// xxh64 is XXH64 with seed 0, for the checksum of the frames built here.
func xxh64(b []byte) uint64 {
	var p1, p2, p3, p4, p5 uint64 = 11400714785074694791, 14029467366897019727, 1609587929392839161, 9650029242287828579, 2870177450012600261
	round := func(acc, in uint64) uint64 {
		acc += in * p2
		return bits.RotateLeft64(acc, 31) * p1
	}
	merge := func(acc, v uint64) uint64 {
		acc ^= round(0, v)
		return acc*p1 + p4
	}
	n := len(b)
	var h uint64
	if n >= 32 {
		v1, v2, v3, v4 := p1+p2, p2, uint64(0), -p1
		for ; len(b) >= 32; b = b[32:] {
			v1 = round(v1, binary.LittleEndian.Uint64(b))
			v2 = round(v2, binary.LittleEndian.Uint64(b[8:]))
			v3 = round(v3, binary.LittleEndian.Uint64(b[16:]))
			v4 = round(v4, binary.LittleEndian.Uint64(b[24:]))
		}
		h = bits.RotateLeft64(v1, 1) + bits.RotateLeft64(v2, 7) + bits.RotateLeft64(v3, 12) + bits.RotateLeft64(v4, 18)
		h = merge(h, v1)
		h = merge(h, v2)
		h = merge(h, v3)
		h = merge(h, v4)
	} else {
		h = p5
	}
	h += uint64(n)
	for ; len(b) >= 8; b = b[8:] {
		h ^= round(0, binary.LittleEndian.Uint64(b))
		h = bits.RotateLeft64(h, 27)*p1 + p4
	}
	if len(b) >= 4 {
		h ^= uint64(binary.LittleEndian.Uint32(b)) * p1
		h = bits.RotateLeft64(h, 23)*p2 + p3
		b = b[4:]
	}
	for _, c := range b {
		h ^= uint64(c) * p5
		h = bits.RotateLeft64(h, 11) * p1
	}
	h ^= h >> 33
	h *= p2
	h ^= h >> 29
	h *= p3
	h ^= h >> 32
	return h
}

// blk is a raw block (data) or an RLE block (n copies of b).
type blk struct {
	rle  bool
	b    byte
	n    int
	data []byte
}

// build writes a frame of raw and RLE blocks with a 128 KiB window, as the Go tests' rawFrame.
func build(blks []blk, fcs, cks bool, dictID byte) []byte {
	var content []byte
	for _, k := range blks {
		if k.rle {
			content = append(content, bytes.Repeat([]byte{k.b}, k.n)...)
		} else {
			content = append(content, k.data...)
		}
	}
	f := []byte{0x28, 0xB5, 0x2F, 0xFD}
	fhd := byte(0)
	if fcs {
		fhd |= 3 << 6 // 8-byte content size
	}
	if cks {
		fhd |= 4
	}
	if dictID != 0 {
		fhd |= 1
	}
	f = append(f, fhd, 17<<3-10<<3) // window 128 KiB
	if dictID != 0 {
		f = append(f, dictID)
	}
	if fcs {
		f = binary.LittleEndian.AppendUint64(f, uint64(len(content)))
	}
	for i, k := range blks {
		last := uint32(0)
		if i == len(blks)-1 {
			last = 1
		}
		if k.rle {
			bh := uint32(k.n)<<3 | 1<<1 | last
			f = append(f, byte(bh), byte(bh>>8), byte(bh>>16), k.b)
		} else {
			bh := uint32(len(k.data))<<3 | last
			f = append(f, byte(bh), byte(bh>>8), byte(bh>>16))
			f = append(f, k.data...)
		}
	}
	if cks {
		f = binary.LittleEndian.AppendUint32(f, uint32(xxh64(content)))
	}
	return f
}

// rawFrame is a frame of raw blocks of at most 128 KiB.
func rawFrame(data []byte, fcs, cks bool, dictID byte) []byte {
	var blks []blk
	for first := true; first || len(data) > 0; first = false {
		n := min(len(data), 128<<10)
		blks = append(blks, blk{data: data[:n]})
		data = data[n:]
	}
	return build(blks, fcs, cks, dictID)
}

// periodFrame is a frame with a content size and one compressed block, built by hand (the oracle
// has no compressor): n sequences of p literals (p at most 15) and a match of 1027 to 2050 bytes
// at offset p, the overlapping match copy for p below 16. The three tables are in RLE mode (no
// state bits), so every sequence reads the same extra bits: the offset's, then the match
// length's, laid out from the end mark down.
func periodFrame(p, n int, lits []byte) []byte {
	const mlCode, mlBits, mlBase = 46, 10, 0x403
	mlx := uint64(p * 37 % (1 << mlBits))
	ofCode := bits.Len(uint(p+3)) - 1 // offset value p + 3: offset p
	ofx := uint64(p + 3 - 1<<ofCode)
	w := ofCode + mlBits
	chunk := ofx<<mlBits | mlx
	stream := make([]byte, n*w/8+1)
	set := func(at int, v uint64, nb int) {
		for j := 0; j < nb; j++ {
			if v>>j&1 != 0 {
				stream[(at+j)/8] |= 1 << ((at + j) % 8)
			}
		}
	}
	for k := 0; k < n; k++ {
		set(k*w, chunk, w)
	}
	set(n*w, 1, 1) // the end mark
	l := len(lits)
	blk := []byte{byte(l<<4) | 0b0100, byte(l >> 4)} // raw literals, 12-bit size
	blk = append(blk, lits...)
	blk = append(blk, byte(n), 0x54, byte(p), byte(ofCode), mlCode) // RLE codes: ll p, of, ml
	blk = append(blk, stream...)
	f := []byte{0x28, 0xB5, 0x2F, 0xFD, 0xA0} // single segment, 4-byte content size
	f = binary.LittleEndian.AppendUint32(f, uint32(n*(p+mlBase+int(mlx))))
	bh := uint32(len(blk))<<3 | 2<<1 | 1
	f = append(f, byte(bh), byte(bh>>8), byte(bh>>16))
	return append(f, blk...)
}

// rleFrame is a frame of RLE blocks: n copies of c.
func rleFrame(c byte, n int, fcs, cks bool) []byte {
	var blks []blk
	for first := true; first || n > 0; first = false {
		k := min(n, 128<<10)
		blks = append(blks, blk{rle: true, b: c, n: k})
		n -= k
	}
	return build(blks, fcs, cks, 0)
}

type variant struct {
	tag  string
	data []byte
}

// frameHeaderSize is the size of the frame header of s, or -1.
func frameHeaderSize(s []byte) int {
	if len(s) < 6 {
		return -1
	}
	fhd := s[4]
	single := fhd&0x20 != 0
	n := 5 + [4]int{0, 1, 2, 4}[fhd&3]
	if !single {
		n++
	}
	switch fhd >> 6 {
	case 0:
		if single {
			n++
		}
	case 1:
		n += 2
	case 2:
		n += 4
	default:
		n += 8
	}
	return n
}

// mutate returns truncations, bit flips, byte sets and frame header, block header and checksum
// edits of the frame s.
func mutate(s []byte, r *rng) []variant {
	var vs []variant
	n := len(s)
	edit := func(tag string, f func(b []byte)) {
		b := bytes.Clone(s)
		f(b)
		vs = append(vs, variant{tag, b})
	}
	seen := map[int]bool{}
	for _, c := range []int{0, 1, 3, 4, 5, 6, 8, 10, n / 3, n / 2, n - 5, n - 3, n - 1} {
		if c >= 0 && c < n && !seen[c] {
			seen[c] = true
			vs = append(vs, variant{fmt.Sprintf("t%d", c), s[:c]})
		}
	}
	for i := 0; n > 0 && i < 16; i++ {
		p := r.intn(n * 8)
		if i < 6 {
			p = r.intn(min(n, 16) * 8) // the frame header
		}
		edit(fmt.Sprintf("f%d", p), func(b []byte) { b[p/8] ^= 1 << (p % 8) })
	}
	// Byte sets to 0x00, 0xff and a random value.
	for i := 0; n > 0 && i < 9; i++ {
		p, v := r.intn(n), []byte{0x00, 0xff, byte(r.next() >> 24)}[i%3]
		edit(fmt.Sprintf("s%d-%02x", p, v), func(b []byte) { b[p] = v })
	}
	vs = append(vs, variant{"app3", append(bytes.Clone(s), 0xde, 0xad, 0xbe)})
	vs = append(vs, variant{"app5", append(bytes.Clone(s), 0, 0, 0, 0, 0)})
	if n < 6 {
		return vs
	}
	for _, v := range []byte{0x00, 0x01, 0x02, 0x03, 0x04, 0x08, 0x20, 0x24, 0x40, 0x44, 0x80, 0xc0, 0xc4, 0xe0, 0xff} {
		edit(fmt.Sprintf("h%02x", v), func(b []byte) { b[4] = v })
	}
	for _, v := range []byte{0x00, 0x08, 0x50, 0x58, 0x78, 0xf8} {
		edit(fmt.Sprintf("w%02x", v), func(b []byte) { b[5] = v })
	}
	edit("magic", func(b []byte) { b[0] ^= 1 })
	edit("skippable", func(b []byte) { copy(b, []byte{0x50, 0x2a, 0x4d, 0x18}) })
	if o := frameHeaderSize(s); o >= 0 && o+3 <= n {
		for t := byte(0); t < 4; t++ {
			edit(fmt.Sprintf("bt%d", t), func(b []byte) { b[o] = b[o]&^6 | t<<1 })
		}
		edit("blast", func(b []byte) { b[o] ^= 1 })
		edit("bsize", func(b []byte) { b[o+1] ^= 0x10 })
	}
	edit("cksum", func(b []byte) { b[n-1] ^= 1 })
	return vs
}

// mutateBig is mutate for a frame over 64 KiB, whose outputs are large: a capped set, with the
// positions anywhere in the frame (later blocks, the long fast paths) and not only in its head.
func mutateBig(s []byte, r *rng) []variant {
	var vs []variant
	n := len(s)
	edit := func(tag string, f func(b []byte)) {
		b := bytes.Clone(s)
		f(b)
		vs = append(vs, variant{tag, b})
	}
	for _, c := range []int{n / 4, n / 2, 3 * n / 4, n - 5, n - 1} {
		vs = append(vs, variant{fmt.Sprintf("t%d", c), s[:c]})
	}
	for i := 0; i < 8; i++ {
		p := 8*16 + r.intn((n-16)*8) // past the frame header
		edit(fmt.Sprintf("f%d", p), func(b []byte) { b[p/8] ^= 1 << (p % 8) })
	}
	for i := 0; i < 6; i++ {
		p, v := 16+r.intn(n-16), []byte{0x00, 0xff, byte(r.next() >> 24)}[i%3]
		edit(fmt.Sprintf("s%d-%02x", p, v), func(b []byte) { b[p] = v })
	}
	vs = append(vs, variant{"app3", append(bytes.Clone(s), 0xde, 0xad, 0xbe)})
	edit("cksum", func(b []byte) { b[n-1] ^= 1 })
	if o := frameHeaderSize(s); o >= 0 && o+3 <= n {
		edit("blast", func(b []byte) { b[o] ^= 1 })
		edit("bsize", func(b []byte) { b[o+1] ^= 0x10 })
	}
	return vs
}

type named struct {
	tag  string
	data []byte
}

// frameCases are the hand-written frames of the Go tests (TestFramesAndErrors, TestDocFrames,
// TestOneShotParity, TestLyingContentSize, TestContentSize and the seeds of FuzzDecodeAll).
func frameCases() []named {
	data := bytes.Repeat([]byte("zstd-go "), 40000) // 320000 bytes, 3 blocks
	cs := rawFrame(data, true, true, 0)
	skip := []byte{0x5A, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 1, 2, 3}
	var multi []byte
	for _, f := range [][]byte{rawFrame(data, false, false, 0), skip, cs, rawFrame(nil, true, false, 0)} {
		multi = append(multi, f...)
	}
	bad := bytes.Clone(cs)
	bad[len(bad)-1] ^= 1
	window := rawFrame(data, false, false, 0)
	window[5] = (28 - 10) << 3 // 256 MiB without a content size
	lying := make([]byte, 256<<10)
	copy(lying, []byte{0x28, 0xB5, 0x2F, 0xFD, 0xC0, 0})
	binary.LittleEndian.PutUint64(lying[6:], uint64(len(lying)/4)*128<<10)
	cases := []named{
		{"multi", multi},
		{"checksum", bad},
		{"dictionary", rawFrame(data, true, false, 7)},
		{"window", window},
		{"truncated", cs[:len(cs)-1]},
		{"trailing", append(bytes.Clone(cs), 1, 2)},
		{"garbage", []byte("not a zstd frame")},
		{"skippable-truncated", skip[:10]},
		{"skippable", skip},
		{"skippable-header", []byte{0x50, 0x2A, 0x4D, 0x18, 0xFF, 0, 0, 0}},
		{"garbage6", []byte{1, 2, 3, 4, 5, 6}},
		{"empty", nil},
		{"lying-fcs", lying},
		// Output of 140 MiB and 120 MiB with a 256 MiB limit: Go charges each reallocation, so
		// the first fails with the memory limit and the second, just under, decodes.
		{"rle-memory-limit", rleFrame(0, 1120<<17, true, false)},
		{"rle-under-limit", rleFrame(0, 960<<17, true, false)},
		// A one-byte stream frame, then 8.5 MiB of skippable frame: the size guess is 32 times
		// the input, 272 MiB, over the 256 MiB limit, so Go fails with the memory limit although
		// the output is one byte.
		{"hint-memory-limit", append(append(mustHex("28b52ffd003809000041"), 0x50, 0x2a, 0x4d, 0x18, 0, 0, 0x88, 0), make([]byte, 8<<20+512<<10)...)},
	}
	for name, h := range map[string]string{
		"stream-block-over-window":     "28b52ffd00004d00000861015401022e0010",
		"os-window-2^28-small-fcs":     "28b52ffd4090000003080061",
		"os-window-2^31-small-fcs":     "28b52ffd40a8e802431f0007",
		"os-rle-above-window":          "28b52ffd400001030b200061",
		"os-rle-0-in-fcs-0":            "28b52ffd200003000005",
		"errata-0lit-0seq":             "28b52ffd20001500000000",
		"errata-rle-first-block":       "28b52ffda001000200020010000b000000",
		"errata-tiny-fse":              "28b52ffd2027c5000080f3f1f0ecebc6c5c7f09d43000000e0e006580100603e52",
		"errata-magicless":             "27b52ffd0003190000666f6f3fbac459",
		"permissive-truncated-huffman": "28b52ffd000055000072800104207e1f02aa00",
		"permissive-offset-0":          "28b52ffd0000450000080002002f430bae",
		"fuzz-seed-a":                  "28b52ffd20001500000000",
		"fuzz-seed-b":                  "28b52ffd2027c5000080f3f1f0ecebc6c5c7f09d43000000e0e006580100603e52",
		"fuzz-seed-c":                  "28b52ffd000055000072800104207e1f02aa00",
		"fuzz-seed-d":                  "28b52ffd0000450000080002002f430bae",
	} {
		b, err := hex.DecodeString(h)
		if err != nil {
			panic(name + ": " + err.Error())
		}
		cases = append(cases, named{name, b})
	}
	return cases
}

func mustHex(h string) []byte {
	b, err := hex.DecodeString(h)
	if err != nil {
		panic(err)
	}
	return b
}

// synthetic is the Go tests' synthetic(): raw, RLE, short-offset and long-distance data, from the
// oracle's own generator.
func synthetic(r *rng) []named {
	random := r.bytes(200 << 10)
	words := strings.Fields("tile slide pixel stain tissue zoom level jpeg zstd frame block the of and a")
	var text bytes.Buffer
	for text.Len() < 300<<10 {
		text.WriteString(words[r.intn(len(words))])
		text.WriteByte(" \n"[r.intn(8)/7])
	}
	// A smooth 512x512 RGB gradient with a little noise.
	img := make([]byte, 0, 512*512*3)
	for y := 0; y < 512; y++ {
		for x := 0; x < 512; x++ {
			img = append(img, byte(x/2+r.intn(3)), byte(y/2), byte((x+y)/4+r.intn(2)))
		}
	}
	// Periods 1..15. As raw blocks here they reach no match copy; the compressed short periods
	// are periodFrame's.
	var short []byte
	for len(short) < 100<<10 {
		p := 1 + r.intn(15)
		for i := 20 + r.intn(200); i > 0; i-- {
			short = append(short, random[:p]...)
		}
		short = append(short, byte(r.next()))
	}
	var far []byte
	for i := 0; i < 3; i++ {
		far = append(far, random...)
		far = append(far, text.Bytes()...)
	}
	return []named{
		{"empty", nil}, {"one", []byte{'a'}}, {"random", random}, {"text", text.Bytes()},
		{"image", img}, {"short-period", short}, {"far-repeats", far},
	}
}

// image16 is a smooth 16-bit image with noise in the low byte.
func image16(words int, r *rng) []byte {
	img := make([]byte, 2*words)
	for i := 0; i < words; i++ {
		v := uint16(i/7%4096) + uint16(r.intn(64))
		img[2*i], img[2*i+1] = byte(v), byte(v>>8)
	}
	return img
}

// unpack splits 16-bit words into a low plane then a high plane, as libCZI's encoder does.
func unpack(img []byte) []byte {
	half := len(img) / 2
	p := make([]byte, 2*half)
	for i := 0; i < half; i++ {
		p[i], p[half+i] = img[2*i], img[2*i+1]
	}
	return p
}

// cziInput is a czi input: [mode][pixel type][dst length u32 LE][payload].
func cziInput(mode, pt, n int, payload []byte) []byte {
	b := []byte{byte(mode), byte(pt)}
	b = binary.LittleEndian.AppendUint32(b, uint32(n))
	return append(b, payload...)
}

// randFrame is a frame with a random header and random blocks: any block type, sizes mostly
// within the bytes given.
func randFrame(r *rng) []byte {
	f := []byte{0x28, 0xB5, 0x2F, 0xFD}
	fhd := byte(r.next() >> 24)
	if r.intn(16) != 0 {
		fhd &^= 0x08 // reserved bit
	}
	if r.intn(4) != 0 {
		fhd &^= 3 // no dictionary
	}
	f = append(f, fhd)
	single := fhd&0x20 != 0
	if !single {
		f = append(f, byte(r.intn(0x60)))
	}
	f = append(f, r.bytes([4]int{0, 1, 2, 4}[fhd&3])...)
	fcs := [4]int{0, 2, 4, 8}[fhd>>6]
	if fhd>>6 == 0 && single {
		fcs = 1
	}
	b := r.bytes(fcs)
	for i := 4; i < len(b); i++ {
		b[i] = 0 // keep the 8-byte sizes small
	}
	if len(b) > 2 {
		b[2] &= 0x0f
	}
	f = append(f, b...)
	blocks := 1 + r.intn(3)
	for i := 0; i < blocks; i++ {
		typ := r.intn(4)
		size := 1 + r.intn(200)
		body := size
		switch typ {
		case 1:
			body = 1
			size = 1 + r.intn(1000)
		case 3:
			size = r.intn(8)
			body = size
		}
		bh := uint32(size)<<3 | uint32(typ)<<1
		if i == blocks-1 {
			bh |= 1
		}
		f = append(f, byte(bh), byte(bh>>8), byte(bh>>16))
		f = append(f, r.bytes(body)...)
	}
	if fhd&4 != 0 {
		f = append(f, r.bytes(4)...)
	}
	return f
}

// generate writes the fixtures (every .zst under <testdata>), their mutations, the hand-written
// and synthetic frames, random frames, and czi inputs.
func generate(testdata, out string) error {
	if err := os.MkdirAll(out, 0o755); err != nil {
		return err
	}
	put := func(name string, b []byte) error { return os.WriteFile(filepath.Join(out, name), b, 0o644) }
	var fixtures []string
	err := filepath.WalkDir(testdata, func(p string, d fs.DirEntry, err error) error {
		if err == nil && !d.IsDir() && strings.HasSuffix(p, ".zst") {
			fixtures = append(fixtures, p)
		}
		return err
	})
	if err != nil {
		return err
	}
	for _, p := range fixtures {
		s, err := os.ReadFile(p)
		if err != nil {
			return err
		}
		rel, _ := filepath.Rel(testdata, p)
		id := "fx-" + strings.ReplaceAll(strings.TrimSuffix(rel, ".zst"), "/", "_")
		if err := put(id+".zst", s); err != nil {
			return err
		}
		mutants := mutate
		if len(s) > 64<<10 {
			mutants = mutateBig
		}
		for _, v := range mutants(s, seed(rel)) {
			if err := put(id+"~"+v.tag+".zst", v.data); err != nil {
				return err
			}
		}
		// The fixture as a czi payload of every mode, with a destination that fits, is cut and is
		// zero filled. A frame without a content size gets one destination.
		fcs, ok, err := zstd.ContentSize(s)
		if err != nil || !ok || fcs > maxDst {
			fcs = 1000
		}
		even := int(fcs) + int(fcs)&1
		type cz struct {
			tag      string
			mode, pt int
			hdr      []byte
			n        int
		}
		cs := []cz{
			{"z0", czi.Zstd0, czi.Gray16, nil, int(fcs)},
			{"z1h", czi.Zstd1, czi.Gray16, []byte{3, 1, 1}, even},
		}
		cs = append(cs,
			cz{"z0-cut", czi.Zstd0, czi.Gray16, nil, int(fcs) / 2},
			cz{"z0-pad", czi.Zstd0, czi.Gray16, nil, int(fcs) + 7},
			cz{"z1s", czi.Zstd1, czi.Gray16, []byte{1}, int(fcs)},
			cz{"z1p", czi.Zstd1, 3, []byte{3, 1, 0}, int(fcs)},
			cz{"z1h-cut", czi.Zstd1, czi.Bgr48, []byte{3, 1, 0xff}, even / 2 &^ 1},
			cz{"z1h-pad", czi.Zstd1, czi.Gray16, []byte{3, 1, 1}, even + 6},
			cz{"z1h-gray8", czi.Zstd1, 0, []byte{3, 1, 1}, even},
		)
		for _, c := range cs {
			if err := put(id+"-"+c.tag+".czi", cziInput(c.mode, c.pt, c.n, append(bytes.Clone(c.hdr), s...))); err != nil {
				return err
			}
		}
	}
	// Multi-frame inputs: 2 to 4 tiles of at most 512x512 back to back, a skippable frame between
	// some, each with mutateBig's variants, so an error in a later frame is compared too. The
	// output of an input that decodes fits the room and slice operations' buffers, where the Rust
	// oracle decodes the frames on several threads; zstd-go decodes them one after another. A
	// variant that fails decodes serially on both sides (its room is 128 KiB): the parallel
	// path's errors are checked against the serial loop by tests/frames_parallel.rs and the
	// frames fuzz target.
	var tiles [][]byte
	for _, p := range fixtures {
		if filepath.Base(filepath.Dir(p)) != "tiles" || strings.Contains(p, "_1024_") {
			continue
		}
		s, err := os.ReadFile(p)
		if err != nil {
			return err
		}
		tiles = append(tiles, s)
	}
	mr := seed("multi-frame")
	for i := 0; len(tiles) > 0 && i < 6; i++ {
		var mf []byte
		for k, n := 0, 2+mr.intn(3); k < n; k++ {
			if k > 0 && mr.intn(2) == 0 {
				mf = append(mf, 0x50, 0x2a, 0x4d, 0x18, 3, 0, 0, 0, 1, 2, 3)
			}
			mf = append(mf, tiles[mr.intn(len(tiles))]...)
		}
		id := fmt.Sprintf("mf-%d", i)
		if err := put(id+".zst", mf); err != nil {
			return err
		}
		for _, v := range mutateBig(mf, seed(id)) {
			if err := put(id+"~"+v.tag+".zst", v.data); err != nil {
				return err
			}
		}
	}
	for _, c := range frameCases() {
		if err := put("fr-"+c.tag+".zst", c.data); err != nil {
			return err
		}
	}
	// synthetic(): raw frames with and without a content size and checksum; the large ones in two.
	r := seed("synthetic")
	for _, c := range synthetic(r) {
		for i := 0; i < 4; i++ {
			fcs, cks := i&1 != 0, i&2 != 0
			if len(c.data) > 500<<10 && i != 0 && i != 3 {
				continue
			}
			if err := put(fmt.Sprintf("syn-%s-f%dc%d.zst", c.tag, i&1, i>>1), rawFrame(c.data, fcs, cks, 0)); err != nil {
				return err
			}
		}
	}
	for i := 0; i < 4; i++ { // zeros as RLE blocks, and the RLE and raw blocks mixed
		if err := put(fmt.Sprintf("syn-zeros-f%dc%d.zst", i&1, i>>1), rleFrame(0, 1<<20, i&1 != 0, i&2 != 0)); err != nil {
			return err
		}
		mix := []blk{{data: []byte("abc")}, {rle: true, b: 'x', n: 1000}, {data: []byte("def")}, {rle: true, b: 0, n: 128 << 10}, {rle: true, b: 7, n: 5}}
		if err := put(fmt.Sprintf("syn-mixed-f%dc%d.zst", i&1, i>>1), build(mix, i&1 != 0, i&2 != 0, 0)); err != nil {
			return err
		}
	}
	// Short periods as compressed blocks: offsets 1 to 15 and long matches.
	for p := 1; p <= 15; p++ {
		if err := put(fmt.Sprintf("syn-period-%02d.zst", p), periodFrame(p, 40, r.bytes(40*p))); err != nil {
			return err
		}
	}
	// Random frames (the analogue of the random-stream tests of the other ports).
	rr := seed("random-frames")
	for i := 0; i < 3000; i++ {
		if err := put(fmt.Sprintf("rf-%04d.zst", i), randFrame(rr)); err != nil {
			return err
		}
	}
	// czi/czi_test.go: TestSynthetic and TestErrors, on raw-block frames, and mutations of the
	// small frame of TestErrors under every header.
	cr := seed("czi")
	img := image16(3*300*257, cr) // Bgr48 300x257, or Gray16 900x257
	plain := rawFrame(img, true, false, 0)
	hl := rawFrame(unpack(img), true, false, 0)
	cat := func(h []byte, f []byte) []byte { return append(bytes.Clone(h), f...) }
	for _, c := range []struct {
		name     string
		src      []byte
		mode, pt int
		dstLen   int
	}{
		{"zstd0", plain, czi.Zstd0, czi.Gray16, len(img)},
		{"zstd1-short-header", cat([]byte{1}, plain), czi.Zstd1, czi.Gray16, len(img)},
		{"zstd1-plain", cat([]byte{3, 1, 0}, plain), czi.Zstd1, 3, len(img)},
		{"zstd1-hilo-gray16", cat([]byte{3, 1, 1}, hl), czi.Zstd1, czi.Gray16, len(img)},
		{"zstd1-hilo-bgr48", cat([]byte{3, 1, 0xFF}, hl), czi.Zstd1, czi.Bgr48, len(img)},
		{"zstd0-fcs-short", plain, czi.Zstd0, czi.Gray16, len(img) + 1000},
		{"zstd0-fcs-long", plain, czi.Zstd0, czi.Gray16, len(img) - 1000},
		{"hilo-fcs-short", cat([]byte{3, 1, 1}, hl), czi.Zstd1, czi.Gray16, len(img) + 1000},
		{"hilo-fcs-long", cat([]byte{3, 1, 1}, hl), czi.Zstd1, czi.Gray16, len(img) - 1000},
	} {
		if err := put("czi-"+c.name+".czi", cziInput(c.mode, c.pt, c.dstLen, c.src)); err != nil {
			return err
		}
	}
	frame := rawFrame(image16(100, cr), true, false, 0)
	noSize := rawFrame(image16(100, cr), false, false, 0)
	two := append(bytes.Clone(frame), frame...)
	for _, c := range []struct {
		name     string
		src      []byte
		mode, pt int
		n        int
	}{
		{"mode", frame, 4, czi.Gray16, 200},
		{"header-empty", nil, czi.Zstd1, czi.Gray16, 200},
		{"header-only", []byte{1}, czi.Zstd1, czi.Gray16, 200},
		{"header-3-only", []byte{3, 1, 1}, czi.Zstd1, czi.Gray16, 200},
		{"header-bad-chunk", cat([]byte{3, 2, 1}, frame), czi.Zstd1, czi.Gray16, 200},
		{"header-size", cat([]byte{5, 1, 1}, frame), czi.Zstd1, czi.Gray16, 200},
		{"hilo-gray8", cat([]byte{3, 1, 1}, frame), czi.Zstd1, 0, 200},
		{"hilo-odd-size", cat([]byte{3, 1, 1}, frame), czi.Zstd1, czi.Gray16, 201},
		{"no-content-size", noSize, czi.Zstd0, czi.Gray16, 200},
		{"two-frames", two, czi.Zstd0, czi.Gray16, 200},
		{"garbage", []byte{1, 2, 3, 4, 5, 6}, czi.Zstd0, czi.Gray16, 200},
	} {
		if err := put("czi-err-"+c.name+".czi", cziInput(c.mode, c.pt, c.n, c.src)); err != nil {
			return err
		}
	}
	// Payloads that grow czi's scratch buffer. "zoverflow": a first frame that fits dst (2.12 MB)
	// and a second of 63 times that: Go grows a copy of dst from its capacity and charges 126
	// times, 267 MB of the 256 MiB budget, then fails with a size mismatch (corrupt), where
	// charging the first frame again from 0 would pass the budget. "reuse-a" decodes a 100 MiB
	// frame and fails in the next, "reuse-b" (named to follow it) needs 140 MiB: Go's decoder
	// keeps the buffer of the frames that decoded, so b grows once from 100 MiB, not from 0 (which
	// would pass the budget).
	n0 := 2120000
	over := append(rleFrame(0, n0, true, false), rleFrame(0, 63*n0, true, false)...)
	late := append(rleFrame(0, 100<<20, true, false), 0x28, 0xB5, 0x2F, 0xFD, 0x20, 16, 7, 0, 0)
	for _, c := range []struct {
		name     string
		src      []byte
		mode, pt int
		n        int
	}{
		{"zoverflow-z0", over, czi.Zstd0, czi.Gray16, n0},
		{"zoverflow-z1h", cat([]byte{3, 1, 1}, over), czi.Zstd1, czi.Gray16, n0},
		{"reuse-a-fail-late", late, czi.Zstd0, czi.Gray16, 16},
		{"reuse-b-big", rleFrame(0, 1120<<17, true, false), czi.Zstd0, czi.Gray16, 16},
	} {
		if err := put("czi-"+c.name+".czi", cziInput(c.mode, c.pt, c.n, c.src)); err != nil {
			return err
		}
	}
	for _, h := range []struct {
		tag      string
		hdr      []byte
		mode, pt int
	}{{"z0", nil, czi.Zstd0, czi.Gray16}, {"z1s", []byte{1}, czi.Zstd1, czi.Gray16}, {"z1h", []byte{3, 1, 1}, czi.Zstd1, czi.Gray16}} {
		for _, v := range mutate(frame, seed("czi-"+h.tag)) {
			if err := put(fmt.Sprintf("czi-mut-%s~%s.czi", h.tag, v.tag), cziInput(h.mode, h.pt, 200, cat(h.hdr, v.data))); err != nil {
				return err
			}
		}
	}
	return nil
}

// benchDir prints name, output bytes and ns per decode for each tile (a frame of <format>_<size>_
// <content>_z<level>[_hilo].zst), and for each hi/lo tile again as a czi zstd1 payload (named
// czi-<name>). Go reuses its Decoder and a pre-sized dst, as BenchmarkDecodeAll does.
func benchDir(dir string, n int) error {
	files, err := filepath.Glob(filepath.Join(dir, "*.zst"))
	if err != nil {
		return err
	}
	d := zstd.NewDecoder()
	cd := czi.NewDecoder()
	for _, p := range files {
		src, err := os.ReadFile(p)
		if err != nil {
			return err
		}
		name := strings.TrimSuffix(filepath.Base(p), ".zst")
		size, ok, err := zstd.ContentSize(src)
		if err != nil || !ok {
			continue
		}
		dst := make([]byte, 0, size)
		buf, err := d.DecodeAll(dst, src)
		if err != nil {
			return fmt.Errorf("%s: %w", name, err)
		}
		// An input of several frames (mf-*) outgrows the first frame's size: reuse what it grew.
		dst = buf[:0]
		t := time.Now()
		for i := 0; i < n; i++ {
			if buf, err = d.DecodeAll(dst[:0], src); err != nil {
				return err
			}
		}
		fmt.Printf("%s\t%d\t%d\n", name, len(buf), time.Since(t).Nanoseconds()/int64(n))
		if !strings.HasSuffix(name, "_hilo") {
			continue
		}
		payload := append([]byte{3, 1, 1}, src...)
		out := make([]byte, size)
		if err := cd.Decode(context.Background(), out, payload, czi.Zstd1, czi.Gray16); err != nil {
			return fmt.Errorf("czi-%s: %w", name, err)
		}
		t = time.Now()
		for i := 0; i < n; i++ {
			if err := cd.Decode(context.Background(), out, payload, czi.Zstd1, czi.Gray16); err != nil {
				return err
			}
		}
		fmt.Printf("czi-%s\t%d\t%d\n", name, len(out), time.Since(t).Nanoseconds()/int64(n))
	}
	return nil
}
