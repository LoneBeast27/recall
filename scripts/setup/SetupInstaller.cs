using System;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
using System.Threading;

namespace Recall.Setup
{
    /// Setup's log: one file per run target (%TEMP%\RecallSetup.log, or &lt;sandbox&gt;\setup.log).
    internal static class SetupLog
    {
        private static readonly object Gate = new object();

        public static string PathFor(SetupTargets targets)
        {
            return targets != null && targets.IsSandbox ? Path.Combine(targets.SandboxRoot, "setup.log")
                : Path.Combine(Path.GetTempPath(), "RecallSetup.log");
        }

        public static Action<string> For(SetupTargets targets)
        {
            var path = PathFor(targets);
            return line =>
            {
                lock (Gate)
                {
                    try { File.AppendAllText(path, DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") + " [" + System.Diagnostics.Process.GetCurrentProcess().Id + "] " + line + Environment.NewLine); }
                    catch (IOException) { }
                    catch (UnauthorizedAccessException) { }
                }
            };
        }
    }

    /// The install itself, in Setup's own process (the dev installer's steps for a payload: no build).
    /// Order: guards → SSD check → app closed → recorder quit and waited for → stage → verified
    /// all-or-nothing swap → Start menu shortcut → Apps &amp; Features entry → optional detached
    /// recorder start. The cache folder and the user's settings are never touched.
    internal static class Installer
    {
        /// Setup caps its wait for a recorder finishing a clip before anything is replaced.
        internal const int QuitWaitMinutes = 10;
        public const string UninstallerResource = "Recall.Uninstaller.exe";
        private const string FfmpegBin = @"tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared\bin";

        /// The FFmpeg files older installs also copied into bin\ (removed from there by name).
        internal static readonly string[] OldBinFiles = {
            "ffmpeg.exe", "ffprobe.exe", "ffplay.exe", "avcodec-61.dll", "avdevice-61.dll", "avfilter-10.dll", "avformat-61.dll",
            "avutil-59.dll", "postproc-58.dll", "swresample-5.dll", "swscale-8.dll"
        };

        public static string Run(string package, string work, InstallOptions options, IProgress<string> progress)
        {
            var targets = SetupTargets.FromEnvironment();
            var log = SetupLog.For(targets);
            var dir = options.Path;
            log("Setup " + SetupBuildInfo.Version + (options.Update ? " update " : " install ") + dir
                + (targets.IsSandbox ? " (sandbox " + targets.SandboxRoot + ")" : ""));
            targets.CheckInstallDir(dir);
            RecorderQuitter quitter;
            UpdateFlow update;
            try
            {
                var mismatch = ExpectedBuild.Mismatch(options.ExpectRevision, SetupBuildInfo.Revision);
                if (mismatch != null) throw new InvalidOperationException(mismatch);
                InstallDrive.Check(dir, options.AllowUnknownDrive, log);
                if (options.Update && SetupPackage.ExistingInstallPath(dir) == null)
                    throw new InvalidOperationException("The selected Recall installation is incomplete or unavailable. No files were changed.");
                // An update may close the app (it asked for the update); a first install refuses while it runs.
                quitter = new RecorderQuitter(targets, dir, options.Update, log);
                update = options.Update ? new UpdateFlow(targets, dir, options.Resume, log) : null;
            }
            catch (Exception error)
            {
                // Nothing was touched yet, but the app quit itself for this update: reopen it and say so.
                if (options.Update) UpdateFlow.FailedBeforeStart(targets, dir, options, log, error);
                throw;
            }
            Report(progress, "Waiting for Recall to finish saving…");
            try
            {
                // Inside: the app has already quit itself for the update, so any failure here must
                // still reopen it and say so (Failed copes with no backup yet).
                if (update != null) update.Begin(quitter);
                Replace(package, work, dir, quitter, update, progress, log);
            }
            catch (Exception error)
            {
                if (update != null) update.Failed(error, quitter, options);
                throw;
            }

            var notes = new List<string>();
            try { Register(targets, dir, options, log); }
            catch (Exception error)
            {
                // The new files are in: the update stands, Recall still reopens, and the note says what failed.
                if (update == null) throw;
                notes.Add("Recall is updated, but Setup could not finish registering it (" + error.Message + "). Run Setup again to repair the Start menu and Apps & Features entries.");
                log("WARNING: " + notes[notes.Count - 1]);
            }
            if (update != null) update.Succeeded(quitter, options, notes);
            else if (options.Launch) StartRecorder(targets, dir, options, quitter, log, notes);
            else if (options.Autostart) notes.Add("Start with Windows was not set (the recorder was not started). Turn it on in Recall > Settings > General.");
            foreach (var note in notes) log("WARNING: " + note);
            log("Installed to " + dir + ".");
            return notes.Count == 0 ? null : string.Join("\n", notes.ToArray());
        }

        /// Tests only: runs after the swap, before the Start menu and Apps &amp; Features entries.
        internal static Action AfterSwap = null;

        private static void Register(SetupTargets targets, string dir, InstallOptions options, Action<string> log)
        {
            var hook = AfterSwap;
            if (hook != null) hook();
            if (options.Shortcut && !options.Update)
            {
                Shortcut.Write(targets.Shortcut, Path.Combine(dir, "recall-native.exe"), dir, Path.Combine(dir, "recall.ico"),
                    "Recall: game clips, library and editor");
                log("Start menu shortcut: " + targets.Shortcut);
            }
            AppsEntry.Write(targets, dir, AppsEntry.ProgramSizeKb(dir));
            log("Apps & Features entry: HKCU\\" + targets.UninstallKey);
        }

        /// App closed, recorder quit and waited for, then the staged, verified, all-or-nothing swap.
        private static void Replace(string package, string work, string dir, RecorderQuitter quitter, UpdateFlow update,
            IProgress<string> progress, Action<string> log)
        {
            quitter.EnsureAppClosed();
            quitter.Stop(QuitWaitMinutes);
            if (update != null) update.AfterQuit();

            Report(progress, "Installing program files…");
            var stage = Path.Combine(work, "stage");
            Stage(package, stage);
            Directory.CreateDirectory(dir);
            var installed = InstallManifest.Describe(stage);
            InstallSwap.Run(stage, dir);
            InstallManifest.Write(dir, InstallManifest.Merge(installed, InstallManifest.Read(dir), e => InstallManifest.Unchanged(dir, e)));
            RemoveOldBin(dir);
            log("Program files swapped in: " + dir);
        }

        private static void Report(IProgress<string> progress, string text)
        {
            if (progress != null) progress.Report(text);
        }

        /// The payload's files, flat, plus Setup's uninstaller.
        internal static void Stage(string package, string stage)
        {
            Directory.CreateDirectory(stage);
            Action<string> add = relative =>
            {
                var from = Path.Combine(package, relative);
                if (!File.Exists(from)) throw new InvalidOperationException("The setup package is missing " + relative + ".");
                var name = Path.GetFileName(from);
                File.Copy(from, Path.Combine(stage, name), true);
            };
            add(@"target-gpui\release\recall-native.exe");
            foreach (var exe in new[] { "recall-hdr.exe", "recall.exe", "recall-overlay.exe" }) add(@"target-dev\release\" + exe);
            var bin = Path.Combine(package, FfmpegBin);
            if (Directory.Exists(bin))
                foreach (var file in Directory.GetFiles(bin, "*.dll")) add(Path.Combine(FfmpegBin, Path.GetFileName(file)));
            add(Path.Combine(FfmpegBin, "ffmpeg.exe"));
            add(Path.Combine(FfmpegBin, "ffprobe.exe"));
            add(@"recall-native\assets\recall.ico");
            foreach (var notice in new[] { "LICENSE", "NOTICE.md", @"recall-native\assets\fonts\FONTS-LICENSE.txt",
                @"recall-native\assets\icons\LICENSE-lucide.txt", @"recall-native\assets\icons\brand-attribution.txt", "FFMPEG-LICENSE.txt" })
                add(notice);
            WriteUninstaller(Path.Combine(stage, AppsEntry.UninstallerName));
        }

        /// The small uninstaller build embedded by the release build; a build without one copies Setup itself.
        private static void WriteUninstaller(string destination)
        {
            var assembly = Assembly.GetExecutingAssembly();
            using (var embedded = assembly.GetManifestResourceStream(UninstallerResource))
            {
                if (embedded == null) { File.Copy(assembly.Location, destination, true); return; }
                using (var file = File.Create(destination)) embedded.CopyTo(file);
            }
        }

        /// Older installs kept a second copy of every FFmpeg file in bin\: only those names go, then the folder if empty.
        private static void RemoveOldBin(string dir)
        {
            var bin = Path.Combine(dir, "bin");
            if (!Directory.Exists(bin)) return;
            foreach (var file in Directory.GetFiles(bin))
            {
                var name = Path.GetFileName(file);
                if (Array.Exists(OldBinFiles, k => k.Equals(name, StringComparison.OrdinalIgnoreCase))) File.Delete(file);
            }
            if (Directory.GetFileSystemEntries(bin).Length == 0) Directory.Delete(bin);
        }

        /// Starts the installed recorder detached (explorer as its parent), then waits up to 10 s
        /// for it to answer on its pipe. Exit 86 (another recorder holds the engine slot) is a note,
        /// not a failure. Start with Windows is sent only when opted in, never on an update.
        internal static bool StartRecorder(SetupTargets targets, string dir, InstallOptions options, RecorderQuitter quitter,
            Action<string> log, List<string> notes)
        {
            bool answered = false, exited = false;
            int code = 0;
            using (var recorder = DetachedProcess.Start(Path.Combine(dir, "recall-hdr.exe"), "--record", null))
            {
                log("Recorder started: pid " + recorder.Pid + ", parent explorer.exe (pid " + recorder.ParentPid + ")");
                quitter.Pipe.OwnerRefused = false;
                var deadline = DateTime.UtcNow.AddSeconds(10);
                while (!answered && !quitter.Pipe.OwnerRefused && DateTime.UtcNow < deadline)
                {
                    Thread.Sleep(500);
                    if (recorder.HasExited(out code)) { exited = true; break; }
                    answered = quitter.Pipe.Send("{\"cmd\":\"status\"}");
                }
            }
            var outcome = QuitPolicy.StartOutcome(exited, code, answered);
            if (outcome != null) notes.Add(outcome);
            if (!options.Update)
            {
                var stray = quitter.OtherControllers();
                if (stray.Count > 0)
                    notes.Add("An older recorder is still running (" + SessionProcesses.Describe(stray) + "). Quit Recall from the tray, then start it again.");
            }
            if (QuitPolicy.SendAutostart(options.Autostart, options.Update, answered))
            {
                quitter.Pipe.Send("{\"cmd\":\"set_config\",\"patch\":{\"startup\":{\"autostart\":true}}}");
                Thread.Sleep(500);
                var run = RunValue.Read(targets);
                if (run != null && RunValue.PointsInto(run, PathId.Root(dir))) log("Recorder running; it starts with Windows (" + run + ").");
                else notes.Add("Start with Windows was not set; turn it on in Recall > Settings > General.");
            }
            else if (options.Autostart && !options.Update) notes.Add("Start with Windows was not set: the recorder did not answer. Turn it on in Recall > Settings > General.");
            else if (answered) log("Recorder running. Start with Windows is unchanged.");
            return answered;
        }
    }
}
