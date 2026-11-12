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
        /// "version+commit" a Setup with a source revision wrote; null for older Setups and dev builds.
        public readonly string Build;

        public InstalledRecall(string path, string version, string build) { Path = path; Version = version; Build = build; }

        public static InstalledRecall Find(SetupTargets targets)
        {
            try
            {
                using (var key = Registry.CurrentUser.OpenSubKey(targets.UninstallKey))
                {
                    if (key == null) return null;
                    var location = key.GetValue("InstallLocation") as string;
                    var path = location == null ? null : SetupPackage.ExistingInstallPath(location);
                    return path == null ? null : new InstalledRecall(path, key.GetValue("DisplayVersion") as string,
                        key.GetValue(AppsEntry.BuildIdValue) as string);
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
        /// This exact build is installed and nothing newer is published: say so and exit.
        AlreadyLatest,
        /// This version is installed, a newer one is published: point to it and exit.
        InstalledButNewerExists,
        /// A newer version is published than this Setup carries: offer it, or continue.
        NewerExists,
        /// A newer version is installed than this Setup carries: ask before installing an older one.
        Older,
    }

    /// `--expect-revision <commit>`: the app asked for a hotfix build. Another build of Setup (a
    /// release whose files were only partly replaced) refuses before anything is closed or changed.
    internal static class ExpectedBuild
    {
        public const string Argument = "--expect-revision";

        /// The refusal text, or null when this Setup is the expected build (or none was named).
        public static string Mismatch(string expected, string own)
        {
            if (expected == null) return null;
            if (string.Equals(expected, own, StringComparison.OrdinalIgnoreCase) && own.Length != 0) return null;
            return "This Setup is not the Recall build the update expected (expected " + Short(expected) + ", this Setup is "
                + (own.Length == 0 ? "an unstamped build" : Short(own)) + "). Nothing was changed. Try the update again later.";
        }

        private static string Short(string revision) { return revision.Length > 12 ? revision.Substring(0, 12) : revision; }
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
            return Decide(installed, null, setup, "", latest, false);
        }

        /// With builds: <paramref name="installedBuild"/> is the "version+commit" in the Apps entry
        /// (null from older Setups), <paramref name="setupRevision"/> this Setup's commit ("" when
        /// unstamped) and <paramref name="newerBuild"/> whether the latest release's commit is
        /// strictly after this Setup's (false when unknown or when this Setup is ahead). Same version,
        /// different build: a hotfix, installed as an update. Same build: already at the latest. A
        /// Setup whose published version has a newer build counts as outdated, like an older version.
        public static LaunchCheck Decide(string installed, string installedBuild, string setup, string setupRevision, string latest, bool newerBuild)
        {
            if (SetupVersion.Parse(setup) == null || setup.Trim() == "0.0.0") return LaunchCheck.Continue;
            var setupVsLatest = SetupVersion.Compare(setup, latest);
            var installedVsSetup = SetupVersion.Compare(installed, setup);
            bool stamped = !string.IsNullOrEmpty(setupRevision);
            bool outdatedBuild = setupVsLatest == 0 && stamped && newerBuild;
            bool newer = (setupVsLatest.HasValue && setupVsLatest.Value < 0) || outdatedBuild;
            if (installedVsSetup == 0)
            {
                if (newer) return LaunchCheck.InstalledButNewerExists;
                bool sameBuild = !stamped || string.Equals(installedBuild, setup.Trim() + "+" + setupRevision, StringComparison.OrdinalIgnoreCase);
                return sameBuild ? LaunchCheck.AlreadyLatest : LaunchCheck.Continue;
            }
            if (newer) return LaunchCheck.NewerExists;
            if (installedVsSetup.HasValue && installedVsSetup.Value > 0) return LaunchCheck.Older;
            return LaunchCheck.Continue;
        }

        /// Shows the notice the decision needs. True: open the window.
        public static bool Confirm(LaunchCheck check, InstalledRecall installed, string setup, string latest)
        {
            const string Title = "Recall Setup";
            // The same number published again with a newer build: a hotfix of that version.
            var available = SetupVersion.Compare(setup, latest) == 0 ? "a newer build of Recall " + latest : "Recall " + latest;
            var availableTitle = char.ToUpperInvariant(available[0]) + available.Substring(1);
            switch (check)
            {
                case LaunchCheck.AlreadyLatest:
                    MessageBox.Show("Recall " + setup + " is already installed. You are on the latest version.\n\n" + installed.Path
                        + "\n\nTo repair it, run Setup again with " + RepairArgument + ".", Title, MessageBoxButtons.OK, MessageBoxIcon.Information);
                    return false;
                case LaunchCheck.InstalledButNewerExists:
                    if (MessageBox.Show("Recall " + setup + " is already installed, and " + available + " is now available.\n\n"
                        + "Open the download page?", Title, MessageBoxButtons.YesNo, MessageBoxIcon.Information) == DialogResult.Yes)
                        OpenReleases();
                    return false;
                case LaunchCheck.NewerExists:
                    var answer = MessageBox.Show(availableTitle + " is available. This Setup installs Recall " + setup + ".\n\n"
                        + "Yes: open the download page.\nNo: continue with this Setup.",
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
