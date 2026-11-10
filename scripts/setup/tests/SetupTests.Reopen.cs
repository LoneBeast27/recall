using System;
using System.IO;
using System.Linq;

namespace Recall.Setup.Tests
{
    /// An update from an older Recall's updater (`--update` only, no --resume) reopens the app it closed.
    internal static partial class TestRunner
    {
        private static string[] AppStartsAfterUpdate(Sandbox box, int before)
        {
            var until = DateTime.UtcNow.AddSeconds(15);
            while (DateTime.UtcNow < until && box.AppStarts().Length <= before) System.Threading.Thread.Sleep(100);
            return box.AppStarts().Skip(before).ToArray();
        }

        private static void PlainUpdateReopensAnOpenApp()
        {
            using (var box = new Sandbox())
            {
                box.StartApp(3); // open when the update starts; closes within the quit wait
                var error = box.Run(null);
                Check(error == null, "the update ran: " + error);
                var after = AppStartsAfterUpdate(box, 1);
                Check(after.Length == 1 && after[0].EndsWith(" start "), "reopened plainly, no arguments: " + string.Join(" | ", box.Lines()));
                Check(Sandbox.Programs.All(p => box.Version(p) == "NEW"), "updated");
            }
        }

        private static void PlainUpdateLeavesAClosedAppClosed()
        {
            using (var box = new Sandbox())
            {
                var error = box.Run(null);
                Check(error == null, "the update ran: " + error);
                Check(AppStartsAfterUpdate(box, 0).Length == 0, "the app was closed before: it stays closed");
            }
        }

        private static void PlainUpdateNotReopenedAfterAFailedRollback()
        {
            using (var box = new Sandbox())
            {
                box.StartApp(3);
                FileStream held = null;
                InstallSwap.BeforeMove = (from, to) =>
                {
                    if (from.EndsWith(@"Recall\recall.ico", StringComparison.OrdinalIgnoreCase) && to.Contains(@"\.old\")) throw new IOException("recall.ico is locked");
                    if (held == null && from.EndsWith(@".old\recall-hdr.exe", StringComparison.OrdinalIgnoreCase))
                        held = new FileStream(from, FileMode.Open, FileAccess.Read, FileShare.None);
                };
                Exception error = null;
                try { Installer.Run(box.Package, box.Work, new InstallOptions(box.Install, false, false, false, true, false, true), null); }
                catch (Exception e) { error = e; }
                finally
                {
                    InstallSwap.BeforeMove = null;
                    if (held != null) held.Dispose();
                }
                Check(error is UpdateNotReopened, "the NotReopened path wins: " + (error == null ? "no error" : error.Message));
                Check(AppStartsAfterUpdate(box, 1).Length == 0, "never reopened over a broken install");
                Check(InstallSwap.Recover(box.Install) == null, "the next run finishes it");
            }
        }
    }
}
