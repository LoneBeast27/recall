using System;
using System.IO;
using System.Linq;

namespace Recall.Setup.Tests
{
    /// Failures before the update starts, and a Recover that throws: Recall is never left closed unannounced.
    internal static partial class TestRunner
    {
        /// A resume file Setup cannot read (held open): the update still runs and Recall reopens.
        private static void UnreadableResumeFileNeverStopsTheUpdate()
        {
            using (var box = new Sandbox())
            {
                var resume = box.WriteResume(false);
                var roots = new UserRoots(box.Roaming, box.Local);
                string error;
                using (new FileStream(resume, FileMode.Open, FileAccess.Read, FileShare.None))
                {
                    Check(ResumeFile.Open(resume, roots) != null, "Open does not throw on a held file");
                    error = box.Run(resume);
                }
                Check(error == null, "the update ran: " + error);
                var lines = box.LinesAfterApp();
                Check(lines.Any(l => l.StartsWith("recall-native.exe ") && l.Contains("start --resume")), "Recall reopened: " + string.Join(" | ", lines));
                Check(Sandbox.Programs.All(p => box.Version(p) == "NEW"), "updated");
            }
        }

        /// A guard fails before anything is closed (an incomplete install): Recall reopens and is told.
        private static void GuardFailureReopensRecall()
        {
            using (var box = new Sandbox())
            {
                var resume = box.WriteResume(false);
                File.Delete(Path.Combine(box.Install, "recall.exe"));
                var error = box.Run(resume);
                Check(error != null && error.Contains("incomplete"), "the guard's message: " + error);
                var lines = box.LinesAfterApp();
                Check(lines.Any(l => l.StartsWith("recall-native.exe ") && l.Contains("start --resume")), "Recall reopened: " + string.Join(" | ", lines));
                var outcome = box.Resume(resume);
                Check("failed".Equals(outcome["outcome"]) && "0.1.0".Equals(outcome["still_on"]), "told it is still on 0.1.0");
            }
        }

        /// Recover throws inside Failed: Recall stays closed and Setup says so (the NotReopened path).
        private static void ThrowingRecoverInFailedSaysSo()
        {
            using (var box = new Sandbox())
            {
                var resume = box.WriteResume(false);
                FileStream held = null;
                InstallSwap.BeforeMove = (from, to) =>
                {
                    if (from.EndsWith(@"Recall\recall.ico", StringComparison.OrdinalIgnoreCase) && to.Contains(@"\.old\")) throw new IOException("recall.ico is locked");
                    if (held == null && from.EndsWith(@".old\recall-hdr.exe", StringComparison.OrdinalIgnoreCase))
                        held = new FileStream(from, FileMode.Open, FileAccess.Read, FileShare.None);
                };
                InstallSwap.BeforeRecover = () => { throw new UnauthorizedAccessException("the journal is denied"); };
                Exception error = null;
                try
                {
                    var options = new InstallOptions(box.Install, false, false, false, true, false, true);
                    options.Resume = resume;
                    Installer.Run(box.Package, box.Work, options, null);
                }
                catch (Exception e) { error = e; }
                finally
                {
                    InstallSwap.BeforeMove = null;
                    InstallSwap.BeforeRecover = null;
                    if (held != null) held.Dispose();
                }
                Check(error is UpdateNotReopened && SetupWindow.ResultText(error) == UpdateFlow.NotReopenedText,
                    "Setup says so: " + (error == null ? "no error" : error.GetType().Name + " " + error.Message));
                Check(box.SetupLog().Contains(UpdateFlow.NotReopenedText), "and logs it");
                Check(!box.LinesAfterApp().Any(l => l.StartsWith("recall-native.exe ")), "Recall stayed closed");
                Check(InstallSwap.Recover(box.Install) == null && Sandbox.Programs.All(p => box.Version(p) == "OLD"), "the next run finishes it");
            }
        }
    }
}
