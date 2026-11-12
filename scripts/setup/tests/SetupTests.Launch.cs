using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;

namespace Recall.Setup.Tests
{
    /// Setup opened by hand: the version checks before the window, and an install over an existing
    /// Recall that must run as an update (settings untouched, no guided setup).
    internal static partial class TestRunner
    {
        private static void LaunchDecisions()
        {
            // Same version, nothing newer (online or offline): already at the latest.
            Check(LaunchChecks.Decide("0.1.2", "0.1.2", "0.1.2") == LaunchCheck.AlreadyLatest, "same, latest known");
            Check(LaunchChecks.Decide("0.1.2", "0.1.2", null) == LaunchCheck.AlreadyLatest, "same, offline");
            Check(LaunchChecks.Decide(" 0.1.2 ", "0.1.2", null) == LaunchCheck.AlreadyLatest, "spaces in the registry value");
            // Same version, but a newer one is published: point to it.
            Check(LaunchChecks.Decide("0.1.1", "0.1.1", "0.1.2") == LaunchCheck.InstalledButNewerExists, "same, newer published");
            // An older Setup while online: offered the latest, may continue (a rollback stays possible).
            Check(LaunchChecks.Decide("0.1.0", "0.1.1", "0.1.2") == LaunchCheck.NewerExists, "setup older than latest");
            Check(LaunchChecks.Decide(null, "0.1.1", "0.2.0") == LaunchCheck.NewerExists, "no version recorded");
            // Rollback: an older Setup over a newer install asks first.
            Check(LaunchChecks.Decide("0.1.2", "0.1.1", null) == LaunchCheck.Older, "rollback, offline");
            Check(LaunchChecks.Decide("0.1.2", "0.1.1", "0.1.1") == LaunchCheck.Older, "rollback, latest pulled back");
            Check(LaunchChecks.Decide("0.1.2", "0.1.1", "0.1.2") == LaunchCheck.NewerExists, "rollback while a newer is published: offered first");
            // A real update.
            Check(LaunchChecks.Decide("0.1.1", "0.1.2", "0.1.2") == LaunchCheck.Continue, "update to latest");
            Check(LaunchChecks.Decide("0.1.1", "0.1.2", null) == LaunchCheck.Continue, "update, offline");
            Check(LaunchChecks.Decide("0.1.1", "0.1.3", "0.1.2") == LaunchCheck.Continue, "Setup newer than the published latest");
            // A development Setup is never checked.
            Check(LaunchChecks.Decide("0.0.0", "0.0.0", null) == LaunchCheck.Continue, "dev build");
            Check(LaunchChecks.Decide("0.1.1", "0.0.0", "0.1.1") == LaunchCheck.Continue, "dev build over a release");
            Check(LaunchChecks.Decide("garbage", "0.1.1", "nonsense") == LaunchCheck.Continue, "unreadable versions");
            Check(SetupVersion.Compare("0.10.0", "0.9.9") > 0, "numeric, not text, order");
            HotfixDecisions();
            Check(SetupVersion.Parse("1.2") == null && SetupVersion.Parse("1.2.x") == null && SetupVersion.Parse("-1.0.0") == null, "bad versions");
        }

        /// Same-version hotfixes: the build (version+commit) decides, not only the number.
        private static void HotfixDecisions()
        {
            const string Old = "1111111111111111111111111111111111111111";
            const string Fix = "2222222222222222222222222222222222222222";
            const string Later = "3333333333333333333333333333333333333333";
            // Today's 0.1.5 (an older Setup wrote no build), the hotfix Setup run by hand: installs.
            Check(LaunchChecks.Decide("0.1.5", null, "0.1.5", Fix, "0.1.5", Fix) == LaunchCheck.Continue, "hotfix over an unstamped install");
            Check(LaunchChecks.Decide("0.1.5", "0.1.5+" + Old, "0.1.5", Fix, "0.1.5", Fix) == LaunchCheck.Continue, "hotfix over the earlier build");
            Check(LaunchChecks.Decide("0.1.5", "0.1.5+" + Old, "0.1.5", Fix, null, null) == LaunchCheck.Continue, "hotfix, offline");
            // Run again once it is installed: the same build is already the latest, online or not.
            Check(LaunchChecks.Decide("0.1.5", "0.1.5+" + Fix, "0.1.5", Fix, "0.1.5", Fix) == LaunchCheck.AlreadyLatest, "same build");
            Check(LaunchChecks.Decide("0.1.5", "0.1.5+" + Fix, "0.1.5", Fix, null, null) == LaunchCheck.AlreadyLatest, "same build, offline");
            Check(LaunchChecks.Decide("0.1.5", "0.1.5+" + Fix.ToUpperInvariant(), "0.1.5", Fix, "0.1.5", null) == LaunchCheck.AlreadyLatest, "same build, any case");
            // An unstamped Setup of the installed version never reinstalls it.
            Check(LaunchChecks.Decide("0.1.5", "0.1.5+" + Fix, "0.1.5", "", "0.1.5", Fix) == LaunchCheck.AlreadyLatest, "unstamped Setup");
            // A Setup that is not the published build of its version (an earlier download): outdated.
            Check(LaunchChecks.Decide("0.1.5", "0.1.5+" + Later, "0.1.5", Fix, "0.1.5", Later) == LaunchCheck.InstalledButNewerExists, "older build over the newer one");
            Check(LaunchChecks.Decide("0.1.5", "0.1.5+" + Old, "0.1.5", Fix, "0.1.5", Later) == LaunchCheck.InstalledButNewerExists, "a later hotfix is published");
            Check(LaunchChecks.Decide("0.1.4", null, "0.1.5", Fix, "0.1.5", Later) == LaunchCheck.NewerExists, "older version, outdated build of the new one");
            Check(LaunchChecks.Decide("0.1.4", null, "0.1.5", Fix, "0.1.5", Fix) == LaunchCheck.Continue, "an ordinary update");
            // A build id from another version never counts as this build.
            Check(LaunchChecks.Decide("0.1.5", "0.1.4+" + Fix, "0.1.5", Fix, "0.1.5", Fix) == LaunchCheck.Continue, "stale build id");
            // The newer version check still comes first.
            Check(LaunchChecks.Decide("0.1.5", "0.1.5+" + Fix, "0.1.5", Fix, "0.1.6", null) == LaunchCheck.InstalledButNewerExists, "newer version published");
            Check(LaunchChecks.Decide("0.1.6", null, "0.1.5", Fix, "0.1.5", Fix) == LaunchCheck.Older, "rollback still asks");
            Check(LatestRelease.RevisionOf("\uFEFF" + Fix.ToUpperInvariant() + "\r\n") == Fix, "SOURCE-REVISION.txt");
            Check(LatestRelease.RevisionOf("abc") == null && LatestRelease.RevisionOf("") == null && LatestRelease.RevisionOf(null) == null, "bad SOURCE-REVISION.txt");
            Check(LatestRelease.RevisionOf(new string('g', 40)) == null, "not hex");
        }

        private static void UpdateChecksOffSwitch()
        {
            var root = Path.Combine(Path.GetTempPath(), "recall-setup-checks-test-" + Guid.NewGuid().ToString("N"));
            var dir = Path.Combine(root, "Recall", "updates");
            var file = Path.Combine(dir, "settings.json");
            try
            {
                Check(LatestRelease.ChecksAllowed(null) && LatestRelease.ChecksAllowed(root), "no settings file: on, as in the app");
                Directory.CreateDirectory(dir);
                File.WriteAllText(file, "{\"check_automatically\":false}");
                Check(!LatestRelease.ChecksAllowed(root), "turned off in the app: Setup never calls GitHub");
                File.WriteAllText(file, "{\"check_automatically\":true,\"from_a_newer_build\":[1]}");
                Check(LatestRelease.ChecksAllowed(root), "on");
                File.WriteAllText(file, "not json");
                Check(LatestRelease.ChecksAllowed(root), "unreadable: on, as in the app");
                File.WriteAllText(file, "{\"check_automatically\":\"no\"}");
                Check(LatestRelease.ChecksAllowed(root), "not a bool: on, as in the app");
            }
            finally
            {
                try { Directory.Delete(root, true); } catch (IOException) { } catch (UnauthorizedAccessException) { }
            }
        }

        private static void LatestReleaseParsing()
        {
            const string setup = "{\"name\":\"RecallSetup.exe\",\"state\":\"uploaded\"}";
            Check(LatestRelease.VersionOf("{\"tag_name\":\"v0.1.2\",\"assets\":[" + setup + "]}") == "0.1.2", "a release with its Setup");
            Check(LatestRelease.VersionOf("{\"tag_name\":\"v0.1.2\",\"assets\":[{\"name\":\"RecallSetup.exe\",\"state\":\"new\"}]}") == null, "upload not finished");
            Check(LatestRelease.VersionOf("{\"tag_name\":\"v0.1.2\",\"assets\":[]}") == null, "no Setup asset");
            Check(LatestRelease.VersionOf("{\"tag_name\":\"v0.1.2\",\"draft\":true,\"assets\":[" + setup + "]}") == null, "draft");
            Check(LatestRelease.VersionOf("{\"tag_name\":\"v0.1.2\",\"prerelease\":true,\"assets\":[" + setup + "]}") == null, "pre-release");
            Check(LatestRelease.VersionOf("{\"tag_name\":\"0.1.2\",\"assets\":[" + setup + "]}") == null, "tag without v");
            Check(LatestRelease.VersionOf("not json") == null && LatestRelease.VersionOf("[]") == null, "garbage");
            Check(LatestRelease.OnMain("{\"status\":\"ahead\",\"behind_by\":0}"), "ahead of the tag: on main");
            Check(LatestRelease.OnMain("{\"status\":\"identical\",\"behind_by\":0}"), "main is the tag");
            Check(!LatestRelease.OnMain("{\"status\":\"diverged\",\"behind_by\":2}"), "a tag off main is never the latest");
            Check(!LatestRelease.OnMain("{\"status\":\"behind\",\"behind_by\":1}"), "behind");
            Check(!LatestRelease.OnMain("{}"), "missing fields");
        }

        private static void OverExistingIsUpdate()
        {
            using (var box = new Sandbox())
            {
                Check(SetupPackage.RunsAsUpdate(false, box.Install), "opened by hand over a verified install: an update");
                Check(SetupPackage.RunsAsUpdate(true, box.Install), "the app asked: an update");
                var empty = Path.Combine(box.Root, @"Programs\Elsewhere");
                Directory.CreateDirectory(empty);
                Check(!SetupPackage.RunsAsUpdate(false, empty), "an empty folder: a first install");
                File.Delete(Path.Combine(box.Install, "recall.exe"));
                Check(!SetupPackage.RunsAsUpdate(false, box.Install), "an incomplete folder is not a verified install");
            }
        }

        private static void OverExistingKeepsSettings()
        {
            using (var box = new Sandbox())
            {
                // Every settings file Recall writes, including keys a newer build added (a rollback).
                var seeded = new Dictionary<string, string>();
                foreach (var file in ConfigBackup.Files)
                {
                    var root = file[0] == "roaming" ? box.Roaming : file[0] == "local" ? box.Local : box.Install;
                    var path = Path.Combine(root, file[1]);
                    Directory.CreateDirectory(Path.GetDirectoryName(path));
                    var text = path.EndsWith(".toml", StringComparison.OrdinalIgnoreCase)
                        ? box.Config(97) + "[from_a_newer_build]\nkey = 1\n"
                        : "{\"user\":\"" + Path.GetFileName(path) + "\",\"newer_key\":[1,2]}";
                    File.WriteAllText(path, text);
                    seeded[path] = InstallSwap.Sha256(path);
                }
                var options = new InstallOptions(box.Install, true, false, false, SetupPackage.RunsAsUpdate(false, box.Install), false, true);
                Exception error = null;
                try { Installer.Run(box.Package, box.Work, options, null); }
                catch (Exception e) { error = e; }
                Check(error == null, "the update ran: " + (error == null ? "" : error.Message));
                Check(Sandbox.Programs.All(p => box.Version(p) == "NEW"), "the programs were replaced");
                foreach (var pair in seeded)
                    Check(File.Exists(pair.Key) && InstallSwap.Sha256(pair.Key) == pair.Value, "unchanged: " + pair.Key);
                var backups = ConfigBackup.Backups(new UserRoots(box.Roaming, box.Local).Updates);
                Check(backups.Count == 1, "one settings backup was made first (" + backups.Count + ")");
            }
        }

        private static void SetAsideNeverOverwrites()
        {
            var root = Path.Combine(Path.GetTempPath(), "recall-setup-aside-test-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(root);
            try
            {
                var file = Path.Combine(root, "library.json");
                File.WriteAllBytes(file, new byte[] { 0, 0 });
                var first = ConfigBackup.SetAside(file);
                File.WriteAllBytes(file, new byte[] { 0, 1 });
                var second = ConfigBackup.SetAside(file);
                Check(first != second && !File.Exists(file), "a second set-aside in the same second gets its own name");
                Check(File.ReadAllBytes(first).SequenceEqual(new byte[] { 0, 0 }) && File.ReadAllBytes(second).SequenceEqual(new byte[] { 0, 1 }), "both damaged copies kept");
                // A file that cannot be moved (held open without delete sharing): Restore must skip it.
                var backup = Path.Combine(root, "backup");
                Directory.CreateDirectory(Path.Combine(backup, "roaming", "Recall"));
                File.WriteAllText(Path.Combine(backup, "roaming", @"Recall\folders.json"), "{\"recordings\":\"from-backup\"}");
                File.WriteAllText(Path.Combine(backup, ConfigBackup.ListName), "roaming\tRecall\\folders.json\n");
                var live = Path.Combine(root, "Roaming", "Recall", "folders.json");
                Directory.CreateDirectory(Path.GetDirectoryName(live));
                File.WriteAllBytes(live, new byte[] { 0 });
                var lines = new List<string>();
                int restored;
                using (new FileStream(live, FileMode.Open, FileAccess.Read, FileShare.Read))
                    restored = ConfigBackup.Restore(backup, new UserRoots(Path.Combine(root, "Roaming"), Path.Combine(root, "Local")), root, lines.Add);
                Check(restored == 0 && File.ReadAllBytes(live).SequenceEqual(new byte[] { 0 }), "a file that could not be set aside is left as it was, not copied over");
                Check(lines.Any(l => l.Contains("could not be set aside")), "and the skip is logged");
            }
            finally
            {
                try { Directory.Delete(root, true); } catch (IOException) { } catch (UnauthorizedAccessException) { }
            }
        }

        private static void HandUpdateRestoresUnreadable()
        {
            using (var box = new Sandbox())
            {
                var library = Path.Combine(box.Roaming, @"Recall\library.json");
                var folders = Path.Combine(box.Roaming, "Recall", "folders.json");
                Directory.CreateDirectory(Path.GetDirectoryName(library));
                const string names = "{\"clip-a.mp4\":{\"name\":\"ace\",\"favorite\":true}}";
                File.WriteAllText(library, names);
                File.WriteAllText(folders, "{\"recordings\":\"D-Clips\"}");
                // After the swap, before the app runs: one file damaged, one changed but readable.
                Installer.AfterSwap = () =>
                {
                    File.WriteAllBytes(library, new byte[] { 0, 0, 0 });
                    File.WriteAllText(folders, "{\"recordings\":\"E-New\"}");
                };
                string error;
                try { error = box.Run(null); }
                finally { Installer.AfterSwap = null; }
                Check(error == null, "the update ran: " + error);
                Check(File.ReadAllText(library) == names, "the damaged library (names, favourites) came back from the backup");
                Check(File.ReadAllText(folders).Contains("E-New"), "a readable file is never replaced");
                var aside = Directory.GetFiles(Path.GetDirectoryName(library), "library.json.unreadable-*");
                Check(aside.Length == 1, "the damaged file was set aside, not overwritten (" + aside.Length + ")");
                Check(aside.Length == 1 && File.ReadAllBytes(aside[0]).SequenceEqual(new byte[] { 0, 0, 0 }), "the damaged bytes survive in the set-aside file");
            }
        }
    }
}
