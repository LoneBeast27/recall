using System;
using System.Collections.Generic;
using System.IO;
using System.Text.RegularExpressions;
using System.Web.Script.Serialization;

namespace Recall.Setup
{
    /// What an uninstall removes, decided with no I/O. Program files: only manifest entries, only
    /// while unchanged, only inside the install folder, never inside or holding a clip folder. The
    /// install folder itself is never deleted recursively.
    internal static class UninstallPlan
    {
        /// Files to delete: manifest entries that are safe, inside, unprotected and unchanged.
        public static List<string> ProgramFiles(string installRoot, IEnumerable<ManifestEntry> manifest,
            IEnumerable<string> protectedRoots, Func<ManifestEntry, bool> unchanged)
        {
            var files = new List<string>();
            var guard = new List<string>(protectedRoots);
            foreach (var e in manifest)
            {
                if (!InstallManifest.IsSafeRelative(e.Path)) continue;
                var path = installRoot + e.Path;
                if (Allowed(path, installRoot, guard) && unchanged(e)) files.Add(path);
            }
            return files;
        }

        /// Strictly inside the install folder, and neither inside nor holding a protected folder.
        public static bool Allowed(string path, string installRoot, IEnumerable<string> protectedRoots)
        {
            var full = path.TrimEnd('\\');
            if (!PathId.IsInside(full, installRoot) || (full + "\\").Equals(installRoot, StringComparison.OrdinalIgnoreCase)) return false;
            if (full.Contains(@"\..\") || full.EndsWith(@"\..", StringComparison.Ordinal)) return false;
            foreach (var root in protectedRoots)
                if (PathId.IsInside(full, root) || PathId.IsInside(root.TrimEnd('\\'), full + "\\")) return false;
            return true;
        }
    }

    internal enum CacheFileKind { Regenerable, Recycle }

    /// Which cache files are provably regenerable, by the cache module's own name rules
    /// (recall-native services/storage budget.rs, unit_files.rs, proxy.rs): `thumbs\<id>.jpg`,
    /// `visuals\<id>\` frames (`.jpg`, `strip.json` and its `.part`), `editor\<id>\` engine files
    /// (`.flac`, `.wav`, `.json`, `.tmp`) and `proxy\<id>.mp4` preview copies, `<id>` being 16
    /// lowercase hex digits. Everything else (kept clip copies in `staging\kept`, copies listed in
    /// owned.json under `video\` and `light\`, projects, history, settings, heads, temporaries, any
    /// file someone put there) is never deleted: it goes to the Recycle Bin or stays.
    internal static class CachePlan
    {
        public static bool IsId(string text)
        {
            if (text == null || text.Length != 16) return false;
            foreach (var c in text) if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))) return false;
            return true;
        }

        /// <paramref name="relative"/>: the path under cache\, '\'-separated.
        public static CacheFileKind Classify(string relative)
        {
            var parts = relative.Split('\\');
            var name = parts[parts.Length - 1];
            var lower = name.ToLowerInvariant();
            if (parts.Length == 2 && parts[0] == "thumbs" && lower.EndsWith(".jpg", StringComparison.Ordinal)
                && IsId(name.Substring(0, name.Length - 4))) return CacheFileKind.Regenerable;
            if (parts.Length == 2 && parts[0] == "proxy" && name.EndsWith(".mp4", StringComparison.Ordinal)
                && IsId(name.Substring(0, name.Length - 4))) return CacheFileKind.Regenerable;
            if (parts.Length == 3 && parts[0] == "visuals" && IsId(parts[1])
                && (lower.EndsWith(".jpg", StringComparison.Ordinal) || lower == "strip.json"
                    || (lower.StartsWith("strip.json.", StringComparison.Ordinal) && lower.EndsWith(".part", StringComparison.Ordinal))))
                return CacheFileKind.Regenerable;
            if (parts.Length == 3 && parts[0] == "editor" && IsId(parts[1])
                && (lower.EndsWith(".flac", StringComparison.Ordinal) || lower.EndsWith(".wav", StringComparison.Ordinal)
                    || lower.EndsWith(".json", StringComparison.Ordinal) || lower.EndsWith(".tmp", StringComparison.Ordinal)))
                return CacheFileKind.Regenerable;
            return CacheFileKind.Recycle;
        }

        /// Splits every file under the cache (paths relative to it) into delete and recycle lists.
        public static void Split(IEnumerable<string> relativeFiles, List<string> regenerable, List<string> recycle)
        {
            foreach (var r in relativeFiles)
                (Classify(r) == CacheFileKind.Regenerable ? regenerable : recycle).Add(r);
        }
    }

    /// The clip folders the app knows (read only to exclude them): the recordings folder, imported
    /// and former recordings folders from %APPDATA%\Recall\folders.json, the recorder's
    /// recording.output_dir from %APPDATA%\recall-hdr\config.toml, and the default Videos\Recall.
    internal static class ClipFolders
    {
        public static List<string> Read()
        {
            var folders = new List<string> {
                Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyVideos), "Recall")
            };
            var appData = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
            try { folders.AddRange(FromFoldersJson(File.ReadAllText(Path.Combine(appData, @"Recall\folders.json")))); }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
            try { folders.AddRange(FromConfigToml(File.ReadAllText(Path.Combine(appData, @"recall-hdr\config.toml")))); }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
            var roots = new List<string>();
            foreach (var f in folders)
            {
                if (string.IsNullOrWhiteSpace(f)) continue;
                try { roots.Add(PathId.Root(Path.GetFullPath(f))); }
                catch (ArgumentException) { }
                catch (NotSupportedException) { }
            }
            return roots;
        }

        public static List<string> FromFoldersJson(string json)
        {
            var found = new List<string>();
            Dictionary<string, object> map;
            try { map = new JavaScriptSerializer().DeserializeObject(json) as Dictionary<string, object>; }
            catch (ArgumentException) { return found; }
            catch (InvalidOperationException) { return found; }
            if (map == null) return found;
            foreach (var key in new[] { "recordings", "imported", "former_recordings" })
            {
                object value;
                if (!map.TryGetValue(key, out value) || value == null) continue;
                var text = value as string;
                if (text != null) { found.Add(text); continue; }
                var list = value as object[];
                if (list != null) foreach (var item in list) if (item is string) found.Add((string)item);
            }
            return found;
        }

        public static List<string> FromConfigToml(string toml)
        {
            var found = new List<string>();
            foreach (Match m in Regex.Matches(toml, "^\\s*output_dir\\s*=\\s*(\"(?<basic>(?:[^\"\\\\]|\\\\.)*)\"|'(?<literal>[^']*)')", RegexOptions.Multiline))
            {
                if (m.Groups["literal"].Success && m.Groups["literal"].Length > 0) found.Add(m.Groups["literal"].Value);
                else if (m.Groups["basic"].Success) found.Add(Regex.Unescape(m.Groups["basic"].Value));
            }
            return found;
        }
    }
}
