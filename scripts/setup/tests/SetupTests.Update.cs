using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;

namespace Recall.Setup.Tests
{
    /// The coupled update in a sandbox (RECALL_SETUP_SANDBOX_ROOT): fake Recall programs
    /// (tests/fake/FakeRecall.cs), a fake package, sandbox pipes and per-user folders. Nothing real
    /// is closed, read or written; no window shows.
    internal static partial class TestRunner
    {
        private static string fakeExe;

        private sealed class Sandbox : IDisposable
        {
            public readonly string Root, Install, Package, Work, Log, Clips, Roaming, Local;
            private readonly Dictionary<string, string> saved = new Dictionary<string, string>();

            public Sandbox()
            {
                Root = Path.Combine(Path.GetDirectoryName(fakeExe), "sandbox-" + Guid.NewGuid().ToString("N").Substring(0, 12));
                Install = Path.Combine(Root, @"Programs\Recall");
                Package = Path.Combine(Root, "package");
                Work = Path.Combine(Root, "work");
                Log = Path.Combine(Root, "fake.log");
                Clips = Path.Combine(Root, "Clips");
                Roaming = Path.Combine(Root, @"AppData\Roaming");
                Local = Path.Combine(Root, @"AppData\Local");
                foreach (var d in new[] { Install, Package, Work, Clips, Roaming, Local }) Directory.CreateDirectory(d);
                var pipe = "recall-sandbox-test-" + Guid.NewGuid().ToString("N");
                Env(SetupTargets.SandboxVariable, Root);
                Env(SetupTargets.ControllerVariable, Path.Combine(Install, "recall-hdr.exe"));
                Env(SetupTargets.PipeVariable, pipe);
                Env(SetupTargets.LegacyPipeVariable, pipe + "-legacy");
                Env("FAKE_LOG", Log);
                foreach (var k in new[] { "FAKE_FINISHING_POLLS", "FAKE_LONG_PATH", "FAKE_FINALIZE", "FAKE_TOUCH", "FAKE_TOUCH_TEXT", "RECALL_SETUP" }) Env(k, null);
                UpdateFlow.CaptureWaitSeconds = 5;
                UpdateFlow.BeforeBackup = null;
                Installer.AfterSwap = null;
                foreach (var name in Programs) Fake(Path.Combine(Install, name), "OLD");
                foreach (var name in Others) File.WriteAllText(Path.Combine(Install, Path.GetFileName(name)), "old " + name);
                FakePackage();
            }

            public static readonly string[] Programs = { "recall-hdr.exe", "recall-native.exe", "recall.exe", "recall-overlay.exe" };
            private static readonly string[] Others = {
                @"tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared\bin\ffmpeg.exe", @"tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared\bin\ffprobe.exe",
                @"recall-native\assets\recall.ico", "LICENSE", "NOTICE.md", @"recall-native\assets\fonts\FONTS-LICENSE.txt",
                @"recall-native\assets\icons\LICENSE-lucide.txt", @"recall-native\assets\icons\brand-attribution.txt", "FFMPEG-LICENSE.txt"
            };

            private void FakePackage()
            {
                Fake(Path.Combine(Package, @"target-gpui\release\recall-native.exe"), "NEW");
                foreach (var exe in new[] { "recall-hdr.exe", "recall.exe", "recall-overlay.exe" }) Fake(Path.Combine(Package, @"target-dev\release\" + exe), "NEW");
                foreach (var name in Others)
                {
                    var to = Path.Combine(Package, name);
                    Directory.CreateDirectory(Path.GetDirectoryName(to));
                    File.WriteAllText(to, "new " + name);
                }
            }

            /// The fake exe with a trailer naming its version (the loader ignores trailing bytes).
            private static void Fake(string to, string version)
            {
                Directory.CreateDirectory(Path.GetDirectoryName(to));
                File.Copy(fakeExe, to, true);
                using (var f = new FileStream(to, FileMode.Append)) { var b = System.Text.Encoding.ASCII.GetBytes("\n" + version); f.Write(b, 0, b.Length); }
            }

            public string Version(string name)
            {
                var bytes = File.ReadAllBytes(Path.Combine(Install, name));
                return System.Text.Encoding.ASCII.GetString(bytes, bytes.Length - 3, 3);
            }

            public void Env(string key, string value)
            {
                if (!saved.ContainsKey(key)) saved[key] = Environment.GetEnvironmentVariable(key);
                Environment.SetEnvironmentVariable(key, value);
            }

            /// A config.toml naming the sandbox's recordings folder.
            public string Config(int buffer) { return "[recording]\noutput_dir = '" + Clips + "'\nbuffer_secs = " + buffer + "\n"; }

            public string SetupLog() { var p = Path.Combine(Root, "setup.log"); return File.Exists(p) ? File.ReadAllText(p) : ""; }

            public string[] Lines()
            {
                for (int i = 0; ; i++)
                {
                    try { return File.Exists(Log) ? File.ReadAllLines(Log) : new string[0]; }
                    catch (IOException) { if (i > 20) throw; System.Threading.Thread.Sleep(50); }
                }
            }

            /// The log once the reopened app has written its lines (it starts detached), up to 15 s.
            public string[] LinesAfterApp()
            {
                var until = DateTime.UtcNow.AddSeconds(15);
                while (DateTime.UtcNow < until && !Lines().Any(l => l.StartsWith("recall-native.exe ") && l.Contains("RECALL_SETUP=")))
                    System.Threading.Thread.Sleep(100);
                return Lines();
            }

            /// Starts the installed (old) recorder detached, as a running Recall would have it.
            public void StartRecorder()
            {
                using (var p = DetachedProcess.Start(Path.Combine(Install, "recall-hdr.exe"), "--record", null)) { }
                var until = DateTime.UtcNow.AddSeconds(10);
                while (DateTime.UtcNow < until && !Lines().Any(l => l.StartsWith("recall-hdr.exe "))) System.Threading.Thread.Sleep(100);
                System.Threading.Thread.Sleep(500);
            }

            public string WriteResume(bool longOn)
            {
                var dir = Path.Combine(Local, @"Recall\updates");
                Directory.CreateDirectory(dir);
                var path = Path.Combine(dir, "resume.json");
                File.WriteAllText(path, "{\"version\":1,\"route\":\"editor\",\"clip\":\"C:\\\\clip.mp4\",\"from_version\":\"0.1.0\",\"recorder\":{\"running\":true,\"long_on\":" + (longOn ? "true" : "false") + "}}");
                return path;
            }

            public Dictionary<string, object> Resume(string path)
            {
                return new System.Web.Script.Serialization.JavaScriptSerializer().DeserializeObject(File.ReadAllText(path)) as Dictionary<string, object>;
            }

            public string Run(string resume)
            {
                var options = new InstallOptions(Install, false, false, false, true, false, true);
                options.Resume = resume;
                try { Installer.Run(Package, Work, options, null); return null; }
                catch (Exception e) { return e.Message; }
            }

            public void Dispose()
            {
                // Our own fakes only, by the pids they logged.
                foreach (var line in Lines())
                {
                    var parts = line.Split(' ');
                    int pid;
                    if (parts.Length > 1 && int.TryParse(parts[1], out pid))
                        try { using (var p = System.Diagnostics.Process.GetProcessById(pid)) if (p.MainModule.FileName.StartsWith(Root, StringComparison.OrdinalIgnoreCase)) p.Kill(); }
                        catch (Exception) { }
                }
                foreach (var kv in saved) Environment.SetEnvironmentVariable(kv.Key, kv.Value);
                UpdateFlow.BeforeBackup = null;
                Installer.AfterSwap = null;
                UpdateFlow.BeforeBackup = null;
                Installer.AfterSwap = null;
                try { Microsoft.Win32.Registry.CurrentUser.DeleteSubKeyTree(SetupTargets.SandboxRegistryBase + @"\Uninstall\Recall", false); }
                catch (Exception) { }
                for (int i = 0; i < 20 && Directory.Exists(Root); i++)
                {
                    try { Directory.Delete(Root, true); }
                    catch (IOException) { System.Threading.Thread.Sleep(250); }
                    catch (UnauthorizedAccessException) { System.Threading.Thread.Sleep(250); }
                }
            }
        }

        private static int Count(string[] lines, string what) { return lines.Count(l => l.Contains(what)); }

        /// (a) + (e): an update while a long recording runs.
        private static void UpdateWhileRecording()
        {
            using (var box = new Sandbox())
            {
                var recording = Path.Combine(box.Clips, "Game__2026-11-10__12-00-00__rec.mp4");
                File.WriteAllText(recording + ".part", "fragments");
                box.Env("FAKE_LONG_PATH", recording);
                box.Env("FAKE_FINALIZE", "1");
                box.Env("FAKE_FINISHING_POLLS", "3");
                File.WriteAllText(Path.Combine(box.Roaming, "config-marker.txt"), "unrelated");
                Directory.CreateDirectory(Path.Combine(box.Roaming, "recall-hdr"));
                File.WriteAllText(Path.Combine(box.Roaming, @"recall-hdr\config.toml"), box.Config(60));
                box.StartRecorder();
                var resume = box.WriteResume(true);
                var error = box.Run(resume);
                Check(error == null, "the update ran: " + error);
                var lines = box.LinesAfterApp();
                var log = string.Join(" | ", lines);
                Check(Count(lines, "line {\"cmd\":\"quit\"") == 1, "exactly one quit: " + log);
                Check(Count(lines, "save_first\":true") == 1 && Count(lines, "force") == 0, "a save_first quit, never a forced one: " + log);
                Check(Count(lines, "exit intact") == 1 && Count(lines, "exit changed") == 0, "no file moved before the recorder exited: " + log);
                Check(Sandbox.Programs.All(p => box.Version(p) == "NEW"), "the new programs are in");
                Check(File.Exists(recording) && !File.Exists(recording + ".part"), "the long recording was saved by the quit");
                var hdr = lines.Where(l => l.StartsWith("recall-hdr.exe ") && l.Contains(" start ")).Select(l => l.Split(' ')[1]).Distinct().ToList();
                Check(hdr.Count == 2, "the recorder was started again: " + log);
                Check(lines.Any(l => l.StartsWith("recall-hdr.exe " + hdr[1] + " line {\"cmd\":\"record_start\"}")), "the new recorder got record_start: " + log);
                Check(lines.Any(l => l.StartsWith("recall-native.exe ") && l.Contains("start --resume") && l.Contains("resume.json")), "the app reopened with --resume: " + log + " SETUP LOG: " + box.SetupLog());
                Check(lines.Any(l => l.StartsWith("recall-native.exe ") && l.EndsWith("RECALL_SETUP=-")), "no guided setup on an update");
                var outcome = box.Resume(resume);
                Check("updated".Equals(outcome["outcome"]) && outcome["recovered"] == null, "outcome: updated");
                var backups = ConfigBackup.Backups(Path.Combine(box.Local, @"Recall\updates"));
                Check(backups.Count == 1 && File.Exists(Path.Combine(backups[0], @"roaming\recall-hdr\config.toml")), "settings backed up in the sandbox");
                Check(File.ReadAllText(Path.Combine(box.Roaming, @"recall-hdr\config.toml")) == box.Config(60), "the update never wrote the settings");
            }
        }

        /// A failed swap: the settings come back, the OLD pair reopens and the app is told; a long
        /// recording the quit gave up on is recovered.
        private static void UpdateRollsBack()
        {
            using (var box = new Sandbox())
            {
                var recording = Path.Combine(box.Clips, "Game__2026-11-10__13-00-00__rec.mp4");
                File.WriteAllText(recording + ".part", "fragments");
                box.Env("FAKE_LONG_PATH", recording);
                box.Env("FAKE_FINISHING_POLLS", "1");
                var config = Path.Combine(box.Roaming, @"recall-hdr\config.toml");
                Directory.CreateDirectory(Path.GetDirectoryName(config));
                File.WriteAllText(config, box.Config(45));
                // The recorder saves its settings as it quits: newer than anything before the quit.
                box.Env("FAKE_TOUCH", config);
                box.Env("FAKE_TOUCH_TEXT", box.Config(90));
                box.StartRecorder();
                var resume = box.WriteResume(true);
                string error;
                using (new FileStream(Path.Combine(box.Install, "recall.ico"), FileMode.Open, FileAccess.Read, FileShare.None))
                    error = box.Run(resume);
                Check(error != null && error.Contains("rolled back"), "the update failed and rolled back: " + error);
                var log = string.Join(" | ", box.Lines());
                Check(Sandbox.Programs.All(p => box.Version(p) == "OLD"), "the old programs stayed: " + log);
                Check(File.ReadAllText(config) == box.Config(90), "the recorder's newer settings stayed (a readable file is never restored over)");
                Check(File.Exists(recording) && !File.Exists(recording + ".part"), "the cut-short recording was recovered");
                var outcome = box.Resume(resume);
                Check("failed".Equals(outcome["outcome"]) && "0.1.0".Equals(outcome["still_on"]), "the app is told it is still on 0.1.0");
                Check(recording.Equals(outcome["recovered"]), "and that the recording was recovered");
                var lines = box.LinesAfterApp();
                Check(lines.Count(l => l.StartsWith("recall-hdr.exe ") && l.Contains(" start ")) == 2, "the old recorder was started again: " + log);
                Check(lines.Any(l => l.StartsWith("recall-native.exe ") && l.Contains("start --resume")), "the old app reopened: " + log);
                Check(!File.Exists(SwapJournal.PathIn(box.Install)), "no journal left");
            }
        }

        private static void ResumeFileGuard()
        {
            var roots = new UserRoots(@"L:\nowhere\Roaming", Path.GetTempPath());
            Check(ResumeFile.Open(@"C:\Windows\win.ini", roots) == null, "a file outside the updates folder is refused");
            Check(ResumeFile.Open(null, roots) == null, "none given");
        }

        private static void BackupsKeepThree()
        {
            var root = Path.Combine(Path.GetTempPath(), "recall-setup-backup-test-" + Guid.NewGuid().ToString("N"));
            try
            {
                var roots = new UserRoots(Path.Combine(root, "Roaming"), Path.Combine(root, "Local"));
                var install = Path.Combine(root, "install");
                Directory.CreateDirectory(Path.Combine(roots.Roaming, "Recall"));
                Directory.CreateDirectory(Path.Combine(install, "cache"));
                File.WriteAllText(Path.Combine(roots.Roaming, @"Recall\library.json"), "{\"a\":{\"favourite\":true}}");
                File.WriteAllText(Path.Combine(install, @"cache\storage.json"), "{\"cache_budget_bytes\":5}");
                var t0 = new DateTime(2026, 11, 10, 12, 0, 0);
                string last = null;
                for (int i = 0; i < 5; i++) last = ConfigBackup.Create(roots, install, "0.1." + i, t0.AddMinutes(i), s => { });
                var all = ConfigBackup.Backups(roots.Updates);
                Check(all.Count == 3 && all[0] == last, "the 3 newest stay: " + string.Join(",", all.ToArray()));
                File.WriteAllText(Path.Combine(roots.Roaming, @"Recall\library.json"), "garbage");
                File.Delete(Path.Combine(install, @"cache\storage.json"));
                Check(ConfigBackup.Restore(last, roots, install, s => { }) == 2, "the unreadable and the missing file are restored");
                File.WriteAllText(Path.Combine(roots.Roaming, @"Recall\library.json"), "{\"newer\":{}}");
                Check(ConfigBackup.Restore(last, roots, install, s => { }) == 0, "a readable file is never replaced");
                Check(File.ReadAllText(Path.Combine(roots.Roaming, @"Recall\library.json")) == "{\"newer\":{}}", "the newer file stays");
                File.WriteAllText(Path.Combine(roots.Roaming, @"Recall\library.json"), "{\"a\":{\"favourite\":true}}");
                Check(File.ReadAllText(Path.Combine(roots.Roaming, @"Recall\library.json")) == "{\"a\":{\"favourite\":true}}", "library back");
                Check(File.Exists(Path.Combine(install, @"cache\storage.json")), "storage back");
            }
            finally { if (Directory.Exists(root)) Directory.Delete(root, true); }
        }

        private static void PartRecoveryNames()
        {
            var root = Path.Combine(Path.GetTempPath(), "recall-setup-part-test-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(root);
            try
            {
                var rec = Path.Combine(root, "G__rec.mp4");
                var folders = new List<string> { root };
                Check(PartRecovery.Recover(rec, folders, s => { }) == null, "no .part: nothing to do");
                File.WriteAllText(rec + ".part", "x");
                File.WriteAllText(rec, "taken");
                Check(PartRecovery.Recover(rec, new List<string> { Path.Combine(root, "other") }, s => { }) == null && File.Exists(rec + ".part"), "outside the recordings folders: refused");
                Check(PartRecovery.Recover(rec, folders, s => { }) == Path.Combine(root, "G__rec-recovered.mp4"), "a taken name gets -recovered");
                var clip = Path.Combine(root, "G__clip.mp4");
                File.WriteAllText(clip + ".part", "x");
                Check(PartRecovery.Recover(clip, folders, s => { }) == null && File.Exists(clip + ".part"), "only a long recording's name");
                Check(PartRecovery.IsLongRecording("G__2026-11-10__12-00-00__rec-2.mp4") && !PartRecovery.IsLongRecording("notes.txt"), "the engine's names");
                // A junction inside the folder is never followed.
                var target = Path.Combine(root, "elsewhere");
                Directory.CreateDirectory(target);
                var link = Path.Combine(root, "link");
                var mk = System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("cmd.exe", "/c mklink /J \"" + link + "\" \"" + target + "\"") { CreateNoWindow = true, UseShellExecute = false });
                mk.WaitForExit();
                var linked = Path.Combine(link, "G__rec.mp4");
                File.WriteAllText(linked + ".part", "x");
                Check(PartRecovery.Refusal(linked, folders) != null && File.Exists(linked + ".part"), "a junction on the way: refused");
                Directory.Delete(link);
            }
            finally { Directory.Delete(root, true); }
        }
    }
}
