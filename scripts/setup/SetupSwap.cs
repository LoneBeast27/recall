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
    ///  4. Any failure puts every file back from `.old` (new files that had no old one are removed):
    ///     the folder is all old or all new. Success empties `.old` (a file still in use stays there).
    internal static class InstallSwap
    {
        public static void Run(string stage, string dir)
        {
            Directory.CreateDirectory(dir);
            var marker = Path.Combine(dir, ".installing");
            var newDir = Path.Combine(dir, ".new");
            var oldDir = Path.Combine(dir, ".old");
            FileStream swapLock;
            try
            {
                swapLock = new FileStream(Path.Combine(dir, ".install-swap-lock"), FileMode.CreateNew, FileAccess.Write,
                    FileShare.None, 4096, FileOptions.DeleteOnClose);
            }
            catch (IOException)
            {
                throw new InvalidOperationException("Another Recall install is swapping files in this folder. Wait for it to finish, then try again.");
            }
            try
            {
                File.WriteAllText(marker, MarkerValue());
                TryDeleteTree(newDir);
                if (Directory.Exists(oldDir)) foreach (var f in Directory.GetFiles(oldDir)) TryDelete(f);
                Directory.CreateDirectory(newDir);
                Directory.CreateDirectory(oldDir);
                var files = Directory.GetFiles(stage);
                Array.Sort(files, StringComparer.OrdinalIgnoreCase);
                foreach (var f in files)
                {
                    var copy = Path.Combine(newDir, Path.GetFileName(f));
                    File.Copy(f, copy, true);
                    if (!Sha256(copy).Equals(Sha256(f), StringComparison.Ordinal))
                        throw new InvalidOperationException("staged copy of " + Path.GetFileName(f) + " does not match the build");
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

        private sealed class Moved
        {
            public string Target;
            public string Backup;
            public bool Had;
        }

        private static void Swap(string[] files, string dir, string newDir, string oldDir)
        {
            var done = new List<Moved>();
            try
            {
                foreach (var f in files)
                {
                    var name = Path.GetFileName(f);
                    var move = new Moved { Target = Path.Combine(dir, name), Backup = Path.Combine(oldDir, name) };
                    move.Had = File.Exists(move.Target);
                    if (move.Had) MoveReplace(move.Target, move.Backup);
                    done.Add(move);
                    MoveReplace(Path.Combine(newDir, name), move.Target);
                }
            }
            catch (Exception error)
            {
                if (!(error is IOException || error is UnauthorizedAccessException || error is Win32Exception)) throw;
                for (int i = done.Count - 1; i >= 0; i--)
                {
                    var d = done[i];
                    if (d.Had) { if (File.Exists(d.Backup)) MoveReplace(d.Backup, d.Target); }
                    else if (File.Exists(d.Target)) File.Delete(d.Target);
                }
                throw new InvalidOperationException("install rolled back, the previous version is unchanged (" + error.Message + ")");
            }
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
