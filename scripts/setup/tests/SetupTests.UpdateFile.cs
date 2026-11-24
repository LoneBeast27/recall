using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Text;

namespace Recall.Setup.Tests
{
    /// Setup's "is a newer Recall published?" check: the signed update file from every source, as
    /// the app reads it. Fixtures in tests\fixtures: the real 0.1.11 file (release key) and files
    /// signed with a throwaway test key whose secret half was never kept.
    internal static partial class TestRunner
    {
        private const string TestKey = "RWQHypEnZgmNHqFnI2zsaK9HiSyfyzsK0zM7ZXR1KhN/18JMy9sRrg7z";

        private static byte[] Fixture(string name) { return File.ReadAllBytes(Path.Combine(sourceDir, "tests", "fixtures", name)); }

        private static List<UpdateSources.Key> TestKeys()
        {
            return UpdateSources.Keys("release test-1 " + TestKey);
        }

        private static void UpdateFileChecks()
        {
            // BLAKE2b-512 (RFC 7693 appendix A) and Ed25519 (RFC 8032 test 1).
            Check(Hex(Blake2b.Hash512(Encoding.ASCII.GetBytes("abc"))) == "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d17d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923", "BLAKE2b-512 of abc");
            Check(Hex(Blake2b.Hash512(new byte[300])).Length == 128 && Hex(Blake2b.Hash512(new byte[128])) != Hex(Blake2b.Hash512(new byte[129])), "BLAKE2b multi-block");
            var pk = Bytes("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
            var sig = Bytes("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b");
            Check(Ed25519.Verify(pk, new byte[0], sig), "RFC 8032 test 1 verifies");
            sig[5] ^= 1;
            Check(!Ed25519.Verify(pk, new byte[0], sig), "a changed signature fails");
            Check(!Ed25519.Verify(pk, new byte[] { 1 }, Bytes("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b")), "another message fails");

            // The shipped keys: the same file the app is built with, and the real 0.1.11 update file.
            var shipped = UpdateSources.Keys(UpdateSources.Embedded(UpdateSources.KeysResource));
            Check(shipped.Count == 2 && shipped.Exists(k => k.Name == "release-1") && shipped.Exists(k => k.Name == "backup-1"), "both pinned keys embedded");
            var real = UpdateSources.Verify(Fixture("v0.1.11.json"), Fixture("v0.1.11.json.minisig"), shipped, new string[0]);
            Check(real != null && real.Version == "0.1.11" && real.Sequence == 202611231410L && real.Revision == "cc60411c806bc02c872f0aa44206dda92f393138"
                && real.Supersedes.Count == 2, "the published 0.1.11 update file verifies with the release key");
            Check(UpdateSources.Verify(Fixture("v0.1.11.json"), Fixture("v0.1.11.json.minisig"), shipped, new[] { "release-1" }) == null, "a revoked key is not trusted");
            Check(UpdateSources.Verify(Fixture("v0.1.11.json"), Fixture("v0.1.11.json.minisig"), TestKeys(), new string[0]) == null, "another key: refused");
            var changed = Fixture("v0.1.11.json");
            changed[changed.Length / 2] ^= 1;
            Check(UpdateSources.Verify(changed, Fixture("v0.1.11.json.minisig"), shipped, new string[0]) == null, "one changed byte: refused");
            var signature = Encoding.UTF8.GetString(Fixture("v0.1.11.json.minisig")).Replace("sequence 202611231410", "sequence 202611231411");
            Check(UpdateSources.Verify(Fixture("v0.1.11.json"), Encoding.UTF8.GetBytes(signature), shipped, new string[0]) == null, "a changed trusted comment: refused");

            // Fields after the signature, and the old unhashed signature form.
            Check(UpdateSources.Verify(Fixture("schema2.json"), Fixture("schema2.json.minisig"), TestKeys(), new string[0]) == null, "unknown layout: refused");
            Check(UpdateSources.Verify(Fixture("legacy.json"), Fixture("legacy.json.minisig"), TestKeys(), new string[0]) == null, "legacy signature: refused, as in the app");
            Check(UpdateSources.Verify(Fixture("older.json"), Fixture("older.json.minisig"), TestKeys(), new string[0]) != null, "a valid test file passes");
            Check(UpdateSources.ValidSequence(202611231410L) && !UpdateSources.ValidSequence(210101010000L) && !UpdateSources.ValidSequence(202613010000L), "sequence range");
            Check(UpdateSources.Keys("release a " + TestKey + "\nrelease a " + TestKey).Count == 0 && UpdateSources.Keys("other a " + TestKey).Count == 0, "bad key file: no keys");

            // Sources: the same file the app is built with, GitLab first and GitHub kept.
            var urls = UpdateSources.ManifestUrls(UpdateSources.Embedded(UpdateSources.TiersResource));
            Check(urls.Count == 3 && urls[0].StartsWith("https://gitlab.com/", StringComparison.Ordinal) && urls[2].StartsWith("https://github.com/", StringComparison.Ordinal), "three sources embedded");
            Check(UpdateSources.ManifestUrls("# c\nhost a.example\nmanifest http://plain.example/x\nmanifest https://a.example/x").Count == 1, "https manifest lines only");
            UpdateRevokedFile();
        }

        private static void UpdateRevokedFile()
        {
            var root = Path.Combine(Path.GetTempPath(), "recall-setup-trust-" + Guid.NewGuid().ToString("N"));
            try
            {
                Check(UpdateSources.Revoked(root).Count == 0 && UpdateSources.Revoked(null).Count == 0, "no trust file: nothing revoked");
                var dir = Path.Combine(root, "Recall", "updates");
                Directory.CreateDirectory(dir);
                File.WriteAllText(Path.Combine(dir, "trust.json"), "{\"highest_sequence\":1,\"revoked\":[\"release-1\",7]}");
                var revoked = UpdateSources.Revoked(root);
                Check(revoked.Count == 1 && revoked[0] == "release-1", "revoked key names read");
                File.WriteAllText(Path.Combine(dir, "trust.json"), "not json");
                Check(UpdateSources.Revoked(root).Count == 0, "garbled trust file: nothing revoked");
            }
            finally
            {
                try { Directory.Delete(root, true); } catch (IOException) { } catch (UnauthorizedAccessException) { }
            }
        }

        /// The online check with fake sources: newest sequence wins, a failing source is skipped, the budget holds.
        private static void UpdateFileFetch(string old, string fix, string later)
        {
            const string A = "https://a.example/recall-update.json", B = "https://b.example/recall-update.json", C = "https://c.example/recall-update.json";
            var sources = new List<string> { A, B, C };
            Func<string, string, Dictionary<string, byte[]>> serving = (a, b) => new Dictionary<string, byte[]> {
                { A, Fixture(a + ".json") }, { A + ".minisig", Fixture(a + ".json.minisig") },
                { B, Fixture(b + ".json") }, { B + ".minisig", Fixture(b + ".json.minisig") },
            };
            // null: a non-200 answer (as Get returns); "403": the WebException HttpWebRequest throws.
            Func<Dictionary<string, byte[]>, Func<string, int, byte[]>> serve = served => (url, limit) =>
            {
                byte[] body;
                if (!served.TryGetValue(url, out body)) return null;
                if (body.Length == 3 && Encoding.ASCII.GetString(body) == "403") throw new WebException("HTTP 403", WebExceptionStatus.ProtocolError);
                return body.Length > limit ? null : body;
            };
            var none = new string[0];
            var info = LatestRelease.Fetch(serve(serving("older", "hotfix")), old, sources, TestKeys(), none, 3000);
            Check(info != null && info.Version == "0.1.5" && info.Revision == fix && info.NewerBuild, "newest file wins; it replaces this Setup's build");
            info = LatestRelease.Fetch(serve(serving("hotfix", "older")), fix, sources, TestKeys(), none, 3000);
            Check(info != null && info.Revision == fix && !info.NewerBuild, "this Setup is the published build");
            info = LatestRelease.Fetch(serve(serving("hotfix", "older")), later, sources, TestKeys(), none, 3000);
            Check(info != null && !info.NewerBuild, "a build the file does not replace is never redirected");
            info = LatestRelease.Fetch(serve(serving("hotfix", "older")), "", sources, TestKeys(), none, 3000);
            Check(info != null && info.Version == "0.1.5" && !info.NewerBuild, "unstamped Setup: no build comparison");
            var map = serving("hotfix", "older");
            map[A] = Encoding.ASCII.GetBytes("403");
            info = LatestRelease.Fetch(serve(map), old, sources, TestKeys(), none, 3000);
            Check(info != null && info.Version == "0.1.4", "first source refused: the next one answers");
            map = serving("hotfix", "schema2");
            map.Remove(A + ".minisig");
            Check(LatestRelease.Fetch(serve(map), old, sources, TestKeys(), none, 3000) == null, "no verified file anywhere: offline");
            map = serving("hotfix", "older");
            map[A] = Fixture("older.json");
            info = LatestRelease.Fetch(serve(map), old, sources, TestKeys(), none, 3000);
            Check(info != null && info.Version == "0.1.4", "a file under another file's signature is skipped");
            Check(LatestRelease.Fetch(serve(serving("hotfix", "older")), old, sources, new List<UpdateSources.Key>(), none, 3000) == null, "no pinned keys: fail closed");
            Func<string, int, byte[]> slowA = (url, limit) =>
            {
                if (url.StartsWith(A, StringComparison.Ordinal)) System.Threading.Thread.Sleep(3000);
                return serve(serving("hotfix", "older"))(url, limit);
            };
            var watch = System.Diagnostics.Stopwatch.StartNew();
            info = LatestRelease.Fetch(slowA, old, sources, TestKeys(), none, 500);
            Check(info != null && info.Version == "0.1.4" && watch.ElapsedMilliseconds < 2000, "a slow source is dropped, the others answer");
            watch = System.Diagnostics.Stopwatch.StartNew();
            Check(LatestRelease.Within(() => { System.Threading.Thread.Sleep(3000); return new LatestRelease.Info("9.9.9", null, false); }, 300) == null
                && watch.ElapsedMilliseconds < 2000, "a slow check (DNS included) counts as offline after the budget");
            Check(LatestRelease.Within(() => new LatestRelease.Info("0.1.5", null, false), 2000).Version == "0.1.5", "a quick answer is used");
            Check(LatestRelease.BudgetMs <= 4000, "the whole check stays within about 4 s");
        }

        private static string Hex(byte[] bytes) { return string.Concat(bytes.Select(b => b.ToString("x2"))); }

        private static byte[] Bytes(string hex)
        {
            var bytes = new byte[hex.Length / 2];
            for (int i = 0; i < bytes.Length; i++) bytes[i] = Convert.ToByte(hex.Substring(i * 2, 2), 16);
            return bytes;
        }
    }
}
