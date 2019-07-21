#!/usr/bin/env python3

# Sonata IO BitStream Reader Huffman Table Generator
# Copyright (c) 2019 The Sonata Project Developers.
#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import collections
import random
import sys

MPEG_HUFFCODES_1 = [
    0x0001, 0x0001, 0x0001, 0x0000,
    ]

MPEG_HUFFBITS_1 = [
    1,  3,  2,  3,
    ]

MPEG_HUFFCODES_2 = [
    0x0001, 0x0002, 0x0001, 0x0003, 0x0001, 0x0001, 0x0003, 0x0002,
    0x0000,
    ]

MPEG_HUFFBITS_2 = [
    1,  3,  6,  3,  3,  5,  5,  5,
    6,
    ]

MPEG_HUFFCODES_3 = [
    0x0003, 0x0002, 0x0001, 0x0001, 0x0001, 0x0001, 0x0003, 0x0002,
    0x0000,
    ]

MPEG_HUFFBITS_3 = [
    2,  2,  6,  3,  2,  5,  5,  5,
    6,
    ]

MPEG_HUFFCODES_5 = [
    0x0001, 0x0002, 0x0006, 0x0005, 0x0003, 0x0001, 0x0004, 0x0004,
    0x0007, 0x0005, 0x0007, 0x0001, 0x0006, 0x0001, 0x0001, 0x0000,
    ]

MPEG_HUFFBITS_5 = [
    1,  3,  6,  7,  3,  3,  6,  7,
    6,  6,  7,  8,  7,  6,  7,  8,
    ]

MPEG_HUFFCODES_6 = [
    0x0007, 0x0003, 0x0005, 0x0001, 0x0006, 0x0002, 0x0003, 0x0002,
    0x0005, 0x0004, 0x0004, 0x0001, 0x0003, 0x0003, 0x0002, 0x0000,
    ]

MPEG_HUFFBITS_6 = [
    3,  3,  5,  7,  3,  2,  4,  5,
    4,  4,  5,  6,  6,  5,  6,  7,
    ]

MPEG_HUFFCODES_7 = [
    0x0001, 0x0002, 0x000a, 0x0013, 0x0010, 0x000a, 0x0003, 0x0003,
    0x0007, 0x000a, 0x0005, 0x0003, 0x000b, 0x0004, 0x000d, 0x0011,
    0x0008, 0x0004, 0x000c, 0x000b, 0x0012, 0x000f, 0x000b, 0x0002,
    0x0007, 0x0006, 0x0009, 0x000e, 0x0003, 0x0001, 0x0006, 0x0004,
    0x0005, 0x0003, 0x0002, 0x0000,
    ]

MPEG_HUFFBITS_7 = [
    1,  3,  6,  8,  8,  9,  3,  4,
    6,  7,  7,  8,  6,  5,  7,  8,
    8,  9,  7,  7,  8,  9,  9,  9,
    7,  7,  8,  9,  9, 10,  8,  8,
    9, 10, 10, 10,
    ]

MPEG_HUFFCODES_8 = [
    0x0003, 0x0004, 0x0006, 0x0012, 0x000c, 0x0005, 0x0005, 0x0001,
    0x0002, 0x0010, 0x0009, 0x0003, 0x0007, 0x0003, 0x0005, 0x000e,
    0x0007, 0x0003, 0x0013, 0x0011, 0x000f, 0x000d, 0x000a, 0x0004,
    0x000d, 0x0005, 0x0008, 0x000b, 0x0005, 0x0001, 0x000c, 0x0004,
    0x0004, 0x0001, 0x0001, 0x0000,
    ]

MPEG_HUFFBITS_8 = [
    2,  3,  6,  8,  8,  9,  3,  2,
    4,  8,  8,  8,  6,  4,  6,  8,
    8,  9,  8,  8,  8,  9,  9, 10,
    8,  7,  8,  9, 10, 10,  9,  8,
    9,  9, 11, 11,
    ]

MPEG_HUFFCODES_9 = [
    0x0007, 0x0005, 0x0009, 0x000e, 0x000f, 0x0007, 0x0006, 0x0004,
    0x0005, 0x0005, 0x0006, 0x0007, 0x0007, 0x0006, 0x0008, 0x0008,
    0x0008, 0x0005, 0x000f, 0x0006, 0x0009, 0x000a, 0x0005, 0x0001,
    0x000b, 0x0007, 0x0009, 0x0006, 0x0004, 0x0001, 0x000e, 0x0004,
    0x0006, 0x0002, 0x0006, 0x0000,
    ]

MPEG_HUFFBITS_9 = [
    3,  3,  5,  6,  8,  9,  3,  3,
    4,  5,  6,  8,  4,  4,  5,  6,
    7,  8,  6,  5,  6,  7,  7,  8,
    7,  6,  7,  7,  8,  9,  8,  7,
    8,  8,  9,  9,
    ]

MPEG_HUFFCODES_10 = [
    0x0001, 0x0002, 0x000a, 0x0017, 0x0023, 0x001e, 0x000c, 0x0011,
    0x0003, 0x0003, 0x0008, 0x000c, 0x0012, 0x0015, 0x000c, 0x0007,
    0x000b, 0x0009, 0x000f, 0x0015, 0x0020, 0x0028, 0x0013, 0x0006,
    0x000e, 0x000d, 0x0016, 0x0022, 0x002e, 0x0017, 0x0012, 0x0007,
    0x0014, 0x0013, 0x0021, 0x002f, 0x001b, 0x0016, 0x0009, 0x0003,
    0x001f, 0x0016, 0x0029, 0x001a, 0x0015, 0x0014, 0x0005, 0x0003,
    0x000e, 0x000d, 0x000a, 0x000b, 0x0010, 0x0006, 0x0005, 0x0001,
    0x0009, 0x0008, 0x0007, 0x0008, 0x0004, 0x0004, 0x0002, 0x0000,
    ]

MPEG_HUFFBITS_10 = [
    1,  3,  6,  8,  9,  9,  9, 10,
    3,  4,  6,  7,  8,  9,  8,  8,
    6,  6,  7,  8,  9, 10,  9,  9,
    7,  7,  8,  9, 10, 10,  9, 10,
    8,  8,  9, 10, 10, 10, 10, 10,
    9,  9, 10, 10, 11, 11, 10, 11,
    8,  8,  9, 10, 10, 10, 11, 11,
    9,  8,  9, 10, 10, 11, 11, 11,
    ]

MPEG_HUFFCODES_11 = [
    0x0003, 0x0004, 0x000a, 0x0018, 0x0022, 0x0021, 0x0015, 0x000f,
    0x0005, 0x0003, 0x0004, 0x000a, 0x0020, 0x0011, 0x000b, 0x000a,
    0x000b, 0x0007, 0x000d, 0x0012, 0x001e, 0x001f, 0x0014, 0x0005,
    0x0019, 0x000b, 0x0013, 0x003b, 0x001b, 0x0012, 0x000c, 0x0005,
    0x0023, 0x0021, 0x001f, 0x003a, 0x001e, 0x0010, 0x0007, 0x0005,
    0x001c, 0x001a, 0x0020, 0x0013, 0x0011, 0x000f, 0x0008, 0x000e,
    0x000e, 0x000c, 0x0009, 0x000d, 0x000e, 0x0009, 0x0004, 0x0001,
    0x000b, 0x0004, 0x0006, 0x0006, 0x0006, 0x0003, 0x0002, 0x0000,
    ]

MPEG_HUFFBITS_11 = [
    2,  3,  5,  7,  8,  9,  8,  9,
    3,  3,  4,  6,  8,  8,  7,  8,
    5,  5,  6,  7,  8,  9,  8,  8,
    7,  6,  7,  9,  8, 10,  8,  9,
    8,  8,  8,  9,  9, 10,  9, 10,
    8,  8,  9, 10, 10, 11, 10, 11,
    8,  7,  7,  8,  9, 10, 10, 10,
    8,  7,  8,  9, 10, 10, 10, 10,
    ]

MPEG_HUFFCODES_12 = [
    0x0009, 0x0006, 0x0010, 0x0021, 0x0029, 0x0027, 0x0026, 0x001a,
    0x0007, 0x0005, 0x0006, 0x0009, 0x0017, 0x0010, 0x001a, 0x000b,
    0x0011, 0x0007, 0x000b, 0x000e, 0x0015, 0x001e, 0x000a, 0x0007,
    0x0011, 0x000a, 0x000f, 0x000c, 0x0012, 0x001c, 0x000e, 0x0005,
    0x0020, 0x000d, 0x0016, 0x0013, 0x0012, 0x0010, 0x0009, 0x0005,
    0x0028, 0x0011, 0x001f, 0x001d, 0x0011, 0x000d, 0x0004, 0x0002,
    0x001b, 0x000c, 0x000b, 0x000f, 0x000a, 0x0007, 0x0004, 0x0001,
    0x001b, 0x000c, 0x0008, 0x000c, 0x0006, 0x0003, 0x0001, 0x0000,
    ]

MPEG_HUFFBITS_12 = [
    4,  3,  5,  7,  8,  9,  9,  9,
    3,  3,  4,  5,  7,  7,  8,  8,
    5,  4,  5,  6,  7,  8,  7,  8,
    6,  5,  6,  6,  7,  8,  8,  8,
    7,  6,  7,  7,  8,  8,  8,  9,
    8,  7,  8,  8,  8,  9,  8,  9,
    8,  7,  7,  8,  8,  9,  9, 10,
    9,  8,  8,  9,  9,  9,  9, 10,
    ]

MPEG_HUFFCODES_13 = [
    0x0001, 0x0005, 0x000e, 0x0015, 0x0022, 0x0033, 0x002e, 0x0047,
    0x002a, 0x0034, 0x0044, 0x0034, 0x0043, 0x002c, 0x002b, 0x0013,
    0x0003, 0x0004, 0x000c, 0x0013, 0x001f, 0x001a, 0x002c, 0x0021,
    0x001f, 0x0018, 0x0020, 0x0018, 0x001f, 0x0023, 0x0016, 0x000e,
    0x000f, 0x000d, 0x0017, 0x0024, 0x003b, 0x0031, 0x004d, 0x0041,
    0x001d, 0x0028, 0x001e, 0x0028, 0x001b, 0x0021, 0x002a, 0x0010,
    0x0016, 0x0014, 0x0025, 0x003d, 0x0038, 0x004f, 0x0049, 0x0040,
    0x002b, 0x004c, 0x0038, 0x0025, 0x001a, 0x001f, 0x0019, 0x000e,
    0x0023, 0x0010, 0x003c, 0x0039, 0x0061, 0x004b, 0x0072, 0x005b,
    0x0036, 0x0049, 0x0037, 0x0029, 0x0030, 0x0035, 0x0017, 0x0018,
    0x003a, 0x001b, 0x0032, 0x0060, 0x004c, 0x0046, 0x005d, 0x0054,
    0x004d, 0x003a, 0x004f, 0x001d, 0x004a, 0x0031, 0x0029, 0x0011,
    0x002f, 0x002d, 0x004e, 0x004a, 0x0073, 0x005e, 0x005a, 0x004f,
    0x0045, 0x0053, 0x0047, 0x0032, 0x003b, 0x0026, 0x0024, 0x000f,
    0x0048, 0x0022, 0x0038, 0x005f, 0x005c, 0x0055, 0x005b, 0x005a,
    0x0056, 0x0049, 0x004d, 0x0041, 0x0033, 0x002c, 0x002b, 0x002a,
    0x002b, 0x0014, 0x001e, 0x002c, 0x0037, 0x004e, 0x0048, 0x0057,
    0x004e, 0x003d, 0x002e, 0x0036, 0x0025, 0x001e, 0x0014, 0x0010,
    0x0035, 0x0019, 0x0029, 0x0025, 0x002c, 0x003b, 0x0036, 0x0051,
    0x0042, 0x004c, 0x0039, 0x0036, 0x0025, 0x0012, 0x0027, 0x000b,
    0x0023, 0x0021, 0x001f, 0x0039, 0x002a, 0x0052, 0x0048, 0x0050,
    0x002f, 0x003a, 0x0037, 0x0015, 0x0016, 0x001a, 0x0026, 0x0016,
    0x0035, 0x0019, 0x0017, 0x0026, 0x0046, 0x003c, 0x0033, 0x0024,
    0x0037, 0x001a, 0x0022, 0x0017, 0x001b, 0x000e, 0x0009, 0x0007,
    0x0022, 0x0020, 0x001c, 0x0027, 0x0031, 0x004b, 0x001e, 0x0034,
    0x0030, 0x0028, 0x0034, 0x001c, 0x0012, 0x0011, 0x0009, 0x0005,
    0x002d, 0x0015, 0x0022, 0x0040, 0x0038, 0x0032, 0x0031, 0x002d,
    0x001f, 0x0013, 0x000c, 0x000f, 0x000a, 0x0007, 0x0006, 0x0003,
    0x0030, 0x0017, 0x0014, 0x0027, 0x0024, 0x0023, 0x0035, 0x0015,
    0x0010, 0x0017, 0x000d, 0x000a, 0x0006, 0x0001, 0x0004, 0x0002,
    0x0010, 0x000f, 0x0011, 0x001b, 0x0019, 0x0014, 0x001d, 0x000b,
    0x0011, 0x000c, 0x0010, 0x0008, 0x0001, 0x0001, 0x0000, 0x0001,
    ]

MPEG_HUFFBITS_13 = [
    1,  4,  6,  7,  8,  9,  9, 10,
    9, 10, 11, 11, 12, 12, 13, 13,
    3,  4,  6,  7,  8,  8,  9,  9,
    9,  9, 10, 10, 11, 12, 12, 12,
    6,  6,  7,  8,  9,  9, 10, 10,
    9, 10, 10, 11, 11, 12, 13, 13,
    7,  7,  8,  9,  9, 10, 10, 10,
    10, 11, 11, 11, 11, 12, 13, 13,
    8,  7,  9,  9, 10, 10, 11, 11,
    10, 11, 11, 12, 12, 13, 13, 14,
    9,  8,  9, 10, 10, 10, 11, 11,
    11, 11, 12, 11, 13, 13, 14, 14,
    9,  9, 10, 10, 11, 11, 11, 11,
    11, 12, 12, 12, 13, 13, 14, 14,
    10,  9, 10, 11, 11, 11, 12, 12,
    12, 12, 13, 13, 13, 14, 16, 16,
    9,  8,  9, 10, 10, 11, 11, 12,
    12, 12, 12, 13, 13, 14, 15, 15,
    10,  9, 10, 10, 11, 11, 11, 13,
    12, 13, 13, 14, 14, 14, 16, 15,
    10, 10, 10, 11, 11, 12, 12, 13,
    12, 13, 14, 13, 14, 15, 16, 17,
    11, 10, 10, 11, 12, 12, 12, 12,
    13, 13, 13, 14, 15, 15, 15, 16,
    11, 11, 11, 12, 12, 13, 12, 13,
    14, 14, 15, 15, 15, 16, 16, 16,
    12, 11, 12, 13, 13, 13, 14, 14,
    14, 14, 14, 15, 16, 15, 16, 16,
    13, 12, 12, 13, 13, 13, 15, 14,
    14, 17, 15, 15, 15, 17, 16, 16,
    12, 12, 13, 14, 14, 14, 15, 14,
    15, 15, 16, 16, 19, 18, 19, 16,
    ]

MPEG_HUFFCODES_15 = [
    0x0007, 0x000c, 0x0012, 0x0035, 0x002f, 0x004c, 0x007c, 0x006c,
    0x0059, 0x007b, 0x006c, 0x0077, 0x006b, 0x0051, 0x007a, 0x003f,
    0x000d, 0x0005, 0x0010, 0x001b, 0x002e, 0x0024, 0x003d, 0x0033,
    0x002a, 0x0046, 0x0034, 0x0053, 0x0041, 0x0029, 0x003b, 0x0024,
    0x0013, 0x0011, 0x000f, 0x0018, 0x0029, 0x0022, 0x003b, 0x0030,
    0x0028, 0x0040, 0x0032, 0x004e, 0x003e, 0x0050, 0x0038, 0x0021,
    0x001d, 0x001c, 0x0019, 0x002b, 0x0027, 0x003f, 0x0037, 0x005d,
    0x004c, 0x003b, 0x005d, 0x0048, 0x0036, 0x004b, 0x0032, 0x001d,
    0x0034, 0x0016, 0x002a, 0x0028, 0x0043, 0x0039, 0x005f, 0x004f,
    0x0048, 0x0039, 0x0059, 0x0045, 0x0031, 0x0042, 0x002e, 0x001b,
    0x004d, 0x0025, 0x0023, 0x0042, 0x003a, 0x0034, 0x005b, 0x004a,
    0x003e, 0x0030, 0x004f, 0x003f, 0x005a, 0x003e, 0x0028, 0x0026,
    0x007d, 0x0020, 0x003c, 0x0038, 0x0032, 0x005c, 0x004e, 0x0041,
    0x0037, 0x0057, 0x0047, 0x0033, 0x0049, 0x0033, 0x0046, 0x001e,
    0x006d, 0x0035, 0x0031, 0x005e, 0x0058, 0x004b, 0x0042, 0x007a,
    0x005b, 0x0049, 0x0038, 0x002a, 0x0040, 0x002c, 0x0015, 0x0019,
    0x005a, 0x002b, 0x0029, 0x004d, 0x0049, 0x003f, 0x0038, 0x005c,
    0x004d, 0x0042, 0x002f, 0x0043, 0x0030, 0x0035, 0x0024, 0x0014,
    0x0047, 0x0022, 0x0043, 0x003c, 0x003a, 0x0031, 0x0058, 0x004c,
    0x0043, 0x006a, 0x0047, 0x0036, 0x0026, 0x0027, 0x0017, 0x000f,
    0x006d, 0x0035, 0x0033, 0x002f, 0x005a, 0x0052, 0x003a, 0x0039,
    0x0030, 0x0048, 0x0039, 0x0029, 0x0017, 0x001b, 0x003e, 0x0009,
    0x0056, 0x002a, 0x0028, 0x0025, 0x0046, 0x0040, 0x0034, 0x002b,
    0x0046, 0x0037, 0x002a, 0x0019, 0x001d, 0x0012, 0x000b, 0x000b,
    0x0076, 0x0044, 0x001e, 0x0037, 0x0032, 0x002e, 0x004a, 0x0041,
    0x0031, 0x0027, 0x0018, 0x0010, 0x0016, 0x000d, 0x000e, 0x0007,
    0x005b, 0x002c, 0x0027, 0x0026, 0x0022, 0x003f, 0x0034, 0x002d,
    0x001f, 0x0034, 0x001c, 0x0013, 0x000e, 0x0008, 0x0009, 0x0003,
    0x007b, 0x003c, 0x003a, 0x0035, 0x002f, 0x002b, 0x0020, 0x0016,
    0x0025, 0x0018, 0x0011, 0x000c, 0x000f, 0x000a, 0x0002, 0x0001,
    0x0047, 0x0025, 0x0022, 0x001e, 0x001c, 0x0014, 0x0011, 0x001a,
    0x0015, 0x0010, 0x000a, 0x0006, 0x0008, 0x0006, 0x0002, 0x0000,
    ]

MPEG_HUFFBITS_15 = [
    3,  4,  5,  7,  7,  8,  9,  9,
    9, 10, 10, 11, 11, 11, 12, 13,
    4,  3,  5,  6,  7,  7,  8,  8,
    8,  9,  9, 10, 10, 10, 11, 11,
    5,  5,  5,  6,  7,  7,  8,  8,
    8,  9,  9, 10, 10, 11, 11, 11,
    6,  6,  6,  7,  7,  8,  8,  9,
    9,  9, 10, 10, 10, 11, 11, 11,
    7,  6,  7,  7,  8,  8,  9,  9,
    9,  9, 10, 10, 10, 11, 11, 11,
    8,  7,  7,  8,  8,  8,  9,  9,
    9,  9, 10, 10, 11, 11, 11, 12,
    9,  7,  8,  8,  8,  9,  9,  9,
    9, 10, 10, 10, 11, 11, 12, 12,
    9,  8,  8,  9,  9,  9,  9, 10,
    10, 10, 10, 10, 11, 11, 11, 12,
    9,  8,  8,  9,  9,  9,  9, 10,
    10, 10, 10, 11, 11, 12, 12, 12,
    9,  8,  9,  9,  9,  9, 10, 10,
    10, 11, 11, 11, 11, 12, 12, 12,
    10,  9,  9,  9, 10, 10, 10, 10,
    10, 11, 11, 11, 11, 12, 13, 12,
    10,  9,  9,  9, 10, 10, 10, 10,
    11, 11, 11, 11, 12, 12, 12, 13,
    11, 10,  9, 10, 10, 10, 11, 11,
    11, 11, 11, 11, 12, 12, 13, 13,
    11, 10, 10, 10, 10, 11, 11, 11,
    11, 12, 12, 12, 12, 12, 13, 13,
    12, 11, 11, 11, 11, 11, 11, 11,
    12, 12, 12, 12, 13, 13, 12, 13,
    12, 11, 11, 11, 11, 11, 11, 12,
    12, 12, 12, 12, 13, 13, 13, 13,
    ]

MPEG_HUFFCODES_16 = [
    0x0001, 0x0005, 0x000e, 0x002c, 0x004a, 0x003f, 0x006e, 0x005d,
    0x00ac, 0x0095, 0x008a, 0x00f2, 0x00e1, 0x00c3, 0x0178, 0x0011,
    0x0003, 0x0004, 0x000c, 0x0014, 0x0023, 0x003e, 0x0035, 0x002f,
    0x0053, 0x004b, 0x0044, 0x0077, 0x00c9, 0x006b, 0x00cf, 0x0009,
    0x000f, 0x000d, 0x0017, 0x0026, 0x0043, 0x003a, 0x0067, 0x005a,
    0x00a1, 0x0048, 0x007f, 0x0075, 0x006e, 0x00d1, 0x00ce, 0x0010,
    0x002d, 0x0015, 0x0027, 0x0045, 0x0040, 0x0072, 0x0063, 0x0057,
    0x009e, 0x008c, 0x00fc, 0x00d4, 0x00c7, 0x0183, 0x016d, 0x001a,
    0x004b, 0x0024, 0x0044, 0x0041, 0x0073, 0x0065, 0x00b3, 0x00a4,
    0x009b, 0x0108, 0x00f6, 0x00e2, 0x018b, 0x017e, 0x016a, 0x0009,
    0x0042, 0x001e, 0x003b, 0x0038, 0x0066, 0x00b9, 0x00ad, 0x0109,
    0x008e, 0x00fd, 0x00e8, 0x0190, 0x0184, 0x017a, 0x01bd, 0x0010,
    0x006f, 0x0036, 0x0034, 0x0064, 0x00b8, 0x00b2, 0x00a0, 0x0085,
    0x0101, 0x00f4, 0x00e4, 0x00d9, 0x0181, 0x016e, 0x02cb, 0x000a,
    0x0062, 0x0030, 0x005b, 0x0058, 0x00a5, 0x009d, 0x0094, 0x0105,
    0x00f8, 0x0197, 0x018d, 0x0174, 0x017c, 0x0379, 0x0374, 0x0008,
    0x0055, 0x0054, 0x0051, 0x009f, 0x009c, 0x008f, 0x0104, 0x00f9,
    0x01ab, 0x0191, 0x0188, 0x017f, 0x02d7, 0x02c9, 0x02c4, 0x0007,
    0x009a, 0x004c, 0x0049, 0x008d, 0x0083, 0x0100, 0x00f5, 0x01aa,
    0x0196, 0x018a, 0x0180, 0x02df, 0x0167, 0x02c6, 0x0160, 0x000b,
    0x008b, 0x0081, 0x0043, 0x007d, 0x00f7, 0x00e9, 0x00e5, 0x00db,
    0x0189, 0x02e7, 0x02e1, 0x02d0, 0x0375, 0x0372, 0x01b7, 0x0004,
    0x00f3, 0x0078, 0x0076, 0x0073, 0x00e3, 0x00df, 0x018c, 0x02ea,
    0x02e6, 0x02e0, 0x02d1, 0x02c8, 0x02c2, 0x00df, 0x01b4, 0x0006,
    0x00ca, 0x00e0, 0x00de, 0x00da, 0x00d8, 0x0185, 0x0182, 0x017d,
    0x016c, 0x0378, 0x01bb, 0x02c3, 0x01b8, 0x01b5, 0x06c0, 0x0004,
    0x02eb, 0x00d3, 0x00d2, 0x00d0, 0x0172, 0x017b, 0x02de, 0x02d3,
    0x02ca, 0x06c7, 0x0373, 0x036d, 0x036c, 0x0d83, 0x0361, 0x0002,
    0x0179, 0x0171, 0x0066, 0x00bb, 0x02d6, 0x02d2, 0x0166, 0x02c7,
    0x02c5, 0x0362, 0x06c6, 0x0367, 0x0d82, 0x0366, 0x01b2, 0x0000,
    0x000c, 0x000a, 0x0007, 0x000b, 0x000a, 0x0011, 0x000b, 0x0009,
    0x000d, 0x000c, 0x000a, 0x0007, 0x0005, 0x0003, 0x0001, 0x0003,
    ]

MPEG_HUFFBITS_16 = [
    1,  4,  6,  8,  9,  9, 10, 10,
    11, 11, 11, 12, 12, 12, 13,  9,
    3,  4,  6,  7,  8,  9,  9,  9,
    10, 10, 10, 11, 12, 11, 12,  8,
    6,  6,  7,  8,  9,  9, 10, 10,
    11, 10, 11, 11, 11, 12, 12,  9,
    8,  7,  8,  9,  9, 10, 10, 10,
    11, 11, 12, 12, 12, 13, 13, 10,
    9,  8,  9,  9, 10, 10, 11, 11,
    11, 12, 12, 12, 13, 13, 13,  9,
    9,  8,  9,  9, 10, 11, 11, 12,
    11, 12, 12, 13, 13, 13, 14, 10,
    10,  9,  9, 10, 11, 11, 11, 11,
    12, 12, 12, 12, 13, 13, 14, 10,
    10,  9, 10, 10, 11, 11, 11, 12,
    12, 13, 13, 13, 13, 15, 15, 10,
    10, 10, 10, 11, 11, 11, 12, 12,
    13, 13, 13, 13, 14, 14, 14, 10,
    11, 10, 10, 11, 11, 12, 12, 13,
    13, 13, 13, 14, 13, 14, 13, 11,
    11, 11, 10, 11, 12, 12, 12, 12,
    13, 14, 14, 14, 15, 15, 14, 10,
    12, 11, 11, 11, 12, 12, 13, 14,
    14, 14, 14, 14, 14, 13, 14, 11,
    12, 12, 12, 12, 12, 13, 13, 13,
    13, 15, 14, 14, 14, 14, 16, 11,
    14, 12, 12, 12, 13, 13, 14, 14,
    14, 16, 15, 15, 15, 17, 15, 11,
    13, 13, 11, 12, 14, 14, 13, 14,
    14, 15, 16, 15, 17, 15, 14, 11,
    9,  8,  8,  9,  9, 10, 10, 10,
    11, 11, 11, 11, 11, 11, 11,  8,
    ]

MPEG_HUFFCODES_24 = [
    0x000f, 0x000d, 0x002e, 0x0050, 0x0092, 0x0106, 0x00f8, 0x01b2,
    0x01aa, 0x029d, 0x028d, 0x0289, 0x026d, 0x0205, 0x0408, 0x0058,
    0x000e, 0x000c, 0x0015, 0x0026, 0x0047, 0x0082, 0x007a, 0x00d8,
    0x00d1, 0x00c6, 0x0147, 0x0159, 0x013f, 0x0129, 0x0117, 0x002a,
    0x002f, 0x0016, 0x0029, 0x004a, 0x0044, 0x0080, 0x0078, 0x00dd,
    0x00cf, 0x00c2, 0x00b6, 0x0154, 0x013b, 0x0127, 0x021d, 0x0012,
    0x0051, 0x0027, 0x004b, 0x0046, 0x0086, 0x007d, 0x0074, 0x00dc,
    0x00cc, 0x00be, 0x00b2, 0x0145, 0x0137, 0x0125, 0x010f, 0x0010,
    0x0093, 0x0048, 0x0045, 0x0087, 0x007f, 0x0076, 0x0070, 0x00d2,
    0x00c8, 0x00bc, 0x0160, 0x0143, 0x0132, 0x011d, 0x021c, 0x000e,
    0x0107, 0x0042, 0x0081, 0x007e, 0x0077, 0x0072, 0x00d6, 0x00ca,
    0x00c0, 0x00b4, 0x0155, 0x013d, 0x012d, 0x0119, 0x0106, 0x000c,
    0x00f9, 0x007b, 0x0079, 0x0075, 0x0071, 0x00d7, 0x00ce, 0x00c3,
    0x00b9, 0x015b, 0x014a, 0x0134, 0x0123, 0x0110, 0x0208, 0x000a,
    0x01b3, 0x0073, 0x006f, 0x006d, 0x00d3, 0x00cb, 0x00c4, 0x00bb,
    0x0161, 0x014c, 0x0139, 0x012a, 0x011b, 0x0213, 0x017d, 0x0011,
    0x01ab, 0x00d4, 0x00d0, 0x00cd, 0x00c9, 0x00c1, 0x00ba, 0x00b1,
    0x00a9, 0x0140, 0x012f, 0x011e, 0x010c, 0x0202, 0x0179, 0x0010,
    0x014f, 0x00c7, 0x00c5, 0x00bf, 0x00bd, 0x00b5, 0x00ae, 0x014d,
    0x0141, 0x0131, 0x0121, 0x0113, 0x0209, 0x017b, 0x0173, 0x000b,
    0x029c, 0x00b8, 0x00b7, 0x00b3, 0x00af, 0x0158, 0x014b, 0x013a,
    0x0130, 0x0122, 0x0115, 0x0212, 0x017f, 0x0175, 0x016e, 0x000a,
    0x028c, 0x015a, 0x00ab, 0x00a8, 0x00a4, 0x013e, 0x0135, 0x012b,
    0x011f, 0x0114, 0x0107, 0x0201, 0x0177, 0x0170, 0x016a, 0x0006,
    0x0288, 0x0142, 0x013c, 0x0138, 0x0133, 0x012e, 0x0124, 0x011c,
    0x010d, 0x0105, 0x0200, 0x0178, 0x0172, 0x016c, 0x0167, 0x0004,
    0x026c, 0x012c, 0x0128, 0x0126, 0x0120, 0x011a, 0x0111, 0x010a,
    0x0203, 0x017c, 0x0176, 0x0171, 0x016d, 0x0169, 0x0165, 0x0002,
    0x0409, 0x0118, 0x0116, 0x0112, 0x010b, 0x0108, 0x0103, 0x017e,
    0x017a, 0x0174, 0x016f, 0x016b, 0x0168, 0x0166, 0x0164, 0x0000,
    0x002b, 0x0014, 0x0013, 0x0011, 0x000f, 0x000d, 0x000b, 0x0009,
    0x0007, 0x0006, 0x0004, 0x0007, 0x0005, 0x0003, 0x0001, 0x0003,
    ]

MPEG_HUFFBITS_24 = [
    4,  4,  6,  7,  8,  9,  9, 10,
    10, 11, 11, 11, 11, 11, 12,  9,
    4,  4,  5,  6,  7,  8,  8,  9,
    9,  9, 10, 10, 10, 10, 10,  8,
    6,  5,  6,  7,  7,  8,  8,  9,
    9,  9,  9, 10, 10, 10, 11,  7,
    7,  6,  7,  7,  8,  8,  8,  9,
    9,  9,  9, 10, 10, 10, 10,  7,
    8,  7,  7,  8,  8,  8,  8,  9,
    9,  9, 10, 10, 10, 10, 11,  7,
    9,  7,  8,  8,  8,  8,  9,  9,
    9,  9, 10, 10, 10, 10, 10,  7,
    9,  8,  8,  8,  8,  9,  9,  9,
    9, 10, 10, 10, 10, 10, 11,  7,
    10,  8,  8,  8,  9,  9,  9,  9,
    10, 10, 10, 10, 10, 11, 11,  8,
    10,  9,  9,  9,  9,  9,  9,  9,
    9, 10, 10, 10, 10, 11, 11,  8,
    10,  9,  9,  9,  9,  9,  9, 10,
    10, 10, 10, 10, 11, 11, 11,  8,
    11,  9,  9,  9,  9, 10, 10, 10,
    10, 10, 10, 11, 11, 11, 11,  8,
    11, 10,  9,  9,  9, 10, 10, 10,
    10, 10, 10, 11, 11, 11, 11,  8,
    11, 10, 10, 10, 10, 10, 10, 10,
    10, 10, 11, 11, 11, 11, 11,  8,
    11, 10, 10, 10, 10, 10, 10, 10,
    11, 11, 11, 11, 11, 11, 11,  8,
    12, 10, 10, 10, 10, 10, 10, 11,
    11, 11, 11, 11, 11, 11, 11,  8,
    8,  7,  7,  7,  7,  7,  7,  7,
    7,  7,  7,  8,  8,  8,  8,  4,
    ]

MPEG_QUADS_HUFFCODES_A = [ 1, 5, 4, 5, 6, 5, 4, 4, 7, 3, 6, 0, 7, 2, 3, 1, ]
MPEG_QUADS_HUFFBITS_A  = [ 1, 4, 4, 5, 4, 6, 5, 6, 4, 5, 5, 6, 5, 6, 6, 6, ]

MPEG_QUADS_HUFFCODES_B = [ 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, ]
MPEG_QUADS_HUFFBITS_B  = [  4,  4,  4,  4,  4,  4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, ]

class MpegTable:
    def __init__(self, name, codes, code_lens, wrap):
        assert len(codes) == len(code_lens)

        self.name = name
        self.codes = codes
        self.code_lens = code_lens

        # Generate the actual data values that the Huffman codes encode.
        self.values = [ ((i // wrap) << 4) | (i % wrap) for i in range(0, len(codes)) ]

MPEG_TABLES = [
    MpegTable( "HUFFMAN_TABLE_1",  MPEG_HUFFCODES_1,  MPEG_HUFFBITS_1, 0x2 ),
    MpegTable( "HUFFMAN_TABLE_2",  MPEG_HUFFCODES_2,  MPEG_HUFFBITS_2, 0x3 ),
    MpegTable( "HUFFMAN_TABLE_3",  MPEG_HUFFCODES_3,  MPEG_HUFFBITS_3, 0x3 ),
    MpegTable( "HUFFMAN_TABLE_5",  MPEG_HUFFCODES_5,  MPEG_HUFFBITS_5, 0x4 ),
    MpegTable( "HUFFMAN_TABLE_6",  MPEG_HUFFCODES_6,  MPEG_HUFFBITS_6, 0x4 ),
    MpegTable( "HUFFMAN_TABLE_7",  MPEG_HUFFCODES_7,  MPEG_HUFFBITS_7, 0x6 ),
    MpegTable( "HUFFMAN_TABLE_8",  MPEG_HUFFCODES_8,  MPEG_HUFFBITS_8, 0x6 ),
    MpegTable( "HUFFMAN_TABLE_9",  MPEG_HUFFCODES_9,  MPEG_HUFFBITS_9, 0x6 ),
    MpegTable("HUFFMAN_TABLE_10", MPEG_HUFFCODES_10, MPEG_HUFFBITS_10, 0x8 ),
    MpegTable("HUFFMAN_TABLE_11", MPEG_HUFFCODES_11, MPEG_HUFFBITS_11, 0x8 ),
    MpegTable("HUFFMAN_TABLE_12", MPEG_HUFFCODES_12, MPEG_HUFFBITS_12, 0x8 ),
    MpegTable("HUFFMAN_TABLE_13", MPEG_HUFFCODES_13, MPEG_HUFFBITS_13, 0x10),
    MpegTable("HUFFMAN_TABLE_15", MPEG_HUFFCODES_15, MPEG_HUFFBITS_15, 0x10),
    MpegTable("HUFFMAN_TABLE_16", MPEG_HUFFCODES_16, MPEG_HUFFBITS_16, 0x10),
    MpegTable("HUFFMAN_TABLE_24", MPEG_HUFFCODES_24, MPEG_HUFFBITS_24, 0x10),
    MpegTable("QUADS_HUFFMAN_TABLE_A", MPEG_QUADS_HUFFCODES_A, MPEG_QUADS_HUFFBITS_A, 0x10),
    MpegTable("QUADS_HUFFMAN_TABLE_B", MPEG_QUADS_HUFFCODES_B, MPEG_QUADS_HUFFBITS_B, 0x10),
    ]

class Node:
    def __init__(self, prefix):
        self.prefix = prefix
        self.prefix_len_max = 0
        self.offset = 0
        self.values = []
        self.children = {}

def eprint(*args, **kwargs):
    print(*args, file=sys.stderr, **kwargs)

def print_tree(node, depth = 0):
    eprint("\t" * depth + "{:#0{w}b} @ {} +{}".format(
        node.prefix, node.offset, 1<<node.prefix_len_max, w=node.prefix_len_max + 2))

    depth = depth + 1
    for value in node.values:
        eprint("\t" * depth + "{:#0{w}b} => {:#x}".format(value[1], value[2], w=value[0] + 2))

    for child in sorted(node.children.keys()):
        print_tree(node.children[child], depth)

def synthesize(node, depth = 0):
    synthesized_nodes = [];

    synth_count = 0;

    for value in node.values:
        # A value in this node may have a prefix less than the longest prefix length in the node.
        # Calculate how many extra padding bits we must add to to this value's prefix.
        extra_bits = node.prefix_len_max - value[0]

        # Synthesize duplicates of this value for all combination of padding bits if there are any.
        if extra_bits > 0:
            # Pad the prefix with extra bits.
            prefix = value[1] << extra_bits

            # The number of values that need to by synthesized is 2^(number of padding bits). 
            count = 1 << extra_bits

            # However, the original value was not technically synthesized, so don't include it in
            # the actual count.
            synth_count += count - 1

            # Synthesize the values
            for i in range(count):
                entry = (value[0], prefix + i, value[2])
                synthesized_nodes.append(entry)
        else:
            synthesized_nodes.append(value)

    # Sort the synthesized values.
    synthesized_nodes.sort(key=lambda x: x[1])
    node.values = synthesized_nodes

    max_depth = depth

    # Descend into child nodes.
    for child in node.children:
        count, child_max_depth = synthesize(node.children[child], depth + 1)

        synth_count += count
        max_depth = max(max_depth, child_max_depth)

    return (synth_count, max_depth)

def assign_offsets_breadth(node):
    queue = collections.deque([node])

    count = 0

    while queue:
        node = queue.popleft()
        node.offset = count

        count = count + len(node.values) + len(node.children)

        for child in sorted(node.children.keys()):
            queue.append(node.children[child])

    return count

def generate_code(root, name, max_code_len):
    print("pub const {}: HuffmanTable<H8> = HuffmanTable {{".format(name))
    print("    data: &[")

    queue = collections.deque([(root, "")])

    while queue:
        node, path = queue.popleft()

        if node != root:
            path += " "
            print("")

        entries = list(range(1 << node.prefix_len_max))

        print("        // 0b{} ... ({} +{})".format(path[:-1], node.offset, len(entries)))

        for value in node.values:
            entries[value[1]] = "        val8!({:#x}, {}),    // {:#0{w}b}".format(
                value[2], value[0], value[1], w=node.prefix_len_max + 2)

        for prefix in sorted(node.children.keys()):
            child = node.children[prefix]
            entries[prefix] = "        jmp8!({}, {}),    // {:#0{w}b}".format(
                child.offset, child.prefix_len_max, prefix, w=node.prefix_len_max + 2)

            child_path = path + "{:0{w}b}".format(prefix, w=node.prefix_len_max)

            queue.append((child, child_path))
        
        print('\n'.join(entries))
        
    print("    ],")
    print("    n_init_bits: {},".format(root.prefix_len_max)),
    print("    n_table_bits: {},".format(max_code_len))
    print("};")
    print("")

def generate_table(name, codes, code_lens, values, group_len):
    group_mask = ~((~0) << group_len)

    # Zip together the Huffman code, Huffman code length in bits, and value.
    full_table_iter = zip(codes, code_lens, values)
    full_table = list(full_table_iter)

    # Build a tree of prefixes.
    root = Node(0)

    max_code_len = 0

    # Build a Trie of Huffman codes by splitting each code into group_len bit prefixes.
    for entry in full_table:
        node = root
        code = entry[0] 
        code_len = entry[1]

        max_code_len = max(max_code_len, code_len)

        # Split the Huffman code into chunks containg group_len bits.
        while code_len > group_len:
            code_len = code_len - group_len
            # Get the prefix from the Huffman code.
            prefix = (code >> code_len) & group_mask
            # The prefix has already been seen, descend into the tree.
            if prefix in node.children:
                node = node.children[prefix]
            # The prefix has not been seen, append it to the tree and then descend.
            else:
                node.children[prefix] = Node(prefix)
                node.prefix_len_max = max(node.prefix_len_max, group_len)
                node = node.children[prefix]

        # The final chunk always has <= group_len bits. Get the final prefix.
        prefix = code & (group_mask >> (group_len - code_len))
        # Append the value to the node, and update the maximum prefix length
        node.values.append((code_len, prefix, entry[2]))
        node.prefix_len_max = max(node.prefix_len_max, code_len)

    # Synthesize all possible prefixes for prefixes that must be padded within their 
    # respective table.
    total_synth, depth = synthesize(root)

    # Assign offset values to each node in the tree (a sub-table) im breadth-first order.
    total = assign_offsets_breadth(root)

    # Generate the actual look-up table from the tree.
    generate_code(root, name, max_code_len)

    eprint("Stats for {}:".format(name))
    eprint("  Total Rows       = {}".format(total))
    eprint("  Synthesized Rows = {} ({:.01%})".format(total_synth, total_synth/total))
    eprint("  Depth            = {}".format(depth + 1))
    eprint("  Max Code Length  = {}".format(max_code_len))
    eprint("")

    # Print the tree.
    eprint("Decode tree for {}:".format(name))
    eprint("")
    print_tree(root)
    eprint("")

def main(args):
    GROUP_LEN = 4

    # Print the preamble.
    print("// Sonata")
    print("// Copyright (c) 2019 The Sonata Project Developers.")
    print("//")
    print("// This Source Code Form is subject to the terms of the Mozilla Public")
    print("// License, v. 2.0. If a copy of the MPL was not distributed with this")
    print("// file, You can obtain one at https://mozilla.org/MPL/2.0/.")
    print("")
    print("//////////////////////////////////////////////////////////////////////")
    print("//                             WARNING                              //")
    print("//                                                                  //")
    print("//         Do not edit the contents of this file manually!          //")
    print("//                                                                  //")
    print("// The tables within this file were automatically derived from the  //")
    print("//          ISO/IEC 11172-3 (MPEG-1 Part 3) standard using          //")
    print("//  mpeg_huffman_tablegen.py in <root>/src/sonata-codec-mp3/tools.  //")
    print("//                                                                  //")
    print("//////////////////////////////////////////////////////////////////////")
    print("")
    print("use sonata_core::{val8, jmp8};")
    print("use sonata_core::io::huffman::{H8, HuffmanTable};")
    print("")

    # For each Huffman table...
    for table in MPEG_TABLES:
        generate_table(table.name, table.codes, table.code_lens, table.values, GROUP_LEN)

if __name__ == "__main__":
    main(sys.argv[1:])