#!/usr/bin/env python3

import argparse
import subprocess
import os
import glob
from urllib.parse import urlparse

def download_format(fmt, urls):
    for i, url in enumerate(filter(lambda url: url.endswith(fmt), urls)):
        out = os.path.basename(urlparse(url).path)
        if os.path.exists(out):
            print(f"{out} already downloaded")
        else:
            print(f"Downloading {out}")
            subprocess.call(['curl', '-Lo', out, url])

def benchmark_file(filepath, ffmpeg, symphonia):
    subprocess.check_call(['hyperfine', '-m', '20', f'{ffmpeg} -threads 1 -benchmark -v 0 -i {filepath} -f null -', f'{symphonia} --decode-only {filepath}'])

def benchmark_format(fmt, ffmpeg, symphonia):
    for filepath in glob.iglob(f'*.{fmt}'):
        benchmark_file(filepath, ffmpeg, symphonia)

parser = argparse.ArgumentParser(description='Benchtest symphonia-play against ffmpeg')
parser.add_argument('formats', nargs='*', default=['flac', 'mp3', 'wav'], help='formats to test')
parser.add_argument('-f', '--ffmpeg', default='ffmpeg', help='Path to ffmpeg executable')
parser.add_argument('-s', '--symphonia', default='symphonia-play', help='Path to symphonia-play executable')
args = parser.parse_args()

urls = [
    'https://archive.org/download/MLKDream/MLKDream.flac',
    'https://archive.org/download/MLKDream/MLKDream.mp3',
    'https://archive.org/download/MLKDream/MLKDream.wav',
    'https://archive.org/download/kdtu2015-01-07.cmc641.flac24/kdtu2015-01-07.cmc641-t01.flac',
    'https://archive.org/download/kdtu2015-01-07.cmc641.flac24/kdtu2015-01-07.cmc641-t01.mp3',
    'https://archive.org/download/tsp1993-08-07.flac16/tsp1993-08-07d2t01.flac',
    'https://archive.org/download/tsp1993-08-07.flac16/tsp1993-08-07d2t01.mp3',
    'https://archive.org/download/gds2004-10-16.matrix.flac/gds10-16-2004d2t10.flac',
    'https://archive.org/download/gds2004-10-16.matrix.flac/gds10-16-2004d2t10.mp3',
    'https://archive.org/download/tsp1998-06-01.flac16/tsp1998-06-01t02.flac',
    'https://archive.org/download/tsp1998-06-01.flac16/tsp1998-06-01t02.mp3',
    'https://archive.org/download/videogamemusic_201806/11-WelcomeStrangermainTitleextendedopllYm241324bit96khz.wav',
    'https://archive.org/download/videogamemusic_201806/11-WelcomeStrangermainTitleextendedopllYm241324bit96khz.flac',
]

for fmt in args.formats:
    download_format(fmt, urls)
    benchmark_format(fmt, args.ffmpeg, args.symphonia)

