#!/usr/bin/env python3
"""
make-punchy.py — derive the PUNCHY MHC2 profile from the ACCURATE one.

The punchy variant is byte-identical to the accurate profile EXCEPT its colorant
tags (rXYZ/gXYZ/bXYZ) and the chrm tag are rewritten to declare sRGB primaries
instead of the panel's native wide gamut. Windows ACM then sees an "sRGB display",
does NO gamut compression, and passes sRGB code values straight to the native panel
primaries at scanout -> the familiar native-wide-gamut saturation stretch. The
gamma-2.2 TRC, the identity MHC2 matrix, the regamma LUT and the 1300-nit luminance
are all preserved unchanged (so the transfer fix and HDR/PQ passthrough are identical).

No MHC2Gen mode emits "sRGB colorants + identity matrix" directly:
  - sdr-acm always declares the device's NATIVE colorants (that IS the accurate profile);
  - sdr-csc --source-gamut sRGB bakes an sRGB->native->sRGB PROOFING (clamp) matrix,
    which emulates sRGB rather than stretching to native.
Hence this surgical tag rewrite.

The sRGB colorants are computed in the accurate profile's own PCS(D50)-relative space
using that profile's 'chad' (D65->D50 Bradford) matrix, so Windows reports exactly
sRGB xy after inverse-chad, with no negative components.

Usage:  python make-punchy.py <accurate.icm> <punchy.icm> [--desc "<profile description>"]
        Both files normally live in this PC's hdrGammaFix.profileDir (.local/machine.json),
        outside the repo: the profiles are built from one display's own calibration.
"""
import argparse, struct, hashlib

# sRGB primaries (xy), the target Windows should report for the punchy profile.
SRGB_XY = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)]

# sRGB colorants pre-computed in THIS profile's PCS(D50)-relative space (via its chad).
# Regenerate these with compute_srgb_pcs_colorants() below if the source profile's
# chad ever changes (i.e. after a fresh Windows HDR Calibration + new accurate profile).
SRGB_PCS_COLORANTS = {
    'rXYZ': (0.416980, 0.214970, 0.015434),
    'gXYZ': (0.360072, 0.720727, 0.103570),
    'bXYZ': (0.151495, 0.063401, 0.773141),
}
DEFAULT_DESC = 'Punchy Gamma2.2 (native gamut, MHC2)'


def parse_tags(d):
    n = struct.unpack('>I', d[128:132])[0]
    tags = {}; off = 132
    for _ in range(n):
        sig = bytes(d[off:off+4]).decode('latin1')
        toff = struct.unpack('>I', d[off+4:off+8])[0]
        tsz = struct.unpack('>I', d[off+8:off+12])[0]
        tags[sig] = (toff, tsz); off += 12
    return tags


def wr_s15f16(dst, pos, val):
    dst[pos:pos+4] = struct.pack('>i', int(round(val * 65536)))


def wr_u16f16(dst, pos, val):
    dst[pos:pos+4] = struct.pack('>I', int(round(val * 65536)))


def compute_srgb_pcs_colorants(d, tags):
    """Recompute SRGB_PCS_COLORANTS from the profile's own chad. Requires numpy."""
    import numpy as np
    def s15f16(b): return struct.unpack('>i', bytes(b))[0] / 65536.0
    coff = tags['chad'][0]
    CHAD = np.array([s15f16(d[coff+8+4*k:coff+8+4*k+4]) for k in range(9)]).reshape(3, 3)
    def xyY(x, y, Y=1.0): return np.array([x*Y/y, Y, (1-x-y)*Y/y])
    R, G, B = (xyY(*p) for p in SRGB_XY)
    W = xyY(0.3127, 0.3290, 1.0)  # D65
    S = np.linalg.solve(np.column_stack([R, G, B]), W)
    out = {}
    for name, col, s in [('rXYZ', R, S[0]), ('gXYZ', G, S[1]), ('bXYZ', B, S[2])]:
        pcs = CHAD @ (col * s)
        out[name] = tuple(round(float(v), 6) for v in pcs)
    return out


def main():
    ap = argparse.ArgumentParser(description='Derive the punchy MHC2 profile from the accurate one.')
    ap.add_argument('src', help='accurate profile (.icm) to read')
    ap.add_argument('out', help='punchy profile (.icm) to write')
    ap.add_argument('--desc', default=DEFAULT_DESC, help='profile description written into desc')
    args = ap.parse_args()
    SRC, OUT, DESC = args.src, args.out, args.desc
    with open(SRC, 'rb') as f:
        d = bytearray(f.read())
    tags = parse_tags(d)

    # 1) colorant XYZ tags -> sRGB (PCS/D50-relative)
    for t, (X, Y, Z) in SRGB_PCS_COLORANTS.items():
        off = tags[t][0]
        assert bytes(d[off:off+4]) == b'XYZ ', (t, bytes(d[off:off+4]))
        wr_s15f16(d, off+8, X); wr_s15f16(d, off+12, Y); wr_s15f16(d, off+16, Z)

    # 2) chrm tag -> sRGB xy (illuminant-relative)
    off = tags['chrm'][0]; p = off + 12
    for (x, y) in SRGB_XY:
        wr_u16f16(d, p, x); wr_u16f16(d, p+4, y); p += 8

    # 3) desc (mluc) in-place if it fits
    off, _ = tags['desc']
    assert bytes(d[off:off+4]) == b'mluc'
    rec0 = off + 16
    ln = struct.unpack('>I', d[rec0+4:rec0+8])[0]
    soff = struct.unpack('>I', d[rec0+8:rec0+12])[0]
    nb = DESC.encode('utf-16-be')
    if len(nb) <= ln:
        d[off+soff:off+soff+ln] = nb + b'\x00' * (ln - len(nb))
        d[rec0+4:rec0+8] = struct.pack('>I', len(nb))

    # 4) recompute ICC profile ID (MD5 with flags/intent/id zeroed per ICC spec)
    tmp = bytearray(d)
    tmp[44:48] = b'\x00' * 4
    tmp[64:68] = b'\x00' * 4
    tmp[84:100] = b'\x00' * 16
    d[84:100] = hashlib.md5(bytes(tmp)).digest()

    with open(OUT, 'wb') as f:
        f.write(d)
    print(f'wrote {OUT} ({len(d)} bytes), profileID={d[84:100].hex()}')


if __name__ == '__main__':
    main()
