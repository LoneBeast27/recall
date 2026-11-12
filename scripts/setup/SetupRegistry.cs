using System;
using System.IO;
using Microsoft.Win32;

namespace Recall.Setup
{
    /// The per-user Apps & Features entry (HKCU\...\Uninstall\Recall, or the sandbox key).
    internal static class AppsEntry
    {
        public const string Publisher = "LoneBeast27";
        public const string UninstallerName = "RecallSetup.exe";
        public const string BuildIdValue = "RecallBuild";

        /// "version+commit" of this Setup, or null when it carries no source revision (dev builds).
        public static string BuildId
        {
            get { return SetupBuildInfo.Revision.Length == 0 ? null : SetupBuildInfo.Version + "+" + SetupBuildInfo.Revision; }
        }

        public static string UninstallCommand(string installDir, bool quiet)
        {
            return "\"" + Path.Combine(installDir, UninstallerName) + "\" /uninstall" + (quiet ? " /quiet" : "");
        }

        public static void Write(SetupTargets targets, string installDir, long sizeKb)
        {
            using (var key = Registry.CurrentUser.CreateSubKey(targets.UninstallKey))
            {
                key.SetValue("DisplayName", "Recall", RegistryValueKind.String);
                key.SetValue("DisplayVersion", SetupBuildInfo.Version, RegistryValueKind.String);
                // The exact build (version+commit), so a same-version hotfix Setup can tell it apart.
                if (BuildId != null) key.SetValue(BuildIdValue, BuildId, RegistryValueKind.String);
                else key.DeleteValue(BuildIdValue, false);
                key.SetValue("Publisher", Publisher, RegistryValueKind.String);
                key.SetValue("DisplayIcon", Path.Combine(installDir, "recall.ico"), RegistryValueKind.String);
                key.SetValue("InstallLocation", installDir, RegistryValueKind.String);
                key.SetValue("UninstallString", UninstallCommand(installDir, false), RegistryValueKind.String);
                key.SetValue("QuietUninstallString", UninstallCommand(installDir, true), RegistryValueKind.String);
                key.SetValue("EstimatedSize", (int)Math.Min(int.MaxValue, Math.Max(0, sizeKb)), RegistryValueKind.DWord);
                key.SetValue("NoModify", 1, RegistryValueKind.DWord);
                key.SetValue("NoRepair", 1, RegistryValueKind.DWord);
            }
        }

        /// Removes the entry only when it names this install folder (another install's entry stays).
        public static bool Remove(SetupTargets targets, string installDir)
        {
            using (var key = Registry.CurrentUser.OpenSubKey(targets.UninstallKey))
            {
                if (key == null) return false;
                var location = key.GetValue("InstallLocation") as string;
                if (location == null || !PathId.Same(location, installDir)) return false;
            }
            Registry.CurrentUser.DeleteSubKeyTree(targets.UninstallKey, false);
            return true;
        }

        /// Program files only: everything under the folder except cache\.
        public static long ProgramSizeKb(string installDir)
        {
            long bytes = 0;
            var cache = PathId.Root(Path.Combine(installDir, "cache"));
            foreach (var file in Directory.GetFiles(installDir, "*", SearchOption.AllDirectories))
            {
                if (PathId.IsInside(file, cache)) continue;
                try { bytes += new FileInfo(file).Length; }
                catch (IOException) { }
            }
            return (bytes + 1023) / 1024;
        }
    }

    /// The recorder's Start with Windows value (recall-hdr config_autostart.rs: `recall-hdr` =
    /// `"<exe>" --record`). The recorder owns it; Setup only removes it on uninstall, when it
    /// points into the folder being removed.
    internal static class RunValue
    {
        public const string Name = "recall-hdr";

        /// Does the Run data start the exe from inside <paramref name="installRoot"/> (from PathId.Root)?
        public static bool PointsInto(string data, string installRoot)
        {
            var exe = ExePath(data);
            if (exe == null) return false;
            string full;
            try { full = Path.GetFullPath(exe); }
            catch (ArgumentException) { return false; }
            catch (NotSupportedException) { return false; }
            return PathId.IsInside(full, installRoot) || PathId.IsInside(PathId.Normalize(full), installRoot);
        }

        internal static string ExePath(string data)
        {
            if (string.IsNullOrWhiteSpace(data)) return null;
            data = data.Trim();
            if (data[0] == '"')
            {
                int end = data.IndexOf('"', 1);
                return end > 1 ? data.Substring(1, end - 1) : null;
            }
            int space = data.IndexOf(' ');
            return space > 0 ? data.Substring(0, space) : data;
        }

        public static string Read(SetupTargets targets)
        {
            using (var key = Registry.CurrentUser.OpenSubKey(targets.RunKey))
                return key == null ? null : key.GetValue(Name) as string;
        }

        public static bool RemoveIfInto(SetupTargets targets, string installDir)
        {
            var data = Read(targets);
            if (data == null || !PointsInto(data, PathId.Root(installDir))) return false;
            using (var key = Registry.CurrentUser.OpenSubKey(targets.RunKey, true))
                if (key != null) key.DeleteValue(Name, false);
            return true;
        }
    }
}
