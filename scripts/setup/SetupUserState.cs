using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Text;

namespace Recall.Setup
{
    /// The per-user folders Recall keeps its settings in: %APPDATA% and %LOCALAPPDATA%, or, in a
    /// sandboxed run (RECALL_SETUP_SANDBOX_ROOT), &lt;sandbox&gt;\AppData\Roaming and \Local.
    internal sealed class UserRoots
    {
        public readonly string Roaming;
        public readonly string Local;

        public UserRoots(string roaming, string local) { Roaming = roaming; Local = local; }

        public static UserRoots For(SetupTargets targets)
        {
            if (targets.IsSandbox)
                return new UserRoots(Path.Combine(targets.SandboxRoot, @"AppData\Roaming"), Path.Combine(targets.SandboxRoot, @"AppData\Local"));
            return new UserRoots(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData));
        }

        /// %LOCALAPPDATA%\Recall\updates: the app's resume file and Setup's settings backups.
        public string Updates { get { return Path.Combine(Local, @"Recall\updates"); } }
    }

    /// The user's settings, copied before an update replaces anything and copied back when the update
    /// rolls back (`config-backup-&lt;version&gt;-&lt;yyyyMMdd-HHmmss&gt;` under the updates folder, the 3
    /// newest kept). The update itself never writes any of them; the new build reads them as they are
    /// and, when one no longer parses, restores it from here with a notice (recall-native
    /// update_restore.rs, recall-hdr config_io.rs).
    internal static class ConfigBackup
    {
        public const string Prefix = "config-backup-";
        public const string ListName = "backup-files.txt";
        public const int Keep = 3;

        /// (root, path under it) for every settings file Recall writes.
        internal static readonly string[][] Files = {
            new[] { "roaming", @"recall-hdr\config.toml" }, new[] { "roaming", @"recall-hdr\config.toml.last-good" },
            new[] { "roaming", @"recall-hdr\state.json" }, new[] { "roaming", @"Recall\library.json" },
            new[] { "roaming", @"Recall\folders.json" }, new[] { "roaming", @"Recall\imports.json" },
            new[] { "roaming", @"Recall\recorded.json" }, new[] { "roaming", @"Recall\open-clips.json" },
            new[] { "roaming", @"Recall\editor.json" }, new[] { "local", @"Recall\updates\settings.json" },
            new[] { "local", @"Recall\updates\skipped.txt" }, new[] { "local", @"io.github.lonebeast27.recall-hub\hub-settings.json" },
            new[] { "install", @"cache\storage.json" }
        };

        private static string Root(string key, UserRoots roots, string installDir)
        {
            return key == "roaming" ? roots.Roaming : key == "local" ? roots.Local : installDir;
        }

        /// Copies every settings file that exists; returns the backup folder. The 3 newest backups stay.
        public static string Create(UserRoots roots, string installDir, string version, DateTime now, Action<string> log)
        {
            Directory.CreateDirectory(roots.Updates);
            var stamp = now.ToString("yyyyMMdd-HHmmss", CultureInfo.InvariantCulture);
            var dir = Path.Combine(roots.Updates, Prefix + Safe(version) + "-" + stamp);
            for (int n = 2; Directory.Exists(dir); n++) dir = Path.Combine(roots.Updates, Prefix + Safe(version) + "-" + stamp + "-" + n);
            Directory.CreateDirectory(dir);
            var listed = new StringBuilder();
            foreach (var file in Files)
            {
                var from = Path.Combine(Root(file[0], roots, installDir), file[1]);
                if (!File.Exists(from)) continue;
                var to = Path.Combine(dir, file[0], file[1]);
                Directory.CreateDirectory(Path.GetDirectoryName(to));
                File.Copy(from, to, false);
                if (!InstallSwap.Sha256(to).Equals(InstallSwap.Sha256(from), StringComparison.Ordinal))
                    throw new InvalidOperationException("The settings backup of " + file[1] + " does not match. Nothing was changed.");
                listed.Append(file[0]).Append('\t').Append(file[1]).Append('\n');
            }
            File.WriteAllText(Path.Combine(dir, ListName), listed.ToString(), new UTF8Encoding(false));
            log("Settings backed up to " + dir);
            Prune(roots.Updates, log);
            return dir;
        }

        /// A settings file Recall can read: a .json file holding a JSON object; any other file not
        /// empty, valid UTF-8 and free of NULs (a power cut leaves them NUL-filled or empty).
        internal static bool Readable(string path)
        {
            if (!File.Exists(path)) return false;
            byte[] bytes;
            try { bytes = File.ReadAllBytes(path); }
            catch (IOException) { return true; } // held by someone: never overwrite it
            catch (UnauthorizedAccessException) { return true; }
            if (bytes.Length == 0 || Array.IndexOf(bytes, (byte)0) >= 0) return false;
            string text;
            try { text = new UTF8Encoding(false, true).GetString(bytes); }
            catch (ArgumentException) { return false; }
            if (!path.EndsWith(".json", StringComparison.OrdinalIgnoreCase)) return true;
            try { return new System.Web.Script.Serialization.JavaScriptSerializer().DeserializeObject(text) is Dictionary<string, object>; }
            catch (ArgumentException) { return false; }
            catch (InvalidOperationException) { return false; }
        }

        /// Puts back each backed-up file that is now missing or unreadable (an update that rolled
        /// back). A readable file is never replaced: it may be newer than the backup. Returns the
        /// files restored.
        public static int Restore(string backup, UserRoots roots, string installDir, Action<string> log)
        {
            int restored = 0;
            foreach (var line in File.ReadAllText(Path.Combine(backup, ListName)).Split('\n'))
            {
                var parts = line.Split('\t');
                if (parts.Length != 2 || !InstallManifest.IsSafeRelative(parts[1])) continue;
                if (parts[0] != "roaming" && parts[0] != "local" && parts[0] != "install") continue;
                var from = Path.Combine(backup, parts[0], parts[1]);
                var to = Path.Combine(Root(parts[0], roots, installDir), parts[1]);
                if (!File.Exists(from) || Readable(to)) continue;
                Directory.CreateDirectory(Path.GetDirectoryName(to));
                // An unreadable file is set aside, never overwritten: its bytes may still matter.
                // A file that cannot be set aside is left alone and not restored.
                if (File.Exists(to))
                {
                    string aside;
                    try { aside = SetAside(to); }
                    catch (IOException e) { log("WARNING: " + to + " could not be set aside (" + e.Message + "); not restored"); continue; }
                    catch (UnauthorizedAccessException e) { log("WARNING: " + to + " could not be set aside (" + e.Message + "); not restored"); continue; }
                    log("Unreadable settings file kept as " + aside);
                }
                File.Copy(from, to, false);
                log("Settings file restored (it was missing or unreadable): " + to);
                restored++;
            }
            log("Settings restored from " + backup + " (" + restored + " files)");
            return restored;
        }

        /// Renames <paramref name="path"/> to "<name>.unreadable-yyyyMMdd-HHmmss[-n]" beside it and
        /// returns the new path. Throws when the rename fails.
        internal static string SetAside(string path)
        {
            var stamp = DateTime.Now.ToString("yyyyMMdd-HHmmss", CultureInfo.InvariantCulture);
            var aside = path + ".unreadable-" + stamp;
            for (int n = 2; File.Exists(aside) || Directory.Exists(aside); n++) aside = path + ".unreadable-" + stamp + "-" + n;
            File.Move(path, aside);
            return aside;
        }

        /// The backups, newest first (by the time in their names).
        public static List<string> Backups(string updates)
        {
            var list = new List<string>();
            if (!Directory.Exists(updates)) return list;
            foreach (var d in Directory.GetDirectories(updates, Prefix + "*")) if (Stamp(d) != null) list.Add(d);
            list.Sort((a, b) => string.CompareOrdinal(Stamp(b), Stamp(a)));
            return list;
        }

        private static void Prune(string updates, Action<string> log)
        {
            var all = Backups(updates);
            for (int i = Keep; i < all.Count; i++)
            {
                try { Directory.Delete(all[i], true); log("Older settings backup removed: " + all[i]); }
                catch (IOException) { }
                catch (UnauthorizedAccessException) { }
            }
        }

        /// "yyyyMMdd-HHmmss[-n]" at the end of a backup folder's name, else null.
        internal static string Stamp(string dir)
        {
            var name = Path.GetFileName(dir);
            var parts = name.Substring(Prefix.Length).Split('-');
            for (int i = 0; i + 1 < parts.Length; i++)
            {
                if (parts[i].Length == 8 && parts[i + 1].Length == 6 && IsDigits(parts[i]) && IsDigits(parts[i + 1]))
                {
                    var n = i + 2 < parts.Length && IsDigits(parts[i + 2]) ? parts[i + 2].PadLeft(4, '0') : "0000";
                    return parts[i] + parts[i + 1] + n;
                }
            }
            return null;
        }

        private static bool IsDigits(string s)
        {
            if (s.Length == 0) return false;
            foreach (var c in s) if (c < '0' || c > '9') return false;
            return true;
        }

        private static string Safe(string version)
        {
            var b = new StringBuilder();
            foreach (var c in version ?? "") if (char.IsLetterOrDigit(c) || c == '.') b.Append(c);
            return b.Length == 0 ? "0.0.0" : b.ToString();
        }
    }
}
