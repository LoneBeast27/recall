using System;
using System.Collections;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Web.Script.Serialization;

namespace Recall.Setup
{
    /// The latest published Recall, as the in-app updater sees it (recall-native update.rs): GitHub's
    /// latest release, with an uploaded RecallSetup.exe, whose tag is on main. Null when offline or
    /// when any check fails: Setup then compares against its own version only.
    internal static class LatestRelease
    {
        private const string Api = "https://api.github.com/repos/LoneBeast27/recall/";
        private const int TimeoutMs = 2000;
        private const int MaxBytes = 1 << 20;

        public static string Fetch()
        {
            try
            {
                ServicePointManager.SecurityProtocol |= (SecurityProtocolType)3072;
                var latest = Get("releases/latest");
                var version = latest == null ? null : VersionOf(latest);
                if (version == null) return null;
                var comparison = Get("compare/v" + version + "...main?per_page=1&page=2");
                return comparison != null && OnMain(comparison) ? version : null;
            }
            catch (WebException) { return null; }
            catch (IOException) { return null; }
            catch (InvalidOperationException) { return null; }
            catch (ArgumentException) { return null; }
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

        private static string Get(string path)
        {
            var request = (HttpWebRequest)WebRequest.Create(Api + path);
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
