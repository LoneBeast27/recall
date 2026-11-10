using System;
using System.IO;
using System.Linq;

namespace Recall.Setup.Tests
{
    /// A rollback that cannot finish: Recall is not reopened, and Setup says so on screen and in its log.
    internal static partial class TestRunner
    {
        private static void UpdateNotReopenedSaysSo()
        {
            using (var box = new Sandbox())
            {
                var resume = box.WriteResume(false);
                FileStream held = null;
                InstallSwap.BeforeMove = (from, to) =>
                {
                    // The swap fails at recall.ico; its rollback finds recall-hdr.exe's backup held open.
                    if (from.EndsWith(@"Recall\recall.ico", StringComparison.OrdinalIgnoreCase) && to.Contains(@"\.old\")) throw new IOException("recall.ico is locked");
                    if (held == null && from.EndsWith(@".old\recall-hdr.exe", StringComparison.OrdinalIgnoreCase))
                        held = new FileStream(from, FileMode.Open, FileAccess.Read, FileShare.None);
                };
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
                    if (held != null) held.Dispose();
                }
                const string text = "Recall couldn't finish updating and wasn't reopened, to avoid a broken install. Your clips and settings are safe. Run Setup again or download it from https://github.com/LoneBeast27/recall/releases.";
                Check(UpdateFlow.NotReopenedText == text, "the agreed text: " + UpdateFlow.NotReopenedText);
                Check(error is UpdateNotReopened, "Setup stops on it: " + (error == null ? "no error" : error.GetType().Name + " " + error.Message));
                Check(SetupWindow.ResultText(error) == text, "the window shows it: " + SetupWindow.ResultText(error));
                Check(box.SetupLog().Contains(text), "the log has it");
                var lines = box.LinesAfterApp();
                Check(!lines.Any(l => l.StartsWith("recall-native.exe ")), "Recall was not reopened: " + string.Join(" | ", lines));
                Check(File.Exists(SwapJournal.PathIn(box.Install)), "the journal stays for the next run");
                Check(InstallSwap.Recover(box.Install) == null && Sandbox.Programs.All(p => box.Version(p) == "OLD"), "the next run finishes it: all old");
                Check("failed".Equals(box.Resume(resume)["outcome"]), "the resume file says failed");
            }
        }
    }
}
