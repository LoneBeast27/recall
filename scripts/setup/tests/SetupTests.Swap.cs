using System;
using System.Collections.Generic;
using System.IO;

namespace Recall.Setup.Tests
{
    /// The swap's journal: whatever fails, the folder ends all old (or all new), never mixed.
    internal static partial class TestRunner
    {
        private sealed class SwapBox : IDisposable
        {
            public readonly string Root, Inst, Stage;

            public SwapBox()
            {
                Root = Path.Combine(Path.GetTempPath(), "recall-setup-journal-test-" + Guid.NewGuid().ToString("N"));
                Inst = Path.Combine(Root, "inst");
                Stage = Path.Combine(Root, "stage");
                Directory.CreateDirectory(Inst);
                Directory.CreateDirectory(Stage);
                foreach (var n in new[] { "a-native.exe", "b-av.dll", "c-recall.exe", "e-hdr.exe" })
                {
                    File.WriteAllText(Path.Combine(Inst, n), "old " + n);
                    File.WriteAllText(Path.Combine(Stage, n), "new " + n);
                }
                File.WriteAllText(Path.Combine(Stage, "d-added.dll"), "new d-added.dll");
            }

            public string State()
            {
                var parts = new List<string>();
                var files = Directory.GetFiles(Inst);
                Array.Sort(files, StringComparer.OrdinalIgnoreCase);
                foreach (var f in files) parts.Add(Path.GetFileName(f) + "=" + File.ReadAllText(f));
                return string.Join("; ", parts.ToArray());
            }

            public const string AllOld = "a-native.exe=old a-native.exe; b-av.dll=old b-av.dll; c-recall.exe=old c-recall.exe; e-hdr.exe=old e-hdr.exe";

            public string Run()
            {
                try { InstallSwap.Run(Stage, Inst); return null; }
                catch (Exception e) { return e.Message; }
            }

            public void Dispose()
            {
                InstallSwap.BeforeMove = null;
                if (Directory.Exists(Root)) Directory.Delete(Root, true);
            }
        }

        private static void SwapThirdFileLocked()
        {
            using (var box = new SwapBox())
            {
                string error;
                using (new FileStream(Path.Combine(box.Inst, "c-recall.exe"), FileMode.Open, FileAccess.Read, FileShare.None)) error = box.Run();
                Check(error != null && error.Contains("rolled back"), "rolled back: " + error);
                Check(box.State() == SwapBox.AllOld, "all old: " + box.State());
                Check(!File.Exists(SwapJournal.PathIn(box.Inst)), "a complete rollback removes the journal");
            }
        }

        private static void SwapNonIoThrow()
        {
            using (var box = new SwapBox())
            {
                int moves = 0;
                InstallSwap.BeforeMove = (from, to) => { if (++moves == 3) throw new InvalidCastException("not an IO error"); };
                var error = box.Run();
                Check(error != null && error.Contains("rolled back") && error.Contains("not an IO error"), "any exception rolls back: " + error);
                Check(box.State() == SwapBox.AllOld, "all old: " + box.State());
            }
        }

        private static void SwapRollbackStepFails()
        {
            using (var box = new SwapBox())
            {
                // c's move fails; during the rollback a's backup in .old is held open, so that step fails too.
                FileStream held = null;
                InstallSwap.BeforeMove = (from, to) =>
                {
                    if (from.EndsWith(@"inst\c-recall.exe", StringComparison.OrdinalIgnoreCase) && to.Contains(@"\.old\")) throw new IOException("c is locked");
                    if (held == null && from.EndsWith(@".old\a-native.exe", StringComparison.OrdinalIgnoreCase))
                        held = new FileStream(from, FileMode.Open, FileAccess.Read, FileShare.None);
                };
                string error;
                try { error = box.Run(); }
                finally { InstallSwap.BeforeMove = null; }
                Check(error != null && error.Contains("could not be fully rolled back"), "says so: " + error);
                Check(File.ReadAllText(Path.Combine(box.Inst, "b-av.dll")) == "old b-av.dll", "the rollback went on past the failing step");
                Check(File.Exists(SwapJournal.PathIn(box.Inst)), "the journal is kept");
                held.Dispose();
                Check(InstallSwap.Recover(box.Inst) == null, "Recover finishes it");
                Check(box.State() == SwapBox.AllOld, "all old: " + box.State());
                Check(!File.Exists(SwapJournal.PathIn(box.Inst)), "journal gone");
                Check(box.Run() == null && box.State().Contains("a-native.exe=new"), "the next swap works");
            }
        }

        private static void SwapLeftoverJournal()
        {
            using (var box = new SwapBox())
            {
                // A swap killed after b moved to .old and its new copy went in, and after c moved out:
                // c is missing, b is new, d (added) is in, and a stale old e from an earlier update waits in .old.
                var old = Path.Combine(box.Inst, ".old");
                Directory.CreateDirectory(old);
                File.Move(Path.Combine(box.Inst, "b-av.dll"), Path.Combine(old, "b-av.dll"));
                File.WriteAllText(Path.Combine(box.Inst, "b-av.dll"), "new b-av.dll");
                File.Move(Path.Combine(box.Inst, "c-recall.exe"), Path.Combine(old, "c-recall.exe"));
                File.WriteAllText(Path.Combine(box.Inst, "d-added.dll"), "new d-added.dll");
                File.WriteAllText(Path.Combine(old, "e-hdr.exe"), "STALE e-hdr.exe");
                File.WriteAllText(SwapJournal.PathIn(box.Inst), SwapJournal.Header + "\nm b-av.dll\nn1 b-av.dll\nn0 d-added.dll\nm c-recall.exe\nn1 c-re");
                for (int round = 0; round < 2; round++)
                {
                    Check(InstallSwap.Recover(box.Inst) == null, "recovered (round " + round + ")");
                    Check(box.State() == SwapBox.AllOld, "all old, the stale .old file untouched: " + box.State());
                }
                File.WriteAllText(SwapJournal.PathIn(box.Inst), "not a journal");
                Check(InstallSwap.Recover(box.Inst) != null && box.Run() != null, "an unreadable journal blocks the next swap");
            }
        }

        private static void SwapNeverCarriesUserFiles()
        {
            using (var box = new SwapBox())
            {
                File.WriteAllText(Path.Combine(box.Stage, "config.toml"), "x");
                var error = box.Run();
                Check(error != null && error.Contains("user settings file"), "refused: " + error);
                Check(box.State() == SwapBox.AllOld, "nothing replaced");
            }
        }
    }
}
