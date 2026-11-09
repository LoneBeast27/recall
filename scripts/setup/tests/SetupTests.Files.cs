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
            const string root = @"L:\S\Programs\Recall\";
            var manifest = new List<ManifestEntry> { Entry("recall.exe"), Entry("changed.dll"), Entry("RecallSetup.exe") };
            var files = UninstallPlan.ProgramFiles(root, manifest, new string[0], e => e.Path != "changed.dll");
            Check(files.Count == 2 && files.Contains(root + "recall.exe") && files.Contains(root + "RecallSetup.exe"), "listed and unchanged only: " + string.Join(",", files.ToArray()));
            Check(!files.Contains(root + "cache"), "never the cache folder");
            Check(!files.Exists(f => f.TrimEnd('\\') + "\\" == root), "never the install folder itself");
        }

        private static void UninstallNeverOutside()
        {
            const string root = @"L:\S\Programs\Recall\";
            var evil = new List<ManifestEntry> {
                Entry(@"..\Other\x.dll"), Entry(@"C:\Windows\x.dll"), Entry(@"\Windows\x.dll"), Entry(@"sub\..\..\x.dll"),
                Entry("clips\\game.mp4"), Entry(InstallManifest.Name), Entry("ok.dll"), Entry("a/b.dll"), Entry("*.dll")
            };
            var protectedRoots = new[] { root + @"clips\" };
            var files = UninstallPlan.ProgramFiles(root, evil, protectedRoots, e => true);
            Check(files.Count == 1 && files[0] == root + "ok.dll", "only ok.dll: " + string.Join(",", files.ToArray()));
            Check(!UninstallPlan.Allowed(@"L:\S\Programs\Recall2\x.dll", root, new string[0]), "a sibling folder is outside");
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
                Check(CachePlan.Classify(r) == CacheFileKind.Recycle, "never deleted: " + r);
        }

        private static BinFacts GoodBin()
        {
            return new BinFacts { VolumeId = @"\\?\Volume{1234}\", Label = "L:", DriveType = 3, TotalBytes = 1L << 40, BinUsed = 0 };
        }

        private static void RecycleDecide()
        {
            Check(RecycleBin.Decide(GoodBin(), 1 << 20) == null, "a fixed drive with room: OK");
            var f = GoodBin(); f.Subst = true; Check(RecycleBin.Decide(f, 1) != null, "subst refused");
            f = GoodBin(); f.ResolvedUnc = true; Check(RecycleBin.Decide(f, 1) != null, "share refused");
            f = GoodBin(); f.DriveType = 2; Check(RecycleBin.Decide(f, 1) != null, "removable refused");
            f = GoodBin(); f.Nuke = 1; Check(RecycleBin.Decide(f, 1) != null, "bin off refused");
            f = GoodBin(); f.PolicyOff = true; Check(RecycleBin.Decide(f, 1) != null, "policy off refused");
            f = GoodBin(); f.BinUsed = null; Check(RecycleBin.Decide(f, 1) != null, "unreadable bin refused");
            f = GoodBin(); f.VolumeId = null; Check(RecycleBin.Decide(f, 1) != null, "no volume refused");
            f = GoodBin(); f.MaxMb = 1000; f.BinUsed = 950L << 20; Check(RecycleBin.Decide(f, 1 << 20) != null, "no room with the margin: refused");
            f = GoodBin(); f.MaxMb = 1000; Check(RecycleBin.Decide(f, 2000L << 20) != null, "bigger than the bin: refused");
            Check(RecycleBin.DefaultMaxMb(40L * 1024 * (1 << 20)) == 4096, "Windows' default size");
        }

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
            const string root = @"L:\S\Programs\Recall\";
            Check(RunValue.PointsInto("\"L:\\S\\Programs\\Recall\\recall-hdr.exe\" --record", root), "quoted, with --record");
            Check(!RunValue.PointsInto("\"C:\\Other\\recall-hdr.exe\" --record", root), "another folder");
            Check(!RunValue.PointsInto("\"L:\\S\\Programs\\Recall2\\recall-hdr.exe\"", root), "a sibling folder");
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
            Check(InstallDrive.Refusal(DriveKind.Unknown, "USB", @"E:\R") != null && InstallDrive.Refusal(DriveKind.Hdd, "WD", @"E:\R") != null, "refused");
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
            var banned = new[] { "powershell", "pwsh", ".ps1", "ExecutionPolicy", "EncodedCommand" };
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
