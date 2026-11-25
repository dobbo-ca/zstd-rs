//go:build !purego

package zstd

func cpuid(eax, ecx uint32) (a, b, c, d uint32)

// cpuOK reports BMI1 and BMI2, which the amd64 kernels use.
func cpuOK() bool {
	if max, _, _, _ := cpuid(0, 0); max < 7 {
		return false
	}
	_, b, _, _ := cpuid(7, 0)
	return b&(1<<3) != 0 && b&(1<<8) != 0
}
