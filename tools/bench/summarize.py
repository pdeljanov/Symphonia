#!/usr/bin/env python3
"""Prints, for every file, the mean decode time of ffmpeg and symphonia-play and their ratio (below 1 means symphonia is faster).

The "open" columns are runs that do not decode (ffmpeg -c copy, symphonia-play --probe-only); they only show the process start up and the demuxing cost, and are not comparable between the two programs.
"""
import glob, json, os, sys

d = sys.argv[1]
rows = []
for p in sorted(glob.glob(os.path.join(d, "res_*.json"))):
    name = os.path.basename(p)[4:-5]
    by = {x["command"]: x["mean"] for x in json.load(open(p))["results"]}
    f, s = by["ffmpeg"], by["symphonia"]
    rows.append((s / f, name, f, s, by["ffmpeg-demux"], by["symphonia-probe"]))
rows.sort(reverse=True)
print(f"{'file':18} {'ffmpeg ms':>9} {'symph ms':>9} {'ratio':>6} | {'ffmpeg open':>11} {'symph open':>10}")
for ratio, name, f, s, fo, so in rows:
    print(f"{name:18} {f*1000:9.0f} {s*1000:9.0f} {ratio:6.2f} | {fo*1000:11.0f} {so*1000:10.0f}")
failed = os.path.join(d, "failed.txt")
if os.path.exists(failed):
    print("not decodable by symphonia-play:", ", ".join(open(failed).read().split()))
