#!/usr/bin/env python3
"""Decodes corrupted variants of corpus files with symphonia-play and reports panics, aborts, and hangs.

Usage: corrupt_check.py [name-regex] [variants-per-file]
"""
import os, random, re, subprocess, sys, tempfile

root = os.environ.get("CORPUS", "tmp/corpus")
play = os.environ.get("PLAY_BIN", "target/release/symphonia-play")
pattern = re.compile(sys.argv[1] if len(sys.argv) > 1 else ".")
variants = int(sys.argv[2]) if len(sys.argv) > 2 else 6
rng = random.Random(int(os.environ.get("SEED", "1234")))

files = []
for sub in ("synth", "real", "fate_aac"):
    d = os.path.join(root, sub)
    if os.path.isdir(d):
        files += [os.path.join(d, f) for f in sorted(os.listdir(d)) if pattern.search(f) and not f.endswith(".txt")]

# One file per (codec, container) family keeps the runtime reasonable.
seen, picked = set(), []
for f in files:
    family = re.sub(r"\d+", "", os.path.basename(f).split(".")[0].rsplit("_", 1)[0]) + os.path.splitext(f)[1]
    if family not in seen and os.path.getsize(f) < 3_000_000:
        seen.add(family)
        picked.append(f)

bad = []
with tempfile.TemporaryDirectory() as tmp:
    for f in picked:
        data = open(f, "rb").read()
        for v in range(variants):
            buf = bytearray(data)
            kind = v % 3
            if kind == 0:
                for _ in range(max(1, len(buf) // 2000)):
                    buf[rng.randrange(len(buf))] = rng.randrange(256)
            elif kind == 1:
                buf = buf[: rng.randrange(1, len(buf))]
            else:
                start = rng.randrange(len(buf))
                for i in range(start, min(len(buf), start + 64)):
                    buf[i] = rng.randrange(256)
            path = os.path.join(tmp, "c" + os.path.splitext(f)[1])
            open(path, "wb").write(buf)
            try:
                p = subprocess.run([play, "--decode-only", "--no-progress", path], capture_output=True, timeout=int(os.environ.get("TIMEOUT", "60")))
                rc = p.returncode
            except subprocess.TimeoutExpired:
                rc = "timeout"
            if rc == "timeout" or (isinstance(rc, int) and (rc == 101 or rc < 0)):
                err = p.stderr.decode(errors="replace").splitlines()[-3:] if rc != "timeout" else ["timeout"]
                bad.append((os.path.basename(f), v, rc, " | ".join(err)[:200]))
                keep = os.path.join(root, "crashes")
                os.makedirs(keep, exist_ok=True)
                open(os.path.join(keep, f"{os.path.basename(f)}.v{v}{os.path.splitext(f)[1]}"), "wb").write(buf)

print(f"checked {len(picked)} families x {variants} variants")
for b in bad:
    print("PROBLEM", *b)
print("problems:", len(bad))
