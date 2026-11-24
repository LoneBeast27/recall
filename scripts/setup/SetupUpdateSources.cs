using System;
using System.Collections;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
using System.Text;
using System.Web.Script.Serialization;

namespace Recall.Setup
{
    /// The app's update file sources and pinned keys, from the same files the app is built with
    /// (recall-native/src/services/update_tiers.txt and update_keys.txt, embedded when Setup is compiled),
    /// and the checks the app makes on a signed update file (update_manifest.rs).
    internal static class UpdateSources
    {
        internal const string TiersResource = "update_tiers.txt";
        internal const string KeysResource = "update_keys.txt";
        internal const int ManifestLimit = 256 * 1024;
        internal const int SignatureLimit = 4096;

        internal sealed class Key
        {
            public readonly string Name;
            public readonly Minisign.PublicKey PublicKey;
            public Key(string name, Minisign.PublicKey key) { Name = name; PublicKey = key; }
        }

        /// A signed update file that passed every check.
        internal sealed class Manifest
        {
            public readonly long Sequence;
            public readonly string Version;
            public readonly string Revision;
            public readonly List<string> Supersedes;
            /// The Setup this file names, for an update Setup runs in place (SetupNewerSetup.cs).
            public readonly NewerSetup.File Setup;
            public Manifest(long sequence, string version, string revision, List<string> supersedes, NewerSetup.File setup)
            {
                Sequence = sequence; Version = version; Revision = revision; Supersedes = supersedes; Setup = setup;
            }
        }

        public static string Embedded(string name)
        {
            using (var stream = Assembly.GetExecutingAssembly().GetManifestResourceStream(name))
            {
                if (stream == null) return "";
                using (var reader = new StreamReader(stream, Encoding.UTF8)) return reader.ReadToEnd();
            }
        }

        /// The `manifest <url>` lines (`host` lines only matter to the app's downloads).
        public static List<string> ManifestUrls(string tiers)
        {
            var urls = new List<string>();
            foreach (var parts in Lines(tiers))
                if (parts.Length == 2 && parts[0] == "manifest" && parts[1].StartsWith("https://", StringComparison.Ordinal)) urls.Add(parts[1]);
            return urls;
        }

        /// `<role> <name> <base64>` lines; a malformed key line drops every key (fail closed, as the app).
        public static List<Key> Keys(string text)
        {
            var keys = new List<Key>();
            foreach (var parts in Lines(text))
            {
                var key = parts.Length == 3 && (parts[0] == "release" || parts[0] == "backup") ? Minisign.PublicKey.FromBase64(parts[2]) : null;
                if (key == null || keys.Exists(k => k.Name == parts[1])) return new List<Key>();
                keys.Add(new Key(parts[1], key));
            }
            return keys;
        }

        /// Key names a backup-signed update revoked on this PC (<localAppData>\Recall\updates\trust.json).
        public static List<string> Revoked(string localAppData)
        {
            var names = new List<string>();
            try
            {
                if (string.IsNullOrEmpty(localAppData)) return names;
                var file = Path.Combine(localAppData, "Recall", "updates", "trust.json");
                if (!File.Exists(file)) return names;
                var trust = Parse(File.ReadAllText(file));
                var list = trust != null && trust.ContainsKey("revoked") ? trust["revoked"] as IEnumerable : null;
                if (list != null) foreach (var item in list) { var name = item as string; if (name != null) names.Add(name); }
            }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
            catch (ArgumentException) { }
            catch (NotSupportedException) { }
            return names;
        }

        /// The update file when a pinned, unrevoked key signed it and its fields pass the app's rules, else null.
        public static Manifest Verify(byte[] json, byte[] signature, List<Key> keys, ICollection<string> revoked)
        {
            if (json == null || signature == null || json.Length > ManifestLimit || signature.Length > SignatureLimit) return null;
            var text = Encoding.UTF8.GetString(signature);
            if (!keys.Exists(k => !revoked.Contains(k.Name) && Minisign.Verify(json, text, k.PublicKey))) return null;
            var m = Parse(Encoding.UTF8.GetString(json));
            if (m == null || !Is(m, "schema", 1L) || Text(m, "channel") != "stable") return null;
            long sequence;
            if (!Long(m, "sequence", out sequence) || !ValidSequence(sequence)) return null;
            var version = Text(m, "version");
            var revision = Text(m, "revision");
            if (SetupVersion.Parse(version) == null || revision == null || LatestRelease.RevisionOf(revision) != revision.ToLowerInvariant()) return null;
            var setup = m.ContainsKey("setup") ? m["setup"] as Dictionary<string, object> : null;
            long size;
            if (setup == null || Text(setup, "name") != AppsEntry.UninstallerName || !Long(setup, "size", out size) || size <= 0) return null;
            var sha = Text(setup, "sha256");
            if (!Hex(sha, 64)) return null;
            var supersedes = new List<string>();
            var list = m.ContainsKey("supersedes") ? m["supersedes"] as IEnumerable : null;
            if (list != null) foreach (var item in list) { var r = LatestRelease.RevisionOf(item as string); if (r != null) supersedes.Add(r); }
            var urls = new List<string>();
            var links = setup.ContainsKey("urls") ? setup["urls"] as IEnumerable : null;
            if (links != null)
                foreach (var item in links)
                {
                    var url = item as string;
                    if (url != null && url.StartsWith("https://", StringComparison.Ordinal)) urls.Add(url);
                }
            var file = new NewerSetup.File(version, size, sha.ToLowerInvariant(), urls);
            return new Manifest(sequence, version, revision.ToLowerInvariant(), supersedes, file);
        }

        /// `yyyymmddHHMM` from 2026 to 2100 (update_manifest.rs `valid_sequence`).
        internal static bool ValidSequence(long n)
        {
            long year = n / 100000000, month = n / 1000000 % 100, day = n / 10000 % 100, hour = n / 100 % 100, minute = n % 100;
            return year >= 2026 && year <= 2100 && month >= 1 && month <= 12 && day >= 1 && day <= 31 && hour < 24 && minute < 60;
        }

        private static IEnumerable<string[]> Lines(string text)
        {
            foreach (var raw in (text ?? "").Replace("\r", "").Split('\n'))
            {
                var line = raw.Trim();
                if (line.Length == 0 || line.StartsWith("#", StringComparison.Ordinal)) continue;
                yield return line.Split(new[] { ' ', '\t' }, StringSplitOptions.RemoveEmptyEntries);
            }
        }

        private static bool Hex(string text, int length)
        {
            if (text == null || text.Length != length) return false;
            foreach (var c in text) if (!Uri.IsHexDigit(c)) return false;
            return true;
        }

        private static string Text(Dictionary<string, object> map, string key) { return map.ContainsKey(key) ? map[key] as string : null; }

        private static bool Long(Dictionary<string, object> map, string key, out long value)
        {
            value = 0;
            if (!map.ContainsKey(key) || !(map[key] is int || map[key] is long || map[key] is decimal)) return false;
            try { value = Convert.ToInt64(map[key]); return Convert.ToDecimal(map[key]) == value; }
            catch (OverflowException) { return false; }
        }

        private static bool Is(Dictionary<string, object> map, string key, long expected)
        {
            long value;
            return Long(map, key, out value) && value == expected;
        }

        internal static Dictionary<string, object> Parse(string json)
        {
            try { return new JavaScriptSerializer { MaxJsonLength = ManifestLimit * 2 }.DeserializeObject(json) as Dictionary<string, object>; }
            catch (ArgumentException) { return null; }
            catch (InvalidOperationException) { return null; }
        }
    }
}
