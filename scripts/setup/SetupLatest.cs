using System;
using System.Collections;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Threading.Tasks;
using System.Web.Script.Serialization;

namespace Recall.Setup
{
    /// The latest published Recall, as the in-app updater sees it (recall-native update.rs): GitHub's
    /// latest release, with an uploaded RecallSetup.exe, whose tag is on main. Null when offline or
    /// when any check fails: Setup then compares against its own version only.
    internal static class LatestRelease
    {
        private const string Api = "https://api.github.com/repos/LoneBeast27/recall/";
        private const string Downloads = "https://github.com/LoneBeast27/recall/releases/download/";
        private const int TimeoutMs = 2000;
        /// The whole online check, DNS included: past it Setup goes on as if offline.
        public const int BudgetMs = 4000;
        private const int MaxBytes = 1 << 20;

        /// The latest release's version, its commit when SOURCE-REVISION.txt is readable, and
        /// whether that commit is strictly after this Setup's (GitHub's compare, as the app does).
        internal sealed class Info
        {
            public readonly string Version;
            public readonly string Revision;
            public readonly bool NewerBuild;
            public Info(string version, string revision, bool newerBuild) { Version = version; Revision = revision; NewerBuild = newerBuild; }
        }

        /// GitHub within <see cref="BudgetMs"/> (a slow DNS lookup too), else null: offline.
        public static Info Fetch(string setupRevision)
        {
            ServicePointManager.SecurityProtocol |= (SecurityProtocolType)3072;
            return Within(() => Fetch(Get, setupRevision), BudgetMs);
        }

        /// <paramref name="check"/>'s answer, or null when it takes longer than <paramref name="ms"/>
        /// (it runs on a background thread, which never keeps Setup open).
        internal static Info Within(Func<Info> check, int ms)
        {
            var task = Task.Run(check);
            try { return task.Wait(ms) ? task.Result : null; }
            catch (AggregateException) { return null; }
        }

        /// <paramref name="get"/> returns a body, null for a non-200 answer, or throws (tests pass their own).
        internal static Info Fetch(Func<string, string> get, string setupRevision)
        {
            try
            {
                var latest = get(Api + "releases/latest");
                var version = latest == null ? null : VersionOf(latest);
                if (version == null) return null;
                var comparison = get(Api + "compare/v" + version + "...main?per_page=1&page=2");
                if (comparison == null || !OnMain(comparison)) return null;
                var revision = RevisionOf(TryGet(get, Downloads + "v" + version + "/SOURCE-REVISION.txt"));
                var own = RevisionOf(setupRevision);
                bool newerBuild = false;
                if (revision != null && own != null && revision != own)
                {
                    // Only a published build after this Setup's counts as newer; ahead of it, or no answer: not.
                    var ancestry = TryGet(get, Api + "compare/" + own + "..." + revision + "?per_page=1&page=2");
                    newerBuild = ancestry != null && Ahead(ancestry);
                }
                return new Info(version, revision, newerBuild);
            }
            catch (WebException) { return null; }
            catch (IOException) { return null; }
            catch (InvalidOperationException) { return null; }
            catch (ArgumentException) { return null; }
            catch (NotSupportedException) { return null; }
            catch (FormatException) { return null; }
            catch (InvalidCastException) { return null; }
            catch (OverflowException) { return null; }
        }

        /// One optional request: any failure is "no answer".
        private static string TryGet(Func<string, string> get, string url)
        {
            try { return get(url); }
            catch (WebException) { return null; }
            catch (IOException) { return null; }
            catch (InvalidOperationException) { return null; }
            catch (NotSupportedException) { return null; }
        }

        /// The app's "Check for updates automatically" (recall-native update_prefs.rs): on unless
        /// <localAppData>\Recall\updates\settings.json says "check_automatically": false. Off, Setup
        /// never calls GitHub before its window.
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

        private static string Get(string url)
        {
            var request = (HttpWebRequest)WebRequest.Create(url);
            request.UserAgent = "RecallSetup";
            request.Accept = "application/vnd.github+json";
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
                        if (memory.Length > MaxBytes) return null;
                    }
                    return System.Text.Encoding.UTF8.GetString(memory.ToArray());
                }
            }
        }

        /// "v1.2.3" of a release that is not a draft or pre-release and has an uploaded Setup, else null.
        internal static string VersionOf(string json)
        {
            var release = Parse(json);
            if (release == null || True(release, "draft") || True(release, "prerelease")) return null;
            var tag = release.ContainsKey("tag_name") ? release["tag_name"] as string : null;
            if (tag == null || !tag.StartsWith("v", StringComparison.Ordinal) || SetupVersion.Parse(tag.Substring(1)) == null) return null;
            var assets = release.ContainsKey("assets") ? release["assets"] as IEnumerable : null;
            if (assets == null) return null;
            foreach (var item in assets)
            {
                var asset = item as Dictionary<string, object>;
                if (asset != null && asset.ContainsKey("name") && (asset["name"] as string) == AppsEntry.UninstallerName
                    && asset.ContainsKey("state") && (asset["state"] as string) == "uploaded")
                    return tag.Substring(1);
            }
            return null;
        }

        /// The second commit is strictly after the first (recall-native update_hotfix.rs `newer_build`).
        internal static bool Ahead(string json)
        {
            var comparison = Parse(json);
            if (comparison == null || !comparison.ContainsKey("status") || !comparison.ContainsKey("behind_by")) return false;
            return comparison["status"] as string == "ahead" && Convert.ToInt64(comparison["behind_by"]) == 0;
        }

        /// The tag is main or behind it (recall-native update.rs `on_main`).
        internal static bool OnMain(string json)
        {
            var comparison = Parse(json);
            if (comparison == null || !comparison.ContainsKey("status") || !comparison.ContainsKey("behind_by")) return false;
            var status = comparison["status"] as string;
            return (status == "ahead" || status == "identical") && Convert.ToInt64(comparison["behind_by"]) == 0;
        }

        private static Dictionary<string, object> Parse(string json)
        {
            try { return new JavaScriptSerializer { MaxJsonLength = MaxBytes }.DeserializeObject(json) as Dictionary<string, object>; }
            catch (ArgumentException) { return null; }
            catch (InvalidOperationException) { return null; }
        }

        private static bool True(Dictionary<string, object> map, string key)
        {
            return map.ContainsKey(key) && map[key] is bool && (bool)map[key];
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
