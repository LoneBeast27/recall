using System;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Linq;
using System.Threading.Tasks;
using System.Web.Script.Serialization;

namespace Recall.Setup
{
    /// The latest published Recall, as the in-app updater sees it (recall-native update_sources.rs):
    /// the signed update file with the highest sequence among every source in update_tiers.txt
    /// (GitLab, the update site, GitHub), asked in parallel. A source that is down, hidden or
    /// serves a bad signature is skipped. Null when no source gave a verified file: Setup then
    /// compares against its own version only.
    internal static class LatestRelease
    {
        private const int TimeoutMs = 2000;
        /// The whole online check, DNS included: past it Setup goes on as if offline.
        public const int BudgetMs = 4000;
        /// How long the sources are waited on inside the budget; late ones are dropped.
        private const int SourcesMs = 3500;

        /// The latest release's version and commit, and whether that build replaces this Setup's
        /// (a same-version hotfix: its update file lists this Setup's commit in `supersedes`).
        internal sealed class Info
        {
            public readonly string Version;
            public readonly string Revision;
            public readonly bool NewerBuild;
            /// The published Setup (size, SHA-256, download addresses) from the signed file; null in tests.
            public readonly NewerSetup.File Setup;
            public Info(string version, string revision, bool newerBuild) : this(version, revision, newerBuild, null) { }
            public Info(string version, string revision, bool newerBuild, NewerSetup.File setup)
            {
                Version = version; Revision = revision; NewerBuild = newerBuild; Setup = setup;
            }
        }

        /// Every source within <see cref="BudgetMs"/> (a slow DNS lookup too), else null: offline.
        public static Info Fetch(string setupRevision, string localAppData)
        {
            ServicePointManager.SecurityProtocol |= (SecurityProtocolType)3072;
            var urls = UpdateSources.ManifestUrls(UpdateSources.Embedded(UpdateSources.TiersResource));
            var keys = UpdateSources.Keys(UpdateSources.Embedded(UpdateSources.KeysResource));
            var revoked = UpdateSources.Revoked(localAppData);
            return Within(() => Fetch(Get, setupRevision, urls, keys, revoked, SourcesMs), BudgetMs);
        }

        /// <paramref name="check"/>'s answer, or null when it takes longer than <paramref name="ms"/>
        /// (it runs on a background thread, which never keeps Setup open).
        internal static Info Within(Func<Info> check, int ms)
        {
            var task = Task.Run(check);
            try { return task.Wait(ms) ? task.Result : null; }
            catch (AggregateException) { return null; }
        }

        /// <paramref name="get"/> returns a body (at most the limit given), null for a non-200 answer,
        /// or throws (tests pass their own). Sources still busy after <paramref name="waitMs"/> are dropped.
        internal static Info Fetch(Func<string, int, byte[]> get, string setupRevision, List<string> urls,
            List<UpdateSources.Key> keys, ICollection<string> revoked, int waitMs)
        {
            if (urls.Count == 0 || keys.Count == 0) return null;
            var tasks = urls.Select(url => Task.Run(() => FromSource(get, url, keys, revoked))).ToArray();
            try { Task.WaitAll(tasks, waitMs); }
            catch (AggregateException) { }
            UpdateSources.Manifest best = null;
            foreach (var task in tasks)
            {
                if (task.Status != TaskStatus.RanToCompletion || task.Result == null) continue;
                if (best == null || task.Result.Sequence > best.Sequence) best = task.Result;
            }
            if (best == null) return null;
            var own = RevisionOf(setupRevision);
            bool newerBuild = own != null && own != best.Revision && best.Supersedes.Contains(own);
            return new Info(best.Version, best.Revision, newerBuild, best.Setup);
        }

        /// One source's verified update file, else null (any failure: that source is skipped).
        private static UpdateSources.Manifest FromSource(Func<string, int, byte[]> get, string url,
            List<UpdateSources.Key> keys, ICollection<string> revoked)
        {
            var json = TryGet(get, url, UpdateSources.ManifestLimit);
            if (json == null) return null;
            var signature = TryGet(get, url + ".minisig", UpdateSources.SignatureLimit);
            return signature == null ? null : UpdateSources.Verify(json, signature, keys, revoked);
        }

        /// One request: any failure is "no answer".
        private static byte[] TryGet(Func<string, int, byte[]> get, string url, int limit)
        {
            try { return get(url, limit); }
            catch (WebException) { return null; }
            catch (IOException) { return null; }
            catch (InvalidOperationException) { return null; }
            catch (NotSupportedException) { return null; }
            catch (ArgumentException) { return null; }
        }

        /// The app's "Check for updates automatically" (recall-native update_prefs.rs): on unless
        /// <localAppData>\Recall\updates\settings.json says "check_automatically": false. Off, Setup
        /// asks no update source before its window.
        public static bool ChecksAllowed(string localAppData)
        {
            try
            {
                if (string.IsNullOrEmpty(localAppData)) return true;
                var file = Path.Combine(localAppData, "Recall", "updates", "settings.json");
                if (!File.Exists(file)) return true;
                var settings = Parse(File.ReadAllText(file));
                return !(settings != null && settings.ContainsKey("check_automatically")
                    && settings["check_automatically"] is bool && !(bool)settings["check_automatically"]);
            }
            catch (IOException) { return true; }
            catch (UnauthorizedAccessException) { return true; }
            catch (ArgumentException) { return true; }
            catch (NotSupportedException) { return true; }
        }

        /// The 40-hex commit on the first line, lowercase; else null.
        internal static string RevisionOf(string text)
        {
            if (text == null) return null;
            var line = text.TrimStart('\uFEFF').Split('\n')[0].Trim().ToLowerInvariant();
            if (line.Length != 40) return null;
            foreach (var c in line) if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))) return null;
            return line;
        }

        private static byte[] Get(string url, int limit)
        {
            var request = (HttpWebRequest)WebRequest.Create(url);
            request.UserAgent = "RecallSetup";
            request.Timeout = request.ReadWriteTimeout = TimeoutMs;
            using (var response = (HttpWebResponse)request.GetResponse())
            {
                if (response.StatusCode != HttpStatusCode.OK) return null;
                using (var body = response.GetResponseStream())
                using (var memory = new MemoryStream())
                {
                    var buffer = new byte[16384];
                    int read;
                    while ((read = body.Read(buffer, 0, buffer.Length)) > 0)
                    {
                        memory.Write(buffer, 0, read);
                        if (memory.Length > limit) return null;
                    }
                    return memory.ToArray();
                }
            }
        }

        private static Dictionary<string, object> Parse(string json)
        {
            try { return new JavaScriptSerializer { MaxJsonLength = 1 << 20 }.DeserializeObject(json) as Dictionary<string, object>; }
            catch (ArgumentException) { return null; }
            catch (InvalidOperationException) { return null; }
        }
    }

    /// "major.minor.patch" versions, as Setup and the updater write them.
    internal static class SetupVersion
    {
        public static int[] Parse(string text)
        {
            if (text == null) return null;
            var parts = text.Trim().Split('.');
            if (parts.Length != 3) return null;
            var numbers = new int[3];
            for (int i = 0; i < 3; i++)
                if (parts[i].Length == 0 || parts[i].Length > 9 || !int.TryParse(parts[i], out numbers[i]) || numbers[i] < 0) return null;
            return numbers;
        }

        /// &lt;0, 0 or &gt;0; null when either is not a version.
        public static int? Compare(string a, string b)
        {
            var x = Parse(a);
            var y = Parse(b);
            if (x == null || y == null) return null;
            for (int i = 0; i < 3; i++) if (x[i] != y[i]) return x[i].CompareTo(y[i]);
            return 0;
        }
    }
}
