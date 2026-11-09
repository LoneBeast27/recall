using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Web.Script.Serialization;

namespace Recall.Setup
{
    /// `--resume &lt;file&gt;`: the file the Recall app wrote before it handed the update to Setup
    /// (recall-native update_resume.rs): the page and clip it showed and the recorder's state. Setup
    /// reads the recorder's state from it and writes the outcome back for the reopened app:
    /// `outcome` "updated" | "failed", `still_on` (the version still installed after a failure),
    /// `error`, `recovered` (a long recording the update cut short, recovered), `config_backup`.
    internal sealed class ResumeFile
    {
        public readonly string Path;
        private readonly Dictionary<string, object> data;

        private ResumeFile(string path, Dictionary<string, object> data) { Path = path; this.data = data; }

        /// Only a .json file directly in the updates folder is accepted (Setup writes to it).
        public static ResumeFile Open(string path, UserRoots roots)
        {
            if (string.IsNullOrEmpty(path)) return null;
            string full;
            try { full = System.IO.Path.GetFullPath(path); }
            catch (ArgumentException) { return null; }
            catch (NotSupportedException) { return null; }
            var folder = PathId.Root(roots.Updates);
            if (!PathId.IsInside(PathId.Normalize(full), folder) || !string.Equals(PathId.Root(System.IO.Path.GetDirectoryName(full)), folder, StringComparison.OrdinalIgnoreCase)
                || !full.EndsWith(".json", StringComparison.OrdinalIgnoreCase) || !File.Exists(full))
                return null;
            Dictionary<string, object> data = null;
            try { data = new JavaScriptSerializer().DeserializeObject(File.ReadAllText(full)) as Dictionary<string, object>; }
            catch (ArgumentException) { }
            catch (InvalidOperationException) { }
            return new ResumeFile(full, data ?? new Dictionary<string, object>());
        }

        private Dictionary<string, object> Recorder
        {
            get { object r; return data.TryGetValue("recorder", out r) ? r as Dictionary<string, object> : null; }
        }

        private bool RecorderFlag(string key)
        {
            var r = Recorder;
            object v;
            return r != null && r.TryGetValue(key, out v) && true.Equals(v);
        }

        public bool LongOn { get { return RecorderFlag("long_on"); } }

        public string FromVersion
        {
            get { object v; return data.TryGetValue("from_version", out v) ? v as string : null; }
        }

        public void Set(string key, object value) { data[key] = value; }

        /// Written whole (temp file, then a replace): the app never reads half an outcome.
        public void Save()
        {
            var temp = Path + ".tmp";
            File.WriteAllText(temp, new JavaScriptSerializer().Serialize(data), new UTF8Encoding(false));
            if (File.Exists(Path)) File.Replace(temp, Path, null);
            else File.Move(temp, Path);
        }
    }

    /// A long recording the quit could not finish (the recorder's 60 s give-up left its `.part`):
    /// renamed to its own name, or `&lt;stem&gt;-recovered.mp4` when that is taken, as the engine's own
    /// start-up recovery does (recall longrec.rs recover_parts). Run only after the recorder exited,
    /// on the one path it reported recording to, and only for a long recording's name
    /// (`*__rec.mp4` / `*__rec-N.mp4`) inside a configured recordings folder with no junction or
    /// link on the way.
    internal static class PartRecovery
    {
        public const string PartExt = ".part";

        /// The recordings folders config.toml names (`recording.output_dir`).
        public static List<string> RecordingFolders(UserRoots roots)
        {
            var config = System.IO.Path.Combine(roots.Roaming, @"recall-hdr\config.toml");
            try { return File.Exists(config) ? ClipFolders.FromConfigToml(File.ReadAllText(config)) : new List<string>(); }
            catch (IOException) { return new List<string>(); }
            catch (UnauthorizedAccessException) { return new List<string>(); }
        }

        /// A long recording's file name, as the engine names it (recall naming.rs is_longrec_part).
        public static bool IsLongRecording(string name)
        {
            if (!name.EndsWith(".mp4", StringComparison.OrdinalIgnoreCase)) return false;
            var stem = name.Substring(0, name.Length - 4);
            int dash = stem.LastIndexOf('-');
            if (dash > 0 && dash < stem.Length - 1 && stem.Substring(dash + 1).All(char.IsDigit)) stem = stem.Substring(0, dash);
            return stem.EndsWith("__rec", StringComparison.OrdinalIgnoreCase);
        }

        /// Why `recording` may not be renamed here, else null.
        public static string Refusal(string recording, List<string> folders)
        {
            if (string.IsNullOrEmpty(recording) || !IsLongRecording(System.IO.Path.GetFileName(recording))) return "not a long recording's name";
            string full;
            try { full = System.IO.Path.GetFullPath(recording); }
            catch (ArgumentException) { return "not a valid path"; }
            catch (NotSupportedException) { return "not a valid path"; }
            string folder = null;
            foreach (var f in folders)
            {
                string root;
                try { root = System.IO.Path.GetFullPath(f).TrimEnd('\\') + "\\"; }
                catch (ArgumentException) { continue; }
                catch (NotSupportedException) { continue; }
                if (full.StartsWith(root, StringComparison.OrdinalIgnoreCase)) { folder = root; break; }
            }
            if (folder == null) return "not inside a recordings folder";
            // No junction, symlink or mount point from the folder down to the file.
            for (var p = full + PartExt; p != null && p.Length >= folder.Length - 1; p = System.IO.Path.GetDirectoryName(p))
            {
                if ((File.Exists(p) || Directory.Exists(p)) && (File.GetAttributes(p) & FileAttributes.ReparsePoint) != 0) return "a link on the way: " + p;
                if (string.Equals(p.TrimEnd('\\') + "\\", folder, StringComparison.OrdinalIgnoreCase)) break;
            }
            return null;
        }

        /// The recovered file, or null when the recording was finished (no `.part` left) or refused.
        public static string Recover(string recording, List<string> folders, Action<string> log)
        {
            var why = Refusal(recording, folders);
            if (why != null)
            {
                log("WARNING: the cut-short recording " + recording + " was not recovered here (" + why + "); Recall recovers it at its next start.");
                return null;
            }
            var part = recording + PartExt;
            if (!File.Exists(part)) return null;
            var target = recording;
            if (File.Exists(target))
            {
                var stem = System.IO.Path.Combine(System.IO.Path.GetDirectoryName(recording), System.IO.Path.GetFileNameWithoutExtension(recording));
                target = stem + "-recovered.mp4";
                for (int n = 2; File.Exists(target); n++) target = stem + "-recovered-" + n + ".mp4";
            }
            File.Move(part, target);
            log("The update cut a long recording short; recovered " + target);
            return target;
        }
    }
}
