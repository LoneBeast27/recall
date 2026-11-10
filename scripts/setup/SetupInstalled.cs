using System;
using System.Diagnostics;
using System.Windows.Forms;
using Microsoft.Win32;

namespace Recall.Setup
{
    /// The Recall this Windows account already has: the Apps &amp; Features entry's folder (only when
    /// that folder still holds a verified install) and the version Setup wrote there.
    internal sealed class InstalledRecall
    {
        public readonly string Path;
        public readonly string Version;

        public InstalledRecall(string path, string version) { Path = path; Version = version; }

        public static InstalledRecall Find(SetupTargets targets)
        {
            try
            {
                using (var key = Registry.CurrentUser.OpenSubKey(targets.UninstallKey))
                {
                    if (key == null) return null;
                    var location = key.GetValue("InstallLocation") as string;
                    var path = location == null ? null : SetupPackage.ExistingInstallPath(location);
                    return path == null ? null : new InstalledRecall(path, key.GetValue("DisplayVersion") as string);
                }
            }
            catch (System.Security.SecurityException) { return null; }
            catch (UnauthorizedAccessException) { return null; }
        }
    }

    /// What Setup opened by hand (no options) does before its window, decided with no I/O.
    internal enum LaunchCheck
    {
        /// Open the window.
        Continue,
        /// This version is installed and nothing newer is published: say so and exit.
        AlreadyLatest,
        /// This version is installed, a newer one is published: point to it and exit.
        InstalledButNewerExists,
        /// A newer version is published than this Setup carries: offer it, or continue.
        NewerExists,
        /// A newer version is installed than this Setup carries: ask before installing an older one.
        Older,
    }

    internal static class LaunchChecks
    {
        /// Opens the window even when this version is installed: a repair, run as an update.
        public const string RepairArgument = "/repair";

        /// <paramref name="latest"/> is the latest published version, or null (offline). A
        /// development Setup (0.0.0) is never checked. An update the app starts (`--update`, also its
        /// Reinstall) never comes here.
        public static LaunchCheck Decide(string installed, string setup, string latest)
        {
            if (SetupVersion.Parse(setup) == null || setup.Trim() == "0.0.0") return LaunchCheck.Continue;
            var setupVsLatest = SetupVersion.Compare(setup, latest);
            var installedVsSetup = SetupVersion.Compare(installed, setup);
            bool newer = setupVsLatest.HasValue && setupVsLatest.Value < 0;
            if (installedVsSetup == 0) return newer ? LaunchCheck.InstalledButNewerExists : LaunchCheck.AlreadyLatest;
            if (newer) return LaunchCheck.NewerExists;
            if (installedVsSetup.HasValue && installedVsSetup.Value > 0) return LaunchCheck.Older;
            return LaunchCheck.Continue;
        }

        /// Shows the notice the decision needs. True: open the window.
        public static bool Confirm(LaunchCheck check, InstalledRecall installed, string setup, string latest)
        {
            const string Title = "Recall Setup";
            switch (check)
            {
                case LaunchCheck.AlreadyLatest:
                    MessageBox.Show("Recall " + setup + " is already installed. You are on the latest version.\n\n" + installed.Path
                        + "\n\nTo repair it, run Setup again with " + RepairArgument + ".", Title, MessageBoxButtons.OK, MessageBoxIcon.Information);
                    return false;
                case LaunchCheck.InstalledButNewerExists:
                    if (MessageBox.Show("Recall " + setup + " is already installed, and Recall " + latest + " is now available.\n\n"
                        + "Open the download page for " + latest + "?", Title, MessageBoxButtons.YesNo, MessageBoxIcon.Information) == DialogResult.Yes)
                        OpenReleases();
                    return false;
                case LaunchCheck.NewerExists:
                    var answer = MessageBox.Show("Recall " + latest + " is available. This Setup installs Recall " + setup + ".\n\n"
                        + "Yes: open the download page for " + latest + ".\nNo: continue with " + setup + ".",
                        Title, MessageBoxButtons.YesNoCancel, MessageBoxIcon.Information);
                    if (answer == DialogResult.Yes) OpenReleases();
                    return answer == DialogResult.No;
                case LaunchCheck.Older:
                    return MessageBox.Show("Recall " + installed.Version + " is installed. This Setup installs the older Recall " + setup + ".\n\n"
                        + "Install " + setup + " anyway? Your clips and settings are kept.", Title,
                        MessageBoxButtons.YesNo, MessageBoxIcon.Question, MessageBoxDefaultButton.Button2) == DialogResult.Yes;
                default:
                    return true;
            }
        }

        private static void OpenReleases()
        {
            try { using (Process.Start(new ProcessStartInfo(UpdateFlow.ReleasesUrl) { UseShellExecute = true })) { } }
            catch (System.ComponentModel.Win32Exception) { }
        }
    }
}
