using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

namespace Recall.Setup
{
    /// All-or-nothing replacement of the installed files (the dev installer's swap, in C#). A copy straight
    /// over the install folder could leave old and new programs mixed when one file is locked
    /// (Recall reopened mid-install), which is a broken app.
    ///  1. A `.installing` marker for the whole swap: the app waits while it exists. It holds this
    ///     pid and this process's start time ("&lt;pid&gt; &lt;FILETIME&gt;"), so a marker left by a
    ///     killed install is not mistaken for a running one (recall-native install_wait.rs).
    ///  2. Every staged file is copied into `&lt;dir&gt;\.new` (same volume, so the swap is a rename) and verified.
    ///  3. Each installed file moves to `&lt;dir&gt;\.old`, then its new copy moves in (MoveFileEx,
    ///     replace-existing, write-through). A running program can be renamed; a file opened without
    ///     sharing cannot.
    ///  4. A `.swap-journal` (SetupSwapJournal.cs) is on disk before the first move and before each
    ///     move after it. Any failure puts every moved file back from `.old` (new files that had no old
    ///     one are removed), going on past a step that fails: the folder is all old or all new. A
    ///     rollback that could not finish keeps the journal; Setup and the app finish it at their next
    ///     start (Recover). Success empties `.old` (a file still in use stays there).
    /// A rollback that could not put every file back (its journal is kept for Recover).
    internal sealed class SwapIncomplete : InvalidOperationException
    {
        public SwapIncomplete(string message) : base(message) { }
    }

    internal static class InstallSwap
    {
        /// Tests only: called before every move (the swap's and a rollback's) with (from, to).
        internal static Action<string, string> BeforeMove = null;

        /// User settings never live in the install folder's top level, and a stage never carries them:
        /// the swap replaces only program files (the cache folder and everything else is not touched).
        internal static readonly string[] UserFiles = {
            "config.toml", "config.toml.last-good", "state.json", "storage.json", "library.json", "folders.json",
            "imports.json", "recorded.json", "open-clips.json", "editor.json", "settings.json", "hub-settings.json"
        };

        public static void Run(string stage, string dir)
        {
            Directory.CreateDirectory(dir);
            var marker = Path.Combine(dir, ".installing");
            var newDir = Path.Combine(dir, ".new");
            var oldDir = Path.Combine(dir, ".old");
            var swapLock = TakeLock(dir);
            if (swapLock == null)
                throw new InvalidOperationException("Another Recall install is swapping files in this folder. Wait for it to finish, then try again.");
            try
            {
                File.WriteAllText(marker, MarkerValue());
                var why = RecoverLocked(dir);
                if (why != null) throw new InvalidOperationException("An earlier update could not be undone yet (" + why + "). Close Recall, then run Setup again.");
                TryDeleteTree(newDir);
                if (Directory.Exists(oldDir)) foreach (var f in Directory.GetFiles(oldDir)) TryDelete(f);
                Directory.CreateDirectory(newDir);
                Directory.CreateDirectory(oldDir);
                var files = Directory.GetFiles(stage);
                Array.Sort(files, StringComparer.OrdinalIgnoreCase);
                foreach (var f in files)
                {
                    var name = Path.GetFileName(f);
                    if (Array.Exists(UserFiles, u => u.Equals(name, StringComparison.OrdinalIgnoreCase)))
                        throw new InvalidOperationException("the setup package carries a user settings file (" + name + "); nothing was replaced");
                    var copy = Path.Combine(newDir, name);
                    File.Copy(f, copy, true);
                    if (!Sha256(copy).Equals(Sha256(f), StringComparison.Ordinal))
                        throw new InvalidOperationException("staged copy of " + name + " does not match the build");
                }
                Swap(files, dir, newDir, oldDir);
                foreach (var f in Directory.GetFiles(oldDir)) TryDelete(f);
            }
            finally
            {
                TryDeleteTree(newDir);
                if (Directory.Exists(oldDir) && Directory.GetFileSystemEntries(oldDir).Length == 0) TryDeleteTree(oldDir);
                TryDelete(marker);
                swapLock.Dispose();
            }
        }

        private static FileStream TakeLock(string dir)
        {
            try
            {
                // The lock is the exclusive handle, not the file: a lock file a power cut left behind
                // opens again (and goes when this handle closes); a held one is a sharing violation.
                return new FileStream(Path.Combine(dir, ".install-swap-lock"), FileMode.OpenOrCreate, FileAccess.Write,
                    FileShare.None, 4096, FileOptions.DeleteOnClose);
            }
            catch (IOException) { return null; }
        }

        /// A swap cut off earlier (its journal is still there): put the folder back to all old.
        /// Run at Setup start and before every swap; the app does the same before it starts
        /// (recall-native swap_journal.rs). Null when the folder is consistent, else why not (the
        /// journal is kept for the next try).
        public static string Recover(string dir)
        {
            if (!File.Exists(SwapJournal.PathIn(dir))) return null;
            var swapLock = TakeLock(dir);
            if (swapLock == null) return "another Recall install is swapping files in this folder";
            using (swapLock) return RecoverLocked(dir);
        }

        private static string RecoverLocked(string dir)
        {
            var path = SwapJournal.PathIn(dir);
            if (!File.Exists(path)) return null;
            var entries = SwapJournal.Read(path);
            if (entries == null) return "the swap journal is unreadable";
            var failures = SwapJournal.RollBack(dir, entries, Move);
            if (failures.Count > 0) return string.Join("; ", failures.ToArray());
            File.Delete(path);
            return null;
        }

        private static void Swap(string[] files, string dir, string newDir, string oldDir)
        {
            // On disk before the first move; deleted only when the folder is all new (success) or
            // all old again (a complete rollback).
            var journal = SwapJournal.Create(dir);
            var entries = new List<SwapJournal.Entry>();
            try
            {
                foreach (var f in files)
                {
                    var name = Path.GetFileName(f);
                    var e = new SwapJournal.Entry { Name = name, Had = File.Exists(Path.Combine(dir, name)) };
                    entries.Add(e);
                    if (e.Had)
                    {
                        journal.Moving(name);
                        e.Moved = true;
                        Move(Path.Combine(dir, name), Path.Combine(oldDir, name));
                    }
                    journal.Placing(name, e.Had);
                    e.Placed = true;
                    Move(Path.Combine(newDir, name), Path.Combine(dir, name));
                }
            }
            catch (Exception error)
            {
                // Any failure (IO, Win32 or anything else) undoes the swap; a failing step does not stop the rest.
                journal.Dispose();
                var failures = SwapJournal.RollBack(dir, entries, Move);
                if (failures.Count == 0)
                {
                    File.Delete(SwapJournal.PathIn(dir));
                    throw new InvalidOperationException("install rolled back, the previous version is unchanged (" + error.Message + ")");
                }
                throw new SwapIncomplete("install could not be fully rolled back (" + error.Message + "; "
                    + string.Join("; ", failures.ToArray()) + "). Setup finishes undoing it the next time it or Recall starts.");
            }
            journal.Dispose();
            File.Delete(SwapJournal.PathIn(dir));
        }

        private static void Move(string from, string to)
        {
            var hook = BeforeMove;
            if (hook != null) hook(from, to);
            MoveReplace(from, to);
        }

        private static void MoveReplace(string from, string to)
        {
            // MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH
            if (!Native.MoveFileExW(from, to, 0x1 | 0x8))
            {
                int code = Marshal.GetLastWin32Error();
                throw new Win32Exception(code, "move " + from + " -> " + to + ": error " + code);
            }
        }

        /// "&lt;pid&gt; &lt;FILETIME&gt;" for this process (recall-native install_wait.rs reads it).
        public static string MarkerValue()
        {
            using (var me = Process.GetCurrentProcess())
            {
                long created = SessionProcesses.CreationTime(me.Id);
                return created != 0 ? me.Id + " " + created : me.Id.ToString();
            }
        }

        public static string Sha256(string path)
        {
            using (var stream = File.OpenRead(path))
            using (var hash = SHA256.Create())
                return BitConverter.ToString(hash.ComputeHash(stream)).Replace("-", "");
        }

        private static void TryDelete(string path)
        {
            try { if (File.Exists(path)) File.Delete(path); }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
        }

        private static void TryDeleteTree(string path)
        {
            try { if (Directory.Exists(path)) Directory.Delete(path, true); }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
        }

        private static class Native
        {
            [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
            public static extern bool MoveFileExW(string from, string to, uint flags);
        }
    }
}
