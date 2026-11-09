using System;
using System.Collections.Generic;
using System.IO;
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
    /// on the one path it reported recording to.
    internal static class PartRecovery
    {
        public const string PartExt = ".part";

        /// The recovered file, or null when the recording was finished (no `.part` left).
        public static string Recover(string recording, Action<string> log)
        {
            if (string.IsNullOrEmpty(recording) || !recording.EndsWith(".mp4", StringComparison.OrdinalIgnoreCase)) return null;
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
