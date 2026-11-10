using System;
using System.Collections.Generic;
using System.IO;
using System.Web.Script.Serialization;

namespace Recall.Setup
{
    /// One file Setup put in the install folder: its path relative to the folder, size and SHA-256.
    internal sealed class ManifestEntry
    {
        public string Path;
        public long Size;
        public string Sha256;
    }

    /// `install-manifest.json` in the install folder: every file Setup installed. Uninstall removes
    /// only these, and only while they are unchanged. An update rewrites it: the new files, plus
    /// earlier entries whose files are still there unchanged (so nothing Setup put there leaks).
    internal static class InstallManifest
    {
        public const string Name = "install-manifest.json";

        public static string PathIn(string dir) { return System.IO.Path.Combine(dir, Name); }

        /// A relative path that stays inside the folder: no drive, no root, no "..", no wildcards.
        public static bool IsSafeRelative(string relative)
        {
            if (string.IsNullOrWhiteSpace(relative) || relative != relative.Trim()) return false;
            if (relative.IndexOfAny(new[] { ':', '*', '?', '"', '<', '>', '|', '/' }) >= 0) return false;
            if (relative.StartsWith("\\", StringComparison.Ordinal) || relative.EndsWith("\\", StringComparison.Ordinal)) return false;
            foreach (var part in relative.Split('\\'))
                if (part.Length == 0 || part == "." || part == "..") return false;
            return !relative.Equals(Name, StringComparison.OrdinalIgnoreCase);
        }

        public static List<ManifestEntry> Describe(string folder)
        {
            var list = new List<ManifestEntry>();
            foreach (var file in Directory.GetFiles(folder))
                list.Add(new ManifestEntry {
                    Path = System.IO.Path.GetFileName(file), Size = new FileInfo(file).Length, Sha256 = InstallSwap.Sha256(file)
                });
            return list;
        }

        public static List<ManifestEntry> Read(string dir)
        {
            var path = PathIn(dir);
            if (!File.Exists(path)) return null;
            try
            {
                var root = new JavaScriptSerializer().DeserializeObject(File.ReadAllText(path)) as Dictionary<string, object>;
                object files;
                if (root == null || !root.TryGetValue("files", out files) || !(files is object[])) return null;
                var list = new List<ManifestEntry>();
                foreach (var item in (object[])files)
                {
                    var map = item as Dictionary<string, object>;
                    if (map == null) continue;
                    object p, s, h;
                    if (!map.TryGetValue("path", out p) || !map.TryGetValue("size", out s) || !map.TryGetValue("sha256", out h)) continue;
                    if (!(p is string) || !(h is string)) continue;
                    list.Add(new ManifestEntry { Path = (string)p, Size = Convert.ToInt64(s), Sha256 = (string)h });
                }
                return list;
            }
            catch (ArgumentException) { return null; }
            catch (InvalidOperationException) { return null; }
            catch (FormatException) { return null; }
        }

        /// The new install's files, plus earlier entries still on disk unchanged and not replaced.
        public static List<ManifestEntry> Merge(List<ManifestEntry> installed, List<ManifestEntry> earlier, Func<ManifestEntry, bool> unchanged)
        {
            var merged = new List<ManifestEntry>(installed);
            var names = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (var e in installed) names.Add(e.Path);
            if (earlier != null)
                foreach (var e in earlier)
                    if (IsSafeRelative(e.Path) && !names.Contains(e.Path) && unchanged(e)) { merged.Add(e); names.Add(e.Path); }
            return merged;
        }

        public static void Write(string dir, List<ManifestEntry> entries)
        {
            var files = new List<object>();
            foreach (var e in entries)
                files.Add(new Dictionary<string, object> { { "path", e.Path }, { "size", e.Size }, { "sha256", e.Sha256 } });
            var json = new JavaScriptSerializer().Serialize(new Dictionary<string, object> {
                { "product", "Recall" }, { "version", SetupBuildInfo.Version }, { "files", files }
            });
            var path = PathIn(dir);
            var temp = path + ".tmp";
            File.WriteAllText(temp, json);
            if (File.Exists(path)) File.Replace(temp, path, null);
            else File.Move(temp, path);
        }

        /// Is the file at <paramref name="dir"/>\entry exactly what Setup installed?
        public static bool Unchanged(string dir, ManifestEntry entry)
        {
            if (!IsSafeRelative(entry.Path)) return false;
            var path = System.IO.Path.Combine(dir, entry.Path);
            if (!File.Exists(path)) return false;
            try
            {
                return new FileInfo(path).Length == entry.Size
                    && InstallSwap.Sha256(path).Equals(entry.Sha256, StringComparison.OrdinalIgnoreCase);
            }
            catch (IOException) { return false; }
            catch (UnauthorizedAccessException) { return false; }
        }
    }
}
