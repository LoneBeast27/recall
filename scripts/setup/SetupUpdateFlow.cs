using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;

namespace Recall.Setup
{
    /// An update (`--update`), coupled to the running Recall: the settings are backed up first, the
    /// recorder is asked to save what it records and quit (`"save_first":true`, recall-hdr
    /// record_quit_save.rs), the files are swapped, then the recorder comes back if it ran (with its
    /// long recording again if one was on) and, when the app asked for the update (`--resume`), the
    /// app reopens where it was. A failure after the quit restores the settings and brings back the
    /// OLD pair the same way; the reopened app says the update failed (no retry loop).
    internal sealed class UpdateFlow
    {
        /// How long the restarted recorder may take to capture before `record_start` is sent once.
        internal static int CaptureWaitSeconds = 30;

        private readonly SetupTargets targets;
        private readonly string dir;
        private readonly Action<string> log;
        private readonly UserRoots roots;
        private readonly ResumeFile resume;
        private bool recorderRan;
        private bool longOn;
        private string longPath;
        private string backup;
        private string recovered;

        public UpdateFlow(SetupTargets targets, string dir, string resumePath, Action<string> log)
        {
            this.targets = targets;
            this.dir = dir;
            this.log = log;
            roots = UserRoots.For(targets);
            resume = ResumeFile.Open(resumePath, roots);
            if (resumePath != null && resume == null) log("WARNING: --resume " + resumePath + " is not a resume file in " + roots.Updates + "; ignored.");
        }

        public string BackupDir { get { return backup; } }

        /// Before anything is closed: what runs, and a copy of every settings file.
        public void Begin(RecorderQuitter quitter)
        {
            var why = InstallSwap.Recover(dir);
            if (why != null) throw new InvalidOperationException("An earlier update could not be undone yet (" + why + "). Close Recall, then run Setup again.");
            recorderRan = quitter.OurControllers().Count > 0;
            var status = recorderRan ? quitter.ReadOurStatus() : null;
            longOn = (status != null && status.LongActive) || (resume != null && resume.LongOn);
            longPath = status != null ? status.LongPath : null;
            log("Update: recorder " + (recorderRan ? "running" : "not running") + (longOn ? ", long recording on" + (longPath == null ? "" : " (" + longPath + ")") : ""));
            backup = ConfigBackup.Create(roots, dir, resume != null && resume.FromVersion != null ? resume.FromVersion : "old", DateTime.Now, log);
            quitter.SaveFirst = true;
        }

        /// The recorder has exited: a long recording its quit gave up on is recovered now.
        public void AfterQuit()
        {
            if (!longOn || longPath == null) return;
            try { recovered = PartRecovery.Recover(longPath, log); }
            catch (Exception error) { log("WARNING: the cut-short recording could not be recovered: " + error.Message); }
        }

        /// The new files are in: bring the recorder (and the app) back.
        public void Succeeded(RecorderQuitter quitter, InstallOptions options, List<string> notes)
        {
            Relaunch(quitter, options, notes, "updated", null);
        }

        /// The update failed after the quit: the settings come back, then the old pair.
        public void Failed(Exception error, RecorderQuitter quitter, InstallOptions options)
        {
            log("Update failed: " + error.GetBaseException().Message);
            // Never over a recorder still running (its own saves are newer than the backup).
            try { if (backup != null && quitter.OurControllers().Count == 0) ConfigBackup.Restore(backup, roots, dir, log); }
            catch (Exception restore) { log("WARNING: settings restore failed: " + restore.Message); }
            // A rollback that could not finish: try again now, and never start a mismatched pair.
            var why = InstallSwap.Recover(dir);
            for (int i = 0; why != null && i < 5; i++) { Thread.Sleep(1000); why = InstallSwap.Recover(dir); }
            if (why != null)
            {
                log("WARNING: the install folder is not consistent yet (" + why + "); Recall is not reopened.");
                Outcome("failed", error.GetBaseException().Message);
                return;
            }
            try
            {
                Relaunch(quitter, options, new List<string>(), "failed", error.GetBaseException().Message);
            }
            catch (Exception relaunch) { log("WARNING: the previous Recall could not be reopened: " + relaunch.Message); }
        }

        /// The recorder first (when it ran and is gone), then the outcome, then the app, which reads it.
        private void Relaunch(RecorderQuitter quitter, InstallOptions options, List<string> notes, string outcome, string error)
        {
            if (recorderRan && quitter.OurControllers().Count == 0 && SetupPackage.ExistingInstallPath(dir) != null)
            {
                var answered = Installer.StartRecorder(targets, dir, options, quitter, log, notes);
                if (answered && longOn) StartLongRecording(quitter);
            }
            Outcome(outcome, error);
            if (resume == null || SetupPackage.ExistingInstallPath(dir) == null) return;
            using (var app = DetachedProcess.Start(Path.Combine(dir, "recall-native.exe"), "--resume \"" + resume.Path + "\"", null))
                log("Recall reopened: pid " + app.Pid + " --resume " + resume.Path);
        }

        /// The long recording the update stopped starts again once the recorder captures (a
        /// `record_start` sent before would only be refused), at most once.
        private void StartLongRecording(RecorderQuitter quitter)
        {
            var target = quitter.Pipe.SentTo;
            var until = DateTime.UtcNow.AddSeconds(CaptureWaitSeconds);
            while (target != null && DateTime.UtcNow < until)
            {
                var status = quitter.Pipe.ReadStatus(target);
                if (status != null && status.Capturing) break;
                Thread.Sleep(500);
            }
            log(quitter.Pipe.Send("{\"cmd\":\"record_start\"}") ? "Long recording started again." : "WARNING: the long recording could not be started again.");
        }

        private void Outcome(string outcome, string error)
        {
            if (resume == null) return;
            resume.Set("outcome", outcome);
            resume.Set("error", error);
            resume.Set("still_on", outcome == "failed" ? resume.FromVersion : null);
            resume.Set("recovered", recovered);
            resume.Set("config_backup", backup);
            resume.Set("setup_version", SetupBuildInfo.Version);
            resume.Save();
        }
    }
}
