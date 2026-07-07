//! Colorimetry: the exact `RGBToRGB` chain novideo_srgb uses to build an sRGB
//! gamut-clamp matrix, ported from ledoge's `Colorimetry.cs`.
//!
//! The clamp matrix engaged by novideo's "Clamped" checkbox is
//! `RGBToRGB(sRGB, panel_primaries) = XYZToRGB(panel) * RGBToXYZ(sRGB)`.
//! Both spaces are D65-white, so no chromatic adaptation is needed (that path is
//! only for ICC profiles / D50 PCS, which we don't do here). This module is pure
//! math -- no FFI, no Windows -- so it is unit-testable in isolation.
//!
//! Math credit (same as ledoge): https://mina86.com/2019/srgb-xyz-matrix/.

use crate::edid::EdidPrimaries;

/// A 3x3 matrix, row-major.
pub type Mat3 = [[f64; 3]; 3];

/// CIE xy chromaticity coordinates for the four color primaries of an RGB space.
#[derive(Debug, Clone, Copy)]
pub struct ColorSpace {
    pub red: (f64, f64),
    pub green: (f64, f64),
    pub blue: (f64, f64),
    pub white: (f64, f64),
}

/// CIE D65 white point (matches `Colorimetry.D65`).
pub const D65: (f64, f64) = (0.3127, 0.3290);

/// sRGB primaries (matches `Colorimetry.sRGB`).
pub const SRGB: ColorSpace = ColorSpace {
    red: (0.64, 0.33),
    green: (0.30, 0.60),
    blue: (0.15, 0.06),
    white: D65,
};

impl From<&EdidPrimaries> for ColorSpace {
    fn from(p: &EdidPrimaries) -> Self {
        ColorSpace {
            red: p.red,
            green: p.green,
            blue: p.blue,
            white: p.white,
        }
    }
}

/// `M * v` for a 3-vector.
fn mul_vec(m: &Mat3, v: &[f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// `A * B` for two 3x3 matrices.
fn mul_mat(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            for k in 0..3 {
                r[i][j] += a[i][k] * b[k][j];
            }
        }
    }
    r
}

/// Diagonal matrix from a 3-vector.
fn diag(v: &[f64; 3]) -> Mat3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

/// 3x3 inverse via cofactors. Returns identity if singular (degenerate EDID) --
/// callers already treat a bad clamp as "reset", so this fails safe.
fn inverse(m: &Mat3) -> Mat3 {
    let a = m[0][0];
    let b = m[0][1];
    let c = m[0][2];
    let d = m[1][0];
    let e = m[1][1];
    let f = m[1][2];
    let g = m[2][0];
    let h = m[2][1];
    let i = m[2][2];

    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let inv = 1.0 / det;
    [
        [
            (e * i - f * h) * inv,
            (c * h - b * i) * inv,
            (b * f - c * e) * inv,
        ],
        [
            (f * g - d * i) * inv,
            (a * i - c * g) * inv,
            (c * d - a * f) * inv,
        ],
        [
            (d * h - e * g) * inv,
            (b * g - a * h) * inv,
            (a * e - b * d) * inv,
        ],
    ]
}

/// `RGBToXYZ` -- ports `Colorimetry.RGBToXYZ`.
///
/// Builds M' from the xy primaries, solves for the per-channel luminance scale
/// so that (1,1,1) maps to the white point, then returns `M' * diag(scale)`.
fn rgb_to_xyz(cs: &ColorSpace) -> Mat3 {
    let (rx, ry) = cs.red;
    let (gx, gy) = cs.green;
    let (bx, by) = cs.blue;
    let (wx, wy) = cs.white;

    let white_xyz = [wx / wy, 1.0, (1.0 - wx - wy) / wy];

    let m_prime: Mat3 = [
        [rx / ry, gx / gy, bx / by],
        [1.0, 1.0, 1.0],
        [
            (1.0 - rx - ry) / ry,
            (1.0 - gx - gy) / gy,
            (1.0 - bx - by) / by,
        ],
    ];

    let scale = mul_vec(&inverse(&m_prime), &white_xyz);
    mul_mat(&m_prime, &diag(&scale))
}

/// `XYZToRGB` -- inverse of [`rgb_to_xyz`].
fn xyz_to_rgb(cs: &ColorSpace) -> Mat3 {
    inverse(&rgb_to_xyz(cs))
}

/// `RGBToRGB(from, to) = XYZToRGB(to) * RGBToXYZ(from)`. This is the clamp
/// matrix: `rgb_to_rgb(&SRGB, &panel)` maps sRGB-encoded content onto the wide
/// panel so full sRGB saturation lands on sRGB gamut (i.e. desaturates).
pub fn rgb_to_rgb(from: &ColorSpace, to: &ColorSpace) -> Mat3 {
    mul_mat(&xyz_to_rgb(to), &rgb_to_xyz(from))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sRGB->sRGB must be identity (within fp tolerance).
    #[test]
    fn srgb_to_srgb_is_identity() {
        let m = rgb_to_rgb(&SRGB, &SRGB);
        for i in 0..3 {
            for j in 0..3 {
                let expect = if i == j { 1.0 } else { 0.0 };
                assert!(
                    (m[i][j] - expect).abs() < 1e-9,
                    "m[{i}][{j}] = {} expected {expect}",
                    m[i][j]
                );
            }
        }
    }

    /// Clamping sRGB content INTO a wider panel (P3) desaturates: to reproduce a
    /// pure-sRGB red on a panel whose red primary is *more* saturated you drive
    /// *less* of that primary, so the R diagonal is < 1, and you add a touch of
    /// the other channels (positive off-diagonals) to pull the hue back toward
    /// sRGB. Row sums stay ~1 (white preserved, both D65). This mirrors the exact
    /// matrix novideo_srgb pushes; a value near 0.82 for R-gain is expected.
    #[test]
    fn clamp_into_wide_panel_desaturates() {
        let p3 = ColorSpace {
            red: (0.68, 0.32),
            green: (0.265, 0.69),
            blue: (0.15, 0.06),
            white: D65,
        };
        let m = rgb_to_rgb(&SRGB, &p3);
        // Narrower source into wider dest -> primary gains shrink below 1.
        assert!(
            m[0][0] < 1.0 && m[0][0] > 0.5,
            "R gain should be <1 (desaturate): {}",
            m[0][0]
        );
        // Cross terms feed in the other channels to correct hue -> non-zero.
        assert!(m[0][1] > 0.0, "R<-G term should be positive: {}", m[0][1]);
        // White (1,1,1) must map to ~(1,1,1): both spaces are D65, so each row
        // of the RGB->RGB matrix sums to ~1.
        for i in 0..3 {
            let row_sum = m[i][0] + m[i][1] + m[i][2];
            assert!(
                (row_sum - 1.0).abs() < 1e-6,
                "row {i} sum should be ~1 (white preserved): {row_sum}"
            );
        }
    }
}
