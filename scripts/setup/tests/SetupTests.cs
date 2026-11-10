using System;
using System.Collections.Generic;
using System.IO;

namespace Recall.Setup.Tests
{
    /// Behaviour pins for Setup's C# install, update and uninstall (the PowerShell installer's
    /// install_script_*_tests.rs pin the dev scripts). Run by `build.ps1 -Test`; exit 0 = all pass.
    internal static partial class TestRunner
    {
        private static int failed;
        private static int passed;
        private static string sourceDir;

        private static int Main(string[] args)
        {
            sourceDir = args.Length > 0 ? args[0] : null;
            fakeExe = args.Length > 1 ? args[1] : null;
            Run("only its own recorder gets a line", OnlyOwnRecorder);
            Run("the quit wait is bounded and never forces a save", BoundedWait);
            Run("a recorder finishing a clip gets no second quit; a held sign-out gets one plain quit", NoSecondQuit);
            Run("exit 86 is a note, not a failure", SlotBusyNotFatal);
            Run("an update never turns Start with Windows on", UpdateNeverAutostarts);
            Run("status lines parse", StatusParse);
            Run("a sandbox stays in its folder", SandboxGuard);
            Run("uninstall deletes only unchanged manifest files inside the install folder", UninstallOnlyManifest);
            Run("uninstall never deletes outside the install folder or in a clip folder", UninstallNeverOutside);
            Run("cache: only provably regenerable files are deleted", CacheClassification);
            Run("kept clip copies are counted in the closing note", KeptNoteCounts);
            Run("the uninstaller's temporary copy cleans only its own folder", TempCopyFolder);
            Run("the next Setup run sweeps only leftover uninstaller copies", TempCopySweep);
            Run("the manifest keeps earlier files still unchanged", ManifestMerge);
            Run("Run values and clip folder settings are read", RunValueAndFolders);
            Run("drive classification", DriveKinds);
            Run("the swap is all new or all old", SwapRollback);
            Run("shipped Setup sources never start PowerShell", NoPowerShell);
            Run("swap: a locked 3rd file rolls back to all old", SwapThirdFileLocked);
            Run("swap: a non-IO throw after 2 moves rolls back", SwapNonIoThrow);
            Run("swap: a failing rollback step is passed over, the journal kept, Recover finishes", SwapRollbackStepFails);
            Run("swap: Recover on a leftover journal is all old, every time", SwapLeftoverJournal);
            Run("swap: a stage never carries a user settings file", SwapNeverCarriesUserFiles);
            Run("settings backups: the 3 newest stay and restore", BackupsKeepThree);
            Run("a cut-short recording is recovered under a free name", PartRecoveryNames);
            Run("the resume file must be in the updates folder", ResumeFileGuard);
            Run("sandbox update while recording: one save_first quit, recorder + record_start + app --resume", UpdateWhileRecording);
            Run("sandbox update that fails: settings restored, old pair reopened, recording recovered", UpdateRollsBack);
            Run("an update that cannot be undone yet leaves Recall closed and says so", UpdateNotReopenedSaysSo);
            Run("a lock file a power cut left is not a lock", StaleLockFileIsNotALock);
            Run("a failing settings backup still reopens Recall and tells it", BackupFailureStillReopens);
            Run("a failure after the swap keeps the update and reopens Recall", FailureAfterSwapStillReopens);
            Run("a resume file Setup cannot read never stops the update", UnreadableResumeFileNeverStopsTheUpdate);
            Run("a guard failing before the update reopens Recall and tells it", GuardFailureReopensRecall);
            Run("a Recover that throws inside Failed leaves Recall closed and says so", ThrowingRecoverInFailedSaysSo);
            Run("an update without --resume reopens the app that was open, plainly", PlainUpdateReopensAnOpenApp);
            Run("an update without --resume leaves a closed app closed", PlainUpdateLeavesAClosedAppClosed);
            Run("an update without --resume never reopens after a failed rollback", PlainUpdateNotReopenedAfterAFailedRollback);
            Run("Setup opened by hand: same, newer, older and offline versions", LaunchDecisions);
            Run("the latest release must have an uploaded Setup and a tag on main", LatestReleaseParsing);
            Run("Setup opened over an existing install runs as an update", OverExistingIsUpdate);
            Run("Setup opened over an existing install keeps every settings file byte for byte", OverExistingKeepsSettings);
            Run("Setup opened by hand restores a settings file left unreadable after the swap", HandUpdateRestoresUnreadable);
            Console.WriteLine(passed + " passed, " + failed + " failed");
            return failed == 0 ? 0 : 1;
        }

        private static void Run(string name, Action test)
        {
            try { test(); passed++; Console.WriteLine("PASS " + name); }
            catch (Exception error) { failed++; Console.WriteLine("FAIL " + name + ": " + error.Message); }
        }

        private static void Check(bool condition, string what)
        {
            if (!condition) throw new Exception(what);
        }

        private const string Me = "S-1-5-21-1-2-3-1001";

        private static void OnlyOwnRecorder()
        {
            Check(QuitPolicy.MayWrite(4242, Me, Me, true) == null, "own controller, own account: written");
            Check(QuitPolicy.MayWrite(4242, "S-1-5-21-1-2-3-1002", Me, true) != null, "another account: refused");
            Check(QuitPolicy.MayWrite(4242, null, Me, true) != null, "unreadable account: refused");
            Check(QuitPolicy.MayWrite(4242, Me, Me, false) != null, "a recorder from another folder (the live one): refused");
            Check(QuitPolicy.MayWrite(0, Me, Me, true) != null, "unknown server: refused");
            Check(QuitPolicy.IsOurController(@"Y:\Sandbox\Programs\Recall\recall-hdr.exe", @"y:\sandbox\programs\recall\RECALL-HDR.EXE"), "case-insensitive");
            Check(!QuitPolicy.IsOurController(@"C:\Users\x\AppData\Local\Programs\Recall\recall-hdr.exe", @"Y:\Sandbox\Programs\Recall\recall-hdr.exe"), "other folder");
            Check(!QuitPolicy.IsOurController(null, @"Y:\x\recall-hdr.exe"), "unreadable path is never ours");
        }

        private static void BoundedWait()
        {
            var t0 = new DateTime(2026, 11, 10, 12, 0, 0);
            var w = new QuitWait(t0, 10);
            // Saving: waits on, past the 2 min grace, until the cap.
            Check(w.Step(t0.AddMinutes(1), true, true) == QuitStep.Wait, "saving at 1 min");
            Check(w.Step(t0.AddMinutes(9), true, true) == QuitStep.Wait, "saving at 9 min");
            Check(w.Step(t0.AddMinutes(10), true, true) == QuitStep.Timeout && w.Why == "cap", "the cap ends it");
            Check(w.TimeoutMessage(77).Contains("pid 77") && w.TimeoutMessage(77).Contains("no second quit"), "names the pid; no forced quit");
            var g = new QuitWait(t0, 10);
            Check(g.Step(t0, true, false) == QuitStep.Wait, "not saving: grace starts");
            Check(g.Step(t0.AddSeconds(119), true, null) == QuitStep.Wait, "within the grace");
            Check(g.Step(t0.AddMinutes(2), true, null) == QuitStep.Timeout && g.Why == "grace" && g.Silent, "grace ends it, silent");
            var e = new QuitWait(t0, 0);
            Check(e.Step(t0.AddHours(5), false, true) == QuitStep.Exited, "an exit always ends the wait");
            var r = new QuitWait(t0, 0);
            r.Step(t0, true, false);
            Check(r.Step(t0.AddMinutes(1), true, true) == QuitStep.Wait, "saving again clears the grace");
            Check(r.Step(t0.AddMinutes(2.5), true, false) == QuitStep.Wait, "grace restarts from there");
            Check(Installer.QuitWaitMinutes > 0, "Setup always caps the wait");
        }

        private static void NoSecondQuit()
        {
            Check(QuitPolicy.OnStatus(new RecorderStatus { FinishingClip = true }) == QuitAction.JoinSave, "finishing a clip: join, no second quit");
            Check(QuitPolicy.OnStatus(new RecorderStatus { FinishingClip = true, QuitForSignOut = true }) == QuitAction.SendQuit, "held sign-out: one plain quit");
            Check(QuitPolicy.OnStatus(new RecorderStatus()) == QuitAction.SendQuit, "idle: quit");
            Check(QuitPolicy.OnStatus(null) == QuitAction.SendQuit, "no answer: quit");
        }

        private static void SlotBusyNotFatal()
        {
            var busy = QuitPolicy.StartOutcome(true, 86, false);
            Check(busy != null && busy.Contains("stepped aside"), "86 explains itself: " + busy);
            Check(QuitPolicy.StartOutcome(false, 0, true) == null, "running and answering: no note");
            Check(QuitPolicy.StartOutcome(true, 1, false).Contains("code 1"), "other exits name the code");
            Check(QuitPolicy.SlotBusyExit == 86, "the engine's EXIT_SLOT_BUSY");
        }

        private static void UpdateNeverAutostarts()
        {
            Check(!QuitPolicy.SendAutostart(true, true, true), "update + opt-in: never");
            Check(!QuitPolicy.SendAutostart(false, false, true), "no opt-in: never");
            Check(!QuitPolicy.SendAutostart(true, false, false), "no answer: not sent");
            Check(QuitPolicy.SendAutostart(true, false, true), "first install, opted in, answering: sent");
            Check(!new InstallOptions(@"C:\R", true, false, false, true).Autostart, "the window's options never opt in");
        }

        private static void StatusParse()
        {
            var s = RecorderStatus.Parse("{\"mod\":\"session\",\"event\":{\"type\":\"status\",\"finishing_clip\":true,\"quit_for_sign_out\":false,\"finishing_progress\":{\"bytes\":5242880,\"saves\":2}}}");
            Check(s != null && s.FinishingClip && !s.QuitForSignOut && s.Bytes == 5242880 && s.Saves == 2, "full status");
            Check(RecorderStatus.Parse("{\"mod\":\"session\",\"event\":{\"type\":\"hello\"}}") == null, "other events ignored");
            Check(RecorderStatus.Parse("not json") == null, "garbage ignored");
            Check(QuitWait.Progress(1, DateTime.Now, DateTime.Now, s).Contains("5 MB written"), "progress line");
        }

        private static void SandboxGuard()
        {
            const string sandbox = @"Y:\T\sandbox\", real = @"C:\Users\u\AppData\Local\Programs\Recall\";
            Check(SandboxPolicy.Refusal(@"Y:\T\sandbox\Programs\Recall\", sandbox, real, null) == null, "inside: allowed");
            Check(SandboxPolicy.Refusal(@"Y:\T\elsewhere\", sandbox, real, null) != null, "outside the sandbox: refused");
            Check(SandboxPolicy.Refusal(real, @"C:\Users\u\AppData\Local\", real, null) != null, "the real folder: refused");
            Check(SandboxPolicy.Refusal(@"C:\Users\u\AppData\Local\Programs\", @"C:\Users\u\AppData\Local\", real, null) != null, "holding it: refused");
            Check(SandboxPolicy.Refusal(@"Y:\T\sandbox\R\", sandbox, real, real + "recall-hdr.exe") != null, "controller outside: refused");
            Check(SandboxPolicy.Refusal(real, null, real, null) == null, "a real run is not a sandbox");
        }
    }
}
