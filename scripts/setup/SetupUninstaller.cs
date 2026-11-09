using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Reflection;
using System.Threading;
using System.Windows.Forms;

namespace Recall.Setup
{
    /// `RecallSetup.exe /uninstall [/quiet] [/remove-cache]` (Apps &amp; Features). The copy in the
    /// install folder can't delete itself, so it starts a copy of itself from %TEMP% with
    /// /uninstall-from and exits. That copy quits the recorder (the same validated pipe quit as an
    /// update), removes only the files install-manifest.json lists (and only unchanged ones), the
    /// Start menu shortcut, the Run value and the Apps &amp; Features entry. The cache is kept unless
    /// the user ticks the box (default off); then only provably regenerable cache files are deleted
    /// and everything else in it goes to the Recycle Bin when the bin can take it, or stays. Never
    /// a recursive delete of the install folder; never a clip, recordings or linked folder, or the
    /// user's settings. A folder that still holds anything is left in place, and the end says so.
    internal static class Uninstaller
    {
        public const string Argument = "/uninstall";
        public const string FromArgument = "/uninstall-from";
        public const string QuietArgument = "/quiet";
        public const string RemoveCacheArgument = "/remove-cache";
        private const string Title = "Uninstall Recall";

        public static int Begin(bool quiet, bool removeCache)
        {
            var exe = Assembly.GetExecutingAssembly().Location;
            var dir = Path.GetDirectoryName(exe);
            var temp = Path.Combine(Path.GetTempPath(), "Recall-Uninstall-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(temp);
            var copy = Path.Combine(temp, AppsEntry.UninstallerName);
            File.Copy(exe, copy);
            var args = FromArgument + " \"" + dir + "\" " + Process.GetCurrentProcess().Id
                + (quiet ? " " + QuietArgument : "") + (removeCache ? " " + RemoveCacheArgument : "");
            using (Process.Start(new ProcessStartInfo(copy, args) { UseShellExecute = false, WorkingDirectory = temp })) { }
            return 0;
        }

        public static int Run(string dir, int parentPid, bool quiet, bool removeCache)
        {
            Action<string> log = line => { };
            try { return Uninstall(dir, parentPid, quiet, removeCache, ref log); }
            finally { TempCopy.ScheduleRemoval(log); }
        }

        private static int Uninstall(string dir, int parentPid, bool quiet, bool removeCache, ref Action<string> log)
        {
            try
            {
                var targets = SetupTargets.FromEnvironment();
                log = SetupLog.For(targets);
                dir = PathId.Normalize(Path.GetFullPath(dir)).TrimEnd('\\');
                log("Uninstall " + dir + (targets.IsSandbox ? " (sandbox " + targets.SandboxRoot + ")" : ""));
                targets.CheckInstallDir(dir);
                var manifest = InstallManifest.Read(dir);
                if (manifest == null)
                    throw new InvalidOperationException(dir + " has no readable " + InstallManifest.Name + ", so Setup can't tell which files it installed. Nothing was removed.");
                if (!quiet)
                {
                    bool? choice = UninstallDialog.Ask(dir);
                    if (choice == null) { log("Uninstall cancelled."); return 1602; } // ERROR_INSTALL_USEREXIT
                    removeCache = choice.Value;
                }
                WaitForExit(parentPid);
                var quitter = new RecorderQuitter(targets, dir, true, log);
                quitter.EnsureAppClosed();
                quitter.Stop(Installer.QuitWaitMinutes);
                var guard = ClipFolders.Read();
                var notes = new List<string>();
                int removed = RemoveProgramFiles(dir, manifest, guard, log);
                if (removeCache) CleanCache(dir, guard, log, notes);
                else log("Cache kept.");
                Tidy(targets, dir, log);
                var left = Leftovers(dir);
                var message = "Recall was removed (" + removed + " program files). Your clips and settings were kept.";
                if (left.Count > 0)
                    message += "\n\nThe folder " + dir + " was left in place: it still holds " + string.Join(", ", left.ToArray()) + ".";
                foreach (var n in notes) message += "\n\n" + n;
                log("Uninstall finished: " + message.Replace("\n\n", " "));
                if (!quiet) MessageBox.Show(message, Title, MessageBoxButtons.OK, MessageBoxIcon.Information);
                return 0;
            }
            catch (Exception error)
            {
                log("Uninstall failed: " + error.Message);
                if (!quiet) MessageBox.Show(error.Message, Title, MessageBoxButtons.OK, MessageBoxIcon.Error);
                return 1;
            }
        }

        /// The copy in the install folder exits right after starting this one; its file is removed below.
        private static void WaitForExit(int pid)
        {
            if (pid <= 0) return;
            try { using (var parent = Process.GetProcessById(pid)) parent.WaitForExit(30000); }
            catch (ArgumentException) { }
            catch (InvalidOperationException) { }
        }

        private static int RemoveProgramFiles(string dir, List<ManifestEntry> manifest, List<string> guard, Action<string> log)
        {
            var root = PathId.Root(dir);
            var files = UninstallPlan.ProgramFiles(root, manifest, guard, e => InstallManifest.Unchanged(dir, e));
            foreach (var e in manifest)
                if (!files.Contains(root + e.Path)) log("Kept (changed, missing or outside the program files): " + e.Path);
            int removed = 0;
            foreach (var file in files) if (Delete(file, root, guard, log)) removed++;
            if (Delete(InstallManifest.PathIn(dir), root, guard, log)) removed++;
            return removed;
        }

        private static bool Delete(string file, string root, List<string> guard, Action<string> log)
        {
            // Checked again on the resolved spelling right before each delete.
            if (!UninstallPlan.Allowed(PathId.Normalize(file), root, guard)) { log("Kept (protected): " + file); return false; }
            for (int attempt = 0; ; attempt++)
            {
                try
                {
                    if (!File.Exists(file)) return false;
                    File.Delete(file);
                    return true;
                }
                catch (Exception error)
                {
                    if (!(error is IOException || error is UnauthorizedAccessException)) throw;
                    if (attempt >= 10) { log("Could not remove " + file + ": " + error.Message); return false; }
                    Thread.Sleep(500);
                }
            }
        }

        private static void CleanCache(string dir, List<string> guard, Action<string> log, List<string> notes)
        {
            var cache = Path.Combine(dir, "cache");
            if (!Directory.Exists(cache) || IsReparse(cache)) return;
            var root = PathId.Root(dir);
            var relative = new List<string>();
            Walk(cache, cache.Length + 1, relative);
            var regenerable = new List<string>();
            var others = new List<string>();
            CachePlan.Split(relative, regenerable, others);
            int deleted = 0;
            foreach (var r in regenerable) if (Delete(Path.Combine(cache, r), root, guard, log)) deleted++;
            log("Cache: deleted " + deleted + " regenerable file(s).");
            var recycle = new List<string>();
            long bytes = 0;
            foreach (var r in others)
            {
                var path = Path.Combine(cache, r);
                if (!UninstallPlan.Allowed(PathId.Normalize(path), root, guard)) continue;
                recycle.Add(path);
                bytes += new FileInfo(path).Length;
            }
            if (recycle.Count > 0)
            {
                var why = RecycleBin.Check(cache, bytes) ?? RecycleBin.Move(recycle);
                if (why == null) log("Cache: moved " + recycle.Count + " file(s) to the Recycle Bin (kept clip copies and other files).");
                else
                {
                    log("Cache: kept " + recycle.Count + " file(s) that are not regenerable: " + why + ".");
                    notes.Add(recycle.Count + " cache file(s) that can't be recreated (such as kept clip copies) were left in "
                        + cache + ": " + why + ".");
                }
            }
            RemoveEmptyFolders(cache);
        }

        private static bool IsReparse(string path)
        {
            return (File.GetAttributes(path) & FileAttributes.ReparsePoint) != 0;
        }

        /// Every file under <paramref name="folder"/>, relative; linked folders are not entered.
        private static void Walk(string folder, int prefix, List<string> files)
        {
            foreach (var f in Directory.GetFiles(folder)) files.Add(f.Substring(prefix));
            foreach (var d in Directory.GetDirectories(folder)) if (!IsReparse(d)) Walk(d, prefix, files);
        }

        private static void RemoveEmptyFolders(string folder)
        {
            if (IsReparse(folder)) return;
            foreach (var d in Directory.GetDirectories(folder)) RemoveEmptyFolders(d);
            try { if (Directory.GetFileSystemEntries(folder).Length == 0) Directory.Delete(folder); }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
        }

        /// What still sits in the install folder; the folder itself goes only when empty.
        private static List<string> Leftovers(string dir)
        {
            var left = new List<string>();
            if (!Directory.Exists(dir)) return left;
            try { if (Directory.GetFileSystemEntries(dir).Length == 0) { Directory.Delete(dir); return left; } }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
            foreach (var entry in Directory.GetFileSystemEntries(dir))
            {
                if (left.Count == 5) { left.Add("…"); break; }
                left.Add(Path.GetFileName(entry) + (Directory.Exists(entry) ? "\\" : ""));
            }
            return left;
        }

        private static void Tidy(SetupTargets targets, string dir, Action<string> log)
        {
            var target = Shortcut.Target(targets.Shortcut);
            if (target != null && PathId.IsInside(target, PathId.Root(dir)))
            {
                File.Delete(targets.Shortcut);
                log("Removed the Start menu shortcut " + targets.Shortcut);
            }
            if (RunValue.RemoveIfInto(targets, dir)) log("Removed Start with Windows (HKCU\\" + targets.RunKey + ")");
            if (AppsEntry.Remove(targets, dir)) log("Removed the Apps & Features entry (HKCU\\" + targets.UninstallKey + ")");
        }
    }

    /// The %TEMP%\Recall-Uninstall-&lt;guid&gt; copy removes itself: when it ends, a hidden cmd.exe
    /// retries `rmdir` on that one folder for up to a minute (it can't go while this exe runs), and
    /// the folder is also queued for removal at the next boot when Windows allows it.
    internal static class TempCopy
    {
        public const string Prefix = "Recall-Uninstall-";

        /// Only a folder named Recall-Uninstall-&lt;32 hex&gt; directly inside the temp folder.
        public static bool IsOurFolder(string folder, string tempRoot)
        {
            if (string.IsNullOrEmpty(folder) || string.IsNullOrEmpty(tempRoot)) return false;
            var parent = Path.GetDirectoryName(folder.TrimEnd('\\'));
            var name = Path.GetFileName(folder.TrimEnd('\\'));
            if (parent == null || !parent.TrimEnd('\\').Equals(tempRoot.TrimEnd('\\'), StringComparison.OrdinalIgnoreCase)) return false;
            if (!name.StartsWith(Prefix, StringComparison.Ordinal) || name.Length != Prefix.Length + 32) return false;
            foreach (var c in name.Substring(Prefix.Length)) if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))) return false;
            return true;
        }

        /// cmd.exe arguments: up to 60 tries, about a second apart, then it gives up.
        public static string CleanupArguments(string folder)
        {
            return "/d /q /c \"for /l %i in (1,1,60) do (rmdir /s /q \"" + folder + "\" 2>nul & if not exist \"" + folder
                + "\" exit /b 0 & ping -n 2 127.0.0.1 >nul)\"";
        }

        public static void ScheduleRemoval(Action<string> log)
        {
            try
            {
                var exe = Assembly.GetExecutingAssembly().Location;
                var folder = Path.GetDirectoryName(exe);
                var temp = Path.GetTempPath();
                if (!IsOurFolder(folder, temp)) return;
                // Next-boot removal needs administrator rights; a standard user's request is simply refused.
                Native.MoveFileExW(exe, null, 0x4);    // MOVEFILE_DELAY_UNTIL_REBOOT
                Native.MoveFileExW(folder, null, 0x4);
                var cmd = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "cmd.exe");
                using (Process.Start(new ProcessStartInfo(cmd, CleanupArguments(folder)) {
                    UseShellExecute = false, CreateNoWindow = true, WorkingDirectory = temp
                })) { }
                log("Temporary copy " + folder + " will be removed once this uninstaller exits.");
            }
            catch (Exception error) { log("Could not schedule removal of the temporary copy: " + error.Message); }
        }

        private static class Native
        {
            [System.Runtime.InteropServices.DllImport("kernel32.dll", CharSet = System.Runtime.InteropServices.CharSet.Unicode, SetLastError = true)]
            public static extern bool MoveFileExW(string from, string to, uint flags);
        }
    }

    /// The uninstall question: Uninstall / Cancel, and "Also remove Recall's cache" (default off).
    internal static class UninstallDialog
    {
        /// Null = cancelled; otherwise whether the cache box was ticked.
        public static bool? Ask(string dir)
        {
            using (var form = new Form {
                Text = "Uninstall Recall", FormBorderStyle = FormBorderStyle.FixedDialog, MaximizeBox = false, MinimizeBox = false,
                StartPosition = FormStartPosition.CenterScreen, ClientSize = new System.Drawing.Size(520, 230), Font = new System.Drawing.Font("Segoe UI", 9.5f)
            })
            {
                form.Controls.Add(new Label {
                    Text = "Remove Recall from " + dir + "?\n\nYour clips, recordings folders, linked folders and settings are kept.",
                    Bounds = new System.Drawing.Rectangle(16, 14, 488, 72)
                });
                var cache = new CheckBox {
                    Text = "Also remove Recall's cache", Checked = false, Bounds = new System.Drawing.Rectangle(16, 92, 488, 24)
                };
                form.Controls.Add(cache);
                form.Controls.Add(new Label {
                    Text = "Deletes previews, thumbnails and audio stems, which Recall can recreate. Kept clip copies and anything else "
                        + "in the cache go to the Recycle Bin, or stay if the Recycle Bin can't take them.",
                    Bounds = new System.Drawing.Rectangle(34, 118, 470, 52), ForeColor = System.Drawing.SystemColors.GrayText
                });
                var ok = new Button { Text = "Uninstall", DialogResult = DialogResult.OK, Bounds = new System.Drawing.Rectangle(322, 186, 90, 30) };
                var cancel = new Button { Text = "Cancel", DialogResult = DialogResult.Cancel, Bounds = new System.Drawing.Rectangle(418, 186, 90, 30) };
                form.Controls.Add(ok);
                form.Controls.Add(cancel);
                form.AcceptButton = ok;
                form.CancelButton = cancel;
                return form.ShowDialog() == DialogResult.OK ? cache.Checked : (bool?)null;
            }
        }
    }
}
