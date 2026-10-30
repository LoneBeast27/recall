"""Deflate PNGs written with stored zlib blocks (recall-overlay's exports).

Rewrites each file in place with its IDAT data compressed at zlib level 9.
The filtered scanlines are byte-identical before and after, so no pixel
changes. Standard library only.

    python -I scripts/dev/png-recompress.py FILE.png [FILE.png ...]
"""
import struct
import sys
import zlib


def chunks(data):
    pos = 8
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        kind = data[pos + 4:pos + 8]
        yield kind, data[pos + 8:pos + 8 + length]
        pos += 12 + length


def chunk(kind, body):
    return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)


def recompress(path):
    data = open(path, "rb").read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", f"{path}: not a PNG"
    parts = list(chunks(data))
    scanlines = zlib.decompress(b"".join(body for kind, body in parts if kind == b"IDAT"))
    packed = zlib.compress(scanlines, 9)
    assert zlib.decompress(packed) == scanlines
    out = [data[:8]]
    wrote_idat = False
    for kind, body in parts:
        if kind == b"IDAT":
            if not wrote_idat:
                out.append(chunk(b"IDAT", packed))
                wrote_idat = True
            continue
        out.append(chunk(kind, body))
    result = b"".join(out)
    open(path, "wb").write(result)
    print(f"{path}: {len(data) / 1e6:.2f} MB -> {len(result) / 1e6:.2f} MB")


if __name__ == "__main__":
    for name in sys.argv[1:]:
        recompress(name)
