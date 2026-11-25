package zstd

import "math/bits"

const (
	prime1    uint64 = 11400714785074694791
	prime2    uint64 = 14029467366897019727
	prime3    uint64 = 1609587929392839161
	prime4    uint64 = 9650029242287828579
	prime5    uint64 = 2870177450012600261
	negPrime1        = 7046029288634856825 // -prime1 mod 2^64
)

func xxRound(acc, in uint64) uint64 {
	return bits.RotateLeft64(acc+in*prime2, 31) * prime1
}

func xxMerge(acc, v uint64) uint64 {
	return (acc^xxRound(0, v))*prime1 + prime4
}

// xxh64 is XXH64 with seed 0, the zstd frame checksum.
func xxh64(b []byte) uint64 {
	n := len(b)
	var h uint64
	if n >= 32 {
		v1, v2, v3, v4 := prime1, prime2, uint64(0), uint64(negPrime1)
		v1 += prime2
		for ; len(b) >= 32; b = b[32:] {
			v1 = xxRound(v1, le64(b))
			v2 = xxRound(v2, le64(b[8:]))
			v3 = xxRound(v3, le64(b[16:]))
			v4 = xxRound(v4, le64(b[24:]))
		}
		h = bits.RotateLeft64(v1, 1) + bits.RotateLeft64(v2, 7) +
			bits.RotateLeft64(v3, 12) + bits.RotateLeft64(v4, 18)
		h = xxMerge(h, v1)
		h = xxMerge(h, v2)
		h = xxMerge(h, v3)
		h = xxMerge(h, v4)
	} else {
		h = prime5
	}
	h += uint64(n)
	for ; len(b) >= 8; b = b[8:] {
		h ^= xxRound(0, le64(b))
		h = bits.RotateLeft64(h, 27)*prime1 + prime4
	}
	if len(b) >= 4 {
		h ^= uint64(le32(b)) * prime1
		h = bits.RotateLeft64(h, 23)*prime2 + prime3
		b = b[4:]
	}
	for _, c := range b {
		h ^= uint64(c) * prime5
		h = bits.RotateLeft64(h, 11) * prime1
	}
	h ^= h >> 33
	h *= prime2
	h ^= h >> 29
	h *= prime3
	h ^= h >> 32
	return h
}
