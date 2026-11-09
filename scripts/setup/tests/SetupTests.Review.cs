using System;
using System.IO;
using System.Linq;

namespace Recall.Setup.Tests
{
    /// Review findings: a lock file a power cut left, a failure before the backup, a failure after the swap.
    internal static partial class TestRunner
    {
        private static void StaleLockFileIsNotALock()
        {
            using (var box = new SwapBox())
            {
                var old = Path.Combine(box.Inst, ".old");
                Directory.CreateDirectory(old);
                File.Move(Path.Combine(box.Inst, "a-native.exe"), Path.Combine(old, "a-native.exe"));
                File.WriteAllText(Path.Combine(box.Inst, "a-native.exe"), "new a-native.exe");
                File.WriteAllText(SwapJournal.PathIn(box.Inst), SwapJournal.Header + "\nm a-native.exe\nn1 a-native.exe\n");
                // A power cut mid-swap: the lock file stays, no process holds it.
                File.WriteAllText(Path.Combine(box.Inst, ".install-swap-lock"), "");
                Check(InstallSwap.Recover(box.Inst) == null, "a leftover lock file does not block Recover");
                Check(box.State() == SwapBox.AllOld, "all old: " + box.State());
                File.WriteAllText(Path.Combine(box.Inst, ".install-swap-lock"), "");
                Check(box.Run() == null, "nor the next swap");
                using (new FileStream(Path.Combine(box.Inst, ".install-swap-lock"), FileMode.OpenOrCreate, FileAccess.Write, FileShare.None))
                    Check(box.Run() != null && box.Run().Contains("Another Recall install"), "a held lock still blocks");
            }
        }

        /// The settings backup fails (after the app quit itself for the update): Recall still reopens and is told.
        private static void BackupFailureStillReopens()
        {
            using (var box = new Sandbox())
            {
                var resume = box.WriteResume(false);
                UpdateFlow.BeforeBackup = () => { throw new IOException("the updates folder is full"); };
                var error = box.Run(resume);
                Check(error != null && error.Contains("the updates folder is full"), "the failure is shown: " + error);
                var lines = box.LinesAfterApp();
                Check(lines.Any(l => l.StartsWith("recall-native.exe ") && l.Contains("start --resume")), "Recall reopened: " + string.Join(" | ", lines));
                var outcome = box.Resume(resume);
                Check("failed".Equals(outcome["outcome"]) && "0.1.0".Equals(outcome["still_on"]), "and told it is still on 0.1.0");
                Check(Sandbox.Programs.All(p => box.Version(p) == "OLD"), "nothing was replaced");
            }
        }

        /// The swap is done but registering fails: the update stands, Recall reopens, the note stays on screen.
        private static void FailureAfterSwapStillReopens()
        {
            using (var box = new Sandbox())
            {
                var resume = box.WriteResume(false);
                Installer.AfterSwap = () => { throw new UnauthorizedAccessException("the registry said no"); };
                var options = new InstallOptions(box.Install, false, false, false, true, false, true);
                options.Resume = resume;
                string note = null;
                Exception error = null;
                try { note = Installer.Run(box.Package, box.Work, options, null); }
                catch (Exception e) { error = e; }
                Check(error == null, "the update stands: " + (error == null ? "" : error.Message));
                Check(note != null && note.Contains("the registry said no"), "the note says what failed: " + note);
                Check(Sandbox.Programs.All(p => box.Version(p) == "NEW"), "the new programs are in");
                var lines = box.LinesAfterApp();
                Check(lines.Any(l => l.StartsWith("recall-native.exe ") && l.Contains("start --resume")), "Recall reopened: " + string.Join(" | ", lines));
                Check("updated".Equals(box.Resume(resume)["outcome"]), "outcome: updated");
            }
        }
    }
}
