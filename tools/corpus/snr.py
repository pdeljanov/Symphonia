#!/usr/bin/env python3
"""Usage: snr.py target.f32 reference.f32 [max_lag]. Prints the best alignment lag (in interleaved samples) and the SNR in dB."""
import array, math, sys

def load(p):
    a = array.array('f')
    with open(p, 'rb') as f:
        a.frombytes(f.read())
    return a

t, r = load(sys.argv[1]), load(sys.argv[2])
max_lag = int(sys.argv[3]) if len(sys.argv) > 3 else 0

def snr(t, r, lag):
    t0, r0 = (lag, 0) if lag >= 0 else (0, -lag)
    n = min(len(t) - t0, len(r) - r0)
    sig = err = 0.0
    for i in range(n):
        x = r[r0 + i]
        d = t[t0 + i] - x
        sig += x * x
        err += d * d
    return 10 * math.log10(sig / err) if err > 0 and sig > 0 else float('inf')

best = max(((snr(t, r, lag), lag) for lag in range(-max_lag, max_lag + 1)), key=lambda x: x[0])
print(f"len_t={len(t)} len_r={len(r)} best_lag={best[1]} snr_db={best[0]:.1f}")
