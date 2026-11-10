using System;
using System.Collections.Generic;
using System.IO;

namespace Recall.Setup.Tests
{
    internal static partial class TestRunner
    {
        private static ManifestEntry Entry(string path) { return new ManifestEntry { Path = path, Size = 1, Sha256 = "AA" }; }

        private static void UninstallOnlyManifest()
        {
            const string root = @"Y:\S\Programs\Recall\";
            var manifest = new List<ManifestEntry> { Entry("recall.exe"), Entry("changed.dll"), Entry("RecallSetup.exe") };
            var files = UninstallPlan.ProgramFiles(root, manifest, new string[0], e => e.Path != "changed.dll");
            Check(files.Count == 2 && files.Contains(root + "recall.exe") && files.Contains(root + "RecallSetup.exe"), "listed and unchanged only: " + string.Join(",", files.ToArray()));
            Check(!files.Contains(root + "cache"), "never the cache folder");
            Check(!files.Exists(f => f.TrimEnd('\\') + "\\" == root), "never the install folder itself");
        }

        private static void UninstallNeverOutside()
        {
            const string root = @"Y:\S\Programs\Recall\";
            var evil = new List<ManifestEntry> {
                Entry(@"..\Other\x.dll"), Entry(@"C:\Windows\x.dll"), Entry(@"\Windows\x.dll"), Entry(@"sub\..\..\x.dll"),
                Entry("clips\\game.mp4"), Entry(InstallManifest.Name), Entry("ok.dll"), Entry("a/b.dll"), Entry("*.dll")
            };
            var protectedRoots = new[] { root + @"clips\" };
            var files = UninstallPlan.ProgramFiles(root, evil, protectedRoots, e => true);
            Check(files.Count == 1 && files[0] == root + "ok.dll", "only ok.dll: " + string.Join(",", files.ToArray()));
            Check(!UninstallPlan.Allowed(@"Y:\S\Programs\Recall2\x.dll", root, new string[0]), "a sibling folder is outside");
            Check(!UninstallPlan.Allowed(root + "cache", root, new[] { root + @"cache\clips\" }), "a folder holding a clip folder is kept");
            Check(!UninstallPlan.Allowed(root, root, new string[0]), "the folder itself is never a target");
            foreach (var f in files) Check(f.StartsWith(root, StringComparison.OrdinalIgnoreCase), "inside: " + f);
        }

        private static void CacheClassification()
        {
            const string id = "0123456789abcdef";
            foreach (var r in new[] { @"thumbs\" + id + ".jpg", @"visuals\" + id + @"\scrub-1.jpg", @"visuals\" + id + @"\strip.json",
                @"editor\" + id + @"\clip.json", @"editor\" + id + @"\a.flac", @"proxy\" + id + ".mp4" })
                Check(CachePlan.Classify(r) == CacheFileKind.Regenerable, "regenerable: " + r);
            foreach (var r in new[] { @"staging\kept\VALORANT 2026-11-10.mp4", @"video\" + id + ".mp4", @"light\" + id + ".mp4", "owned.json",
                "interactions.json", @"projects\p.json", @"thumbs\holiday.jpg", @"visuals\" + id + @"\movie.mp4", @"editor\" + id + @"\clip.mp4",
                @"heads\" + id + ".head", @"tmp\pre-encode\" + id + ".mp4", @"proxy\ABCDEF0123456789.mp4", @"thumbs\" + id + @".jpg\x" })
                Check(CachePlan.Classify(r) == CacheFileKind.Keep, "never moved or deleted: " + r);
        }

        private static void KeptNoteCounts()
        {
            var note = CachePlan.KeptNote(new[] { @"staging\kept\a.mp4", @"video\0123456789abcdef.mp4", "owned.json" }, @"Y:\R\cache");
            Check(note == @"2 clip copies kept in Y:\R\cache, with 1 other file Recall can't recreate.", note);
            Check(CachePlan.KeptNote(new[] { @"staging\kept\a.MKV" }, "C") == "1 clip copy kept in C.", "one copy");
            Check(CachePlan.KeptNote(new string[0], "C") == null, "nothing kept, no note");
        }

        private static void TempCopyFolder()
        {
            const string temp = @"C:\Users\u\AppData\Local\Temp\";
            const string ours = temp + "Recall-Uninstall-0123456789abcdef0123456789abcdef";
            Check(TempCopy.IsOurFolder(ours, temp), "its own folder");
            Check(!TempCopy.IsOurFolder(@"Y:\Programs\Recall", temp), "never the install folder");
            Check(!TempCopy.IsOurFolder(temp.TrimEnd('\\'), temp), "never %TEMP% itself");
            Check(!TempCopy.IsOurFolder(temp + "Recall-Uninstall-x", temp), "exact name only");
            Check(!TempCopy.IsOurFolder(temp + @"sub\Recall-Uninstall-0123456789abcdef0123456789abcdef", temp), "directly in %TEMP% only");
        }

        private static void TempCopySweep()
        {
            var root = Path.Combine(Path.GetTempPath(), "recall-setup-sweep-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(root);
            try
            {
                var ours = Path.Combine(root, "Recall-Uninstall-0123456789abcdef0123456789abcdef");
                Directory.CreateDirectory(ours);
                File.WriteAllText(Path.Combine(ours, "RecallUninstall.exe"), "x");
                var near = Path.Combine(root, "Recall-Uninstall-not-ours");
                Directory.CreateDirectory(near);
                var nested = Path.Combine(root, "Recall-Uninstall-11111111111111111111111111111111");
                Directory.CreateDirectory(Path.Combine(nested, "sub"));
                var other = Path.Combine(root, "Other");
                Directory.CreateDirectory(other);
                var target = Path.Combine(root, "target");
                Directory.CreateDirectory(target);
                File.WriteAllText(Path.Combine(target, "keep.txt"), "keep");
                var link = Path.Combine(root, "Recall-Uninstall-22222222222222222222222222222222");
                // Unprivileged directory symlink (needs Developer Mode); without it the link case is reported skipped.
                var linked = CreateSymbolicLinkW(link, target, 0x1 | 0x2);
                var removed = TempCopy.Sweep(root, line => { });
                Check(!Directory.Exists(ours), "the matching copy is removed");
                Check(Directory.Exists(near) && Directory.Exists(other), "non-matching folders kept");
                Check(Directory.Exists(Path.Combine(nested, "sub")), "a matching folder with a subfolder is kept");
                Check(File.Exists(Path.Combine(target, "keep.txt")), "a link's target is never touched");
                Check(removed == 1, "exactly one removed (" + removed + ")");
                if (linked) Check(Directory.Exists(link), "a reparse point named like ours is kept");
                else Console.WriteLine("  (skipped: reparse-point case, symlink creation not permitted)");
            }
            finally
            {
                foreach (var dir in Directory.GetDirectories(root))
                    if ((File.GetAttributes(dir) & FileAttributes.ReparsePoint) != 0) Directory.Delete(dir, false);
                Directory.Delete(root, true);
            }
        }

        [System.Runtime.InteropServices.DllImport("kernel32.dll", CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
        [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.I1)]
        private static extern bool CreateSymbolicLinkW(string link, string target, int flags);

        private static void ManifestMerge()
        {
            Check(InstallManifest.IsSafeRelative("recall.exe") && InstallManifest.IsSafeRelative(@"sub\a.dll"), "plain names");
            foreach (var bad in new[] { "", " a", @"..\a", @"a\..\b", @"C:\a", @"\a", "a/b", "a:b", InstallManifest.Name, @"a\" })
                Check(!InstallManifest.IsSafeRelative(bad), "unsafe: " + bad);
            var now = new List<ManifestEntry> { Entry("recall.exe") };
            var before = new List<ManifestEntry> { Entry("recall.exe"), Entry("old.dll"), Entry("gone.dll"), Entry(@"..\x") };
            var merged = InstallManifest.Merge(now, before, e => e.Path != "gone.dll");
            var names = merged.ConvertAll(e => e.Path);
            Check(names.Count == 2 && names.Contains("recall.exe") && names.Contains("old.dll"), string.Join(",", names.ToArray()));
        }

        private static void RunValueAndFolders()
        {
            const string root = @"Y:\S\Programs\Recall\";
            Check(RunValue.PointsInto("\"Y:\\S\\Programs\\Recall\\recall-hdr.exe\" --record", root), "quoted, with --record");
            Check(!RunValue.PointsInto("\"C:\\Other\\recall-hdr.exe\" --record", root), "another folder");
            Check(!RunValue.PointsInto("\"Y:\\S\\Programs\\Recall2\\recall-hdr.exe\"", root), "a sibling folder");
            var json = ClipFolders.FromFoldersJson("{\"recordings\":\"D:\\\\Clips\",\"imported\":[\"E:\\\\Old\"],\"former_recordings\":[\"F:\\\\Was\"]}");
            Check(json.Count == 3 && json.Contains(@"D:\Clips") && json.Contains(@"E:\Old") && json.Contains(@"F:\Was"), string.Join(",", json.ToArray()));
            var toml = ClipFolders.FromConfigToml("[recording]\noutput_dir = \"D:\\\\Videos\\\\Recall\"\n[x]\noutput_dir = 'E:\\Raw'\n");
            Check(toml.Count == 2 && toml.Contains(@"D:\Videos\Recall") && toml.Contains(@"E:\Raw"), string.Join(",", toml.ToArray()));
        }

        private static void DriveKinds()
        {
            Check(InstallDrive.Classify(false, 11) == DriveKind.Ssd, "no seek penalty");
            Check(InstallDrive.Classify(null, 17) == DriveKind.Ssd, "NVMe");
            Check(InstallDrive.Classify(true, 11) == DriveKind.Hdd, "seek penalty");
            Check(InstallDrive.Classify(null, 7) == DriveKind.Unknown, "unknown");
            Check(InstallDrive.Refusal(DriveKind.Unknown, "USB", @"E:\R", false) != null && InstallDrive.Refusal(DriveKind.Hdd, "WD", @"E:\R", false) != null, "refused");
            Check(InstallDrive.Refusal(DriveKind.Unknown, "Msft Virtual Disk", @"C:\R", true) == null, "an unclassified drive only when the caller opts in");
            Check(InstallDrive.Refusal(DriveKind.Hdd, "WD", @"E:\R", true) != null, "a hard drive is refused even then");
            Check(!new InstallOptions(@"C:\R", true, false, false, false).AllowUnknownDrive, "the window never opts in");
        }

        private static void SwapRollback()
        {
            var root = Path.Combine(Path.GetTempPath(), "recall-setup-swap-test-" + Guid.NewGuid().ToString("N"));
            var inst = Path.Combine(root, "inst");
            var stage = Path.Combine(root, "stage");
            try
            {
                Action reset = () =>
                {
                    if (Directory.Exists(root)) Directory.Delete(root, true);
                    Directory.CreateDirectory(inst);
                    Directory.CreateDirectory(stage);
                    foreach (var n in new[] { "a-native.exe", "b-av.dll", "c-recall.exe" })
                    {
                        File.WriteAllText(Path.Combine(inst, n), "old " + n);
                        File.WriteAllText(Path.Combine(stage, n), "new " + n);
                    }
                    File.WriteAllText(Path.Combine(stage, "d-added.dll"), "new d-added.dll");
                };
                Func<string> state = () =>
                {
                    var parts = new List<string>();
                    var files = Directory.GetFiles(inst);
                    Array.Sort(files, StringComparer.OrdinalIgnoreCase);
                    foreach (var f in files) parts.Add(Path.GetFileName(f) + "=" + File.ReadAllText(f));
                    return string.Join("; ", parts.ToArray());
                };
                reset();
                InstallSwap.Run(stage, inst);
                Check(state() == "a-native.exe=new a-native.exe; b-av.dll=new b-av.dll; c-recall.exe=new c-recall.exe; d-added.dll=new d-added.dll", "clean swap: " + state());
                Check(Directory.GetDirectories(inst).Length == 0, "no .new/.old litter");
                reset();
                string error = null;
                using (new FileStream(Path.Combine(inst, "c-recall.exe"), FileMode.Open, FileAccess.Read, FileShare.None))
                {
                    try { InstallSwap.Run(stage, inst); }
                    catch (InvalidOperationException e) { error = e.Message; }
                }
                Check(error != null && error.Contains("rolled back"), "says it rolled back: " + error);
                Check(state() == "a-native.exe=old a-native.exe; b-av.dll=old b-av.dll; c-recall.exe=old c-recall.exe", "all old again: " + state());
                Check(!File.Exists(Path.Combine(inst, ".installing")), "marker removed");
            }
            finally { if (Directory.Exists(root)) Directory.Delete(root, true); }
        }

        private static void NoPowerShell()
        {
            Check(sourceDir != null && Directory.Exists(sourceDir), "build.ps1 -Test passes the source folder");
            // No PowerShell, and no code that could send a clip anywhere (the maintainer: never a permanent delete).
            var banned = new[] { "powershell", "pwsh", ".ps1", "ExecutionPolicy", "EncodedCommand", "SHFileOperation", "FOF_", "IFileOperation", "recycle" };
            int files = 0;
            foreach (var file in Directory.GetFiles(sourceDir, "*.cs"))
            {
                files++;
                var text = File.ReadAllText(file);
                foreach (var word in banned)
                    Check(text.IndexOf(word, StringComparison.OrdinalIgnoreCase) < 0, Path.GetFileName(file) + " mentions " + word);
            }
            Check(files >= 10, "read the shipped sources (" + files + ")");
        }
    }
}
