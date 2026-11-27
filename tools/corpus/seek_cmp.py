#!/usr/bin/env python3
"""Usage: seek_cmp.py target.f32 reference.f32 start_frame channels. Compares the seeked output with the reference output starting at `start_frame`. Prints the SNR in dB and the number of compared frames."""
import array, math, sys

def load(p):
    a = array.array('f')
    with open(p, 'rb') as f:
        a.frombytes(f.read())
    return a

t, r = load(sys.argv[1]), load(sys.argv[2])
start, ch = int(sys.argv[3]), int(sys.argv[4])
# Lossy codecs need some frames to warm up after a seek.
WARMUP = 4096
r = r[(start + WARMUP) * ch:]
t = t[WARMUP * ch:]
n = min(len(t), len(r))
if n == 0:
    print("compared=0 snr_db=-inf")
    sys.exit(0)
sig = err = 0.0
for i in range(n):
    x = r[i]
    d = t[i] - x
    sig += x * x
    err += d * d
snr = 10 * math.log10(sig / err) if err > 0 and sig > 0 else float('inf')
print(f"compared={n // ch} len_t={len(t) // ch} len_r={len(r) // ch} snr_db={snr:.1f}")
