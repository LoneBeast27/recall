using System;
using System.Numerics;
using System.Security.Cryptography;
using System.Text;

namespace Recall.Setup
{
    /// Checks a minisign signature (the update file's `.minisig`) against a pinned public key, as the
    /// app does with the `minisign-verify` crate (recall-native update_manifest.rs): only the
    /// pre-hashed "ED" form (BLAKE2b-512 of the file, then Ed25519), and the trusted comment's own
    /// signature too. .NET Framework has no Ed25519, so both are written out here; they only verify
    /// public data, so speed and constant time do not matter.
    internal static class Minisign
    {
        /// A minisign public key ("Ed" + 8-byte key id + 32-byte key, base64), else null.
        internal sealed class PublicKey
        {
            public readonly byte[] KeyId;
            public readonly byte[] Key;
            private PublicKey(byte[] keyId, byte[] key) { KeyId = keyId; Key = key; }

            public static PublicKey FromBase64(string text)
            {
                var raw = Base64(text);
                if (raw == null || raw.Length != 42 || raw[0] != (byte)'E' || raw[1] != (byte)'d') return null;
                return new PublicKey(Slice(raw, 2, 8), Slice(raw, 10, 32));
            }
        }

        /// True when <paramref name="signatureFile"/> signs <paramref name="data"/> with <paramref name="key"/>.
        public static bool Verify(byte[] data, string signatureFile, PublicKey key)
        {
            if (data == null || signatureFile == null || key == null) return false;
            var lines = signatureFile.Replace("\r", "").Split('\n');
            if (lines.Length < 4 || !lines[0].StartsWith("untrusted comment:", StringComparison.Ordinal)) return false;
            const string trusted = "trusted comment: ";
            if (!lines[2].StartsWith(trusted, StringComparison.Ordinal)) return false;
            var signature = Base64(lines[1]);
            var global = Base64(lines[3]);
            if (signature == null || signature.Length != 74 || global == null || global.Length != 64) return false;
            // Legacy "Ed" signatures (not pre-hashed) are refused, as in the app.
            if (signature[0] != (byte)'E' || signature[1] != (byte)'D') return false;
            for (int i = 0; i < 8; i++) if (signature[2 + i] != key.KeyId[i]) return false;
            var ed = Slice(signature, 10, 64);
            if (!Ed25519.Verify(key.Key, Blake2b.Hash512(data), ed)) return false;
            var comment = Encoding.UTF8.GetBytes(lines[2].Substring(trusted.Length));
            var signedComment = new byte[64 + comment.Length];
            Buffer.BlockCopy(ed, 0, signedComment, 0, 64);
            Buffer.BlockCopy(comment, 0, signedComment, 64, comment.Length);
            return Ed25519.Verify(key.Key, signedComment, global);
        }

        private static byte[] Base64(string text)
        {
            try { return text == null ? null : Convert.FromBase64String(text.Trim()); }
            catch (FormatException) { return null; }
        }

        internal static byte[] Slice(byte[] from, int start, int length)
        {
            var part = new byte[length];
            Buffer.BlockCopy(from, start, part, 0, length);
            return part;
        }
    }

    /// Ed25519 signature check (RFC 8032 section 5.1.7, cofactorless), with BigInteger arithmetic.
    internal static class Ed25519
    {
        private static readonly BigInteger P = BigInteger.Pow(2, 255) - 19;
        private static readonly BigInteger L = BigInteger.Pow(2, 252) + BigInteger.Parse("27742317777372353535851937790883648493");
        private static readonly BigInteger D = Mod(-121665 * Inv(121666));
        private static readonly BigInteger SqrtM1 = BigInteger.ModPow(2, (P - 1) / 4, P);
        private static readonly BigInteger[] Base = BasePoint();

        public static bool Verify(byte[] publicKey, byte[] message, byte[] signature)
        {
            if (publicKey == null || publicKey.Length != 32 || signature == null || signature.Length != 64) return false;
            var a = Decode(publicKey);
            var rBytes = Minisign.Slice(signature, 0, 32);
            var r = Decode(rBytes);
            if (a == null || r == null) return false;
            var s = Number(Minisign.Slice(signature, 32, 32));
            if (s >= L) return false;
            byte[] digest;
            using (var sha = SHA512.Create())
            {
                var input = new byte[64 + message.Length];
                Buffer.BlockCopy(rBytes, 0, input, 0, 32);
                Buffer.BlockCopy(publicKey, 0, input, 32, 32);
                Buffer.BlockCopy(message, 0, input, 64, message.Length);
                digest = sha.ComputeHash(input);
            }
            var h = Number(digest) % L;
            return Same(Multiply(Base, s), Add(r, Multiply(a, h)));
        }

        private static BigInteger Mod(BigInteger x) { var m = x % P; return m.Sign < 0 ? m + P : m; }
        private static BigInteger Inv(BigInteger x) { return BigInteger.ModPow(Mod(x), P - 2, P); }

        /// Little-endian unsigned number.
        private static BigInteger Number(byte[] le)
        {
            var bytes = new byte[le.Length + 1];
            Buffer.BlockCopy(le, 0, bytes, 0, le.Length);
            return new BigInteger(bytes);
        }

        private static BigInteger RecoverX(BigInteger y, int sign)
        {
            if (y >= P) return -1;
            var x2 = Mod((y * y - 1) * Inv(D * y * y + 1));
            if (x2.IsZero) return sign == 0 ? BigInteger.Zero : -1;
            var x = BigInteger.ModPow(x2, (P + 3) / 8, P);
            if (!Mod(x * x - x2).IsZero) x = Mod(x * SqrtM1);
            if (!Mod(x * x - x2).IsZero) return -1;
            if ((int)(x & 1) != sign) x = P - x;
            return x;
        }

        /// A point in extended coordinates (X, Y, Z, T), or null when the bytes are not a point.
        private static BigInteger[] Decode(byte[] encoded)
        {
            var bytes = (byte[])encoded.Clone();
            int sign = bytes[31] >> 7;
            bytes[31] &= 0x7f;
            var y = Number(bytes);
            var x = RecoverX(y, sign);
            if (x.Sign < 0) return null;
            return new[] { x, y, BigInteger.One, Mod(x * y) };
        }

        private static BigInteger[] BasePoint()
        {
            var y = Mod(4 * Inv(5));
            var x = RecoverX(y, 0);
            return new[] { x, y, BigInteger.One, Mod(x * y) };
        }

        private static BigInteger[] Add(BigInteger[] p, BigInteger[] q)
        {
            var a = Mod((p[1] - p[0]) * (q[1] - q[0]));
            var b = Mod((p[1] + p[0]) * (q[1] + q[0]));
            var c = Mod(2 * p[3] * q[3] * D);
            var d = Mod(2 * p[2] * q[2]);
            BigInteger e = b - a, f = d - c, g = d + c, h = b + a;
            return new[] { Mod(e * f), Mod(g * h), Mod(f * g), Mod(e * h) };
        }

        private static BigInteger[] Multiply(BigInteger[] point, BigInteger scalar)
        {
            var result = new[] { BigInteger.Zero, BigInteger.One, BigInteger.One, BigInteger.Zero };
            while (scalar > 0)
            {
                if (!scalar.IsEven) result = Add(result, point);
                point = Add(point, point);
                scalar >>= 1;
            }
            return result;
        }

        private static bool Same(BigInteger[] p, BigInteger[] q)
        {
            return Mod(p[0] * q[2] - q[0] * p[2]).IsZero && Mod(p[1] * q[2] - q[1] * p[2]).IsZero;
        }
    }

    /// BLAKE2b-512 without a key (RFC 7693), the hash minisign signs.
    internal static class Blake2b
    {
        private static readonly ulong[] IV =
        {
            0x6a09e667f3bcc908UL, 0xbb67ae8584caa73bUL, 0x3c6ef372fe94f82bUL, 0xa54ff53a5f1d36f1UL,
            0x510e527fade682d1UL, 0x9b05688c2b3e6c1fUL, 0x1f83d9abfb41bd6bUL, 0x5be0cd19137e2179UL,
        };

        private static readonly byte[][] Sigma =
        {
            new byte[] { 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15 },
            new byte[] { 14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3 },
            new byte[] { 11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4 },
            new byte[] { 7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8 },
            new byte[] { 9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13 },
            new byte[] { 2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9 },
            new byte[] { 12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11 },
            new byte[] { 13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10 },
            new byte[] { 6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5 },
            new byte[] { 10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0 },
        };

        public static byte[] Hash512(byte[] data)
        {
            var h = (ulong[])IV.Clone();
            h[0] ^= 0x01010040UL;
            int blocks = Math.Max(1, (data.Length + 127) / 128);
            var block = new byte[128];
            for (int i = 0; i < blocks; i++)
            {
                int start = i * 128, length = Math.Min(128, data.Length - start);
                Array.Clear(block, 0, 128);
                if (length > 0) Buffer.BlockCopy(data, start, block, 0, length);
                bool last = i == blocks - 1;
                Compress(h, block, (ulong)(last ? data.Length : start + 128), last);
            }
            var output = new byte[64];
            for (int i = 0; i < 8; i++) Buffer.BlockCopy(BitConverter.GetBytes(h[i]), 0, output, i * 8, 8);
            return output;
        }

        private static void Compress(ulong[] h, byte[] block, ulong counter, bool last)
        {
            var m = new ulong[16];
            for (int i = 0; i < 16; i++) m[i] = BitConverter.ToUInt64(block, i * 8);
            var v = new ulong[16];
            Array.Copy(h, v, 8);
            Array.Copy(IV, 0, v, 8, 8);
            v[12] ^= counter;
            if (last) v[14] = ~v[14];
            for (int r = 0; r < 12; r++)
            {
                var s = Sigma[r % 10];
                G(v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
                G(v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
                G(v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
                G(v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
                G(v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
                G(v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
                G(v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
                G(v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
            }
            for (int i = 0; i < 8; i++) h[i] ^= v[i] ^ v[i + 8];
        }

        private static void G(ulong[] v, int a, int b, int c, int d, ulong x, ulong y)
        {
            v[a] = v[a] + v[b] + x; v[d] = Rotr(v[d] ^ v[a], 32);
            v[c] = v[c] + v[d]; v[b] = Rotr(v[b] ^ v[c], 24);
            v[a] = v[a] + v[b] + y; v[d] = Rotr(v[d] ^ v[a], 16);
            v[c] = v[c] + v[d]; v[b] = Rotr(v[b] ^ v[c], 63);
        }

        private static ulong Rotr(ulong x, int n) { return (x >> n) | (x << (64 - n)); }
    }
}
