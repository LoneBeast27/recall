using System;
using System.Collections.Generic;

namespace Recall.Setup.Tests
{
    /// The app's Recycle Bin precheck table (recall/src/win_recycle_tests.rs), case for case with the
    /// same numbers, run against Setup's C# port so both sides pin the same rules.
    internal static partial class TestRunner
    {
        private const long Gib = 1L << 30;

        private static BinFacts Local(uint? maxMb, long? used)
        {
            return new BinFacts {
                VolumeId = @"\\?\Volume{6b9a6f8e-84ea-42bb-8615-8aea7bdcfa86}\", Label = "C:", DriveType = 3,
                TotalBytes = 1024 * Gib, BinUsed = used, Nuke = maxMb.HasValue ? (uint?)0 : null, MaxMb = maxMb
            };
        }

        private static void Refuses(BinFacts f, long bytes, string word, string what)
        {
            var why = RecycleBin.Decide(f, bytes);
            Check(why != null && why.Contains(word), what + ": " + (why ?? "accepted"));
        }

        private static void Accepts(BinFacts f, long bytes, string what)
        {
            var why = RecycleBin.Decide(f, bytes);
            Check(why == null, what + ": " + why);
        }

        private static void RecycleTable()
        {
            // only_a_local_enabled_bin_big_enough_takes_a_clip
            Accepts(Local(3000, 0), 1000L << 20, "1,000 MB into a 3,000 MB bin");
            Refuses(Local(1000, 0), (1000L << 20) + 1, "bigger than the Recycle Bin", "too big");
            var share = Local(1000, 0); share.VolumeId = null; share.Label = @"\\nas\clips";
            Refuses(share, 1, "no Recycle Bin", "a share");
            var unknown = Local(1000, 0); unknown.VolumeId = null;
            Check(RecycleBin.Decide(unknown, 1) != null, "unknown drive");
            var removable = Local(1000, 0); removable.DriveType = 2; Refuses(removable, 1, "removable", "removable");
            var remote = Local(1000, 0); remote.DriveType = 4; Refuses(remote, 1, "network", "DRIVE_REMOTE");
            var subst = Local(1000, 0); subst.Subst = true; Refuses(subst, 1, "subst", "subst");
            var unc = Local(1000, 0); unc.ResolvedUnc = true; Refuses(unc, 1, "network share", "resolves to UNC");
            var nuke = Local(1000, 0); nuke.Nuke = 1; Refuses(nuke, 1, "turned off", "NukeOnDelete");
            // windows_default_size_rule
            Check(RecycleBin.DefaultMaxMb(32 * Gib) == 32 * 1024 / 10, "up to 40 GB: 10%");
            Check(RecycleBin.DefaultMaxMb(40 * Gib) == 4096, "40 GB");
            Check(RecycleBin.DefaultMaxMb(3815429L << 20) == 192819, "4 TB disk");
            Check(RecycleBin.DefaultMaxMb(1907727L << 20) == 97434, "2 TB disk");
            // a_volume_without_a_bin_key_uses_windows_defaults
            var fresh = Local(null, 0);
            Accepts(fresh, 10 * Gib, "bin on, default size");
            Refuses(fresh, 60 * Gib, "54476 MB", "default size named");
            var noSize = Local(null, 0); noSize.TotalBytes = null;
            Refuses(noSize, 1, "unknown", "no size, no move");
            // the_no_recycle_files_policy_refuses
            var off = Local(100000, 0); off.PolicyOff = true; Refuses(off, 1, "policy", "NoRecycleFiles");
            // a_size_policy_wins_over_the_volume_setting
            var pct = Local(100000, 0); pct.PolicyPercent = 1;
            Accepts(pct, 9 * Gib, "9 GiB + the 1 GiB margin fit 10.24 GiB");
            Refuses(pct, 20 * Gib, "bigger than the Recycle Bin", "the policy size wins");
            // a_clip_must_fit_the_bins_free_room_not_just_its_size
            var full = Local(100 * 1024, 90 * Gib);
            Refuses(full, 9 * Gib + Gib / 2, "room", "too full");
            Accepts(full, 9 * Gib, "fits the free room");
            Refuses(Local(1000, null), 1, "how full", "an unreadable bin refuses");
            // a_clip_needs_a_margin_of_room_left_in_the_bin
            var half = Local(100 * 1024, 50 * Gib);
            Refuses(half, 50 * Gib - 1024, "room", "room minus 1 KiB is too tight");
            Check(RecycleBin.Decide(half, 49 * Gib + 1) != null, "a 100 GiB bin keeps 1 GiB spare");
            Accepts(half, 49 * Gib, "room minus the margin fits");
            var small = Local(5 * 1024, 0);
            Check(RecycleBin.Decide(small, 4 * Gib + Gib / 2 + 1) != null, "5 GiB bin: 512 MiB spare");
            Accepts(small, 4 * Gib + Gib / 2, "5 GiB bin fits up to the margin");
            // a_small_bin_still_takes_a_clip_and_a_refusal_names_the_spare_room
            Accepts(Local(1000, 0), 100L << 20, "a small bin is not refused forever");
            Check(RecycleBin.Decide(Local(1000, 0), (900L << 20) + 1) != null, "100 MB stays spare");
            var why = RecycleBin.Decide(Local(100 * 1024, 99 * Gib), 500L << 20) ?? "";
            Check(why.Contains("500 MB") && why.Contains("1024 MB spare") && why.Contains("1024 MB free"), why);
        }

        private static void TempCopyFolder()
        {
            const string temp = @"C:\Users\u\AppData\Local\Temp\";
            const string ours = temp + "Recall-Uninstall-0123456789abcdef0123456789abcdef";
            Check(TempCopy.IsOurFolder(ours, temp), "its own folder");
            Check(!TempCopy.IsOurFolder(@"L:\Programs\Recall", temp), "never the install folder");
            Check(!TempCopy.IsOurFolder(temp.TrimEnd('\\'), temp), "never %TEMP% itself");
            Check(!TempCopy.IsOurFolder(temp + "Recall-Uninstall-x", temp), "exact name only");
            Check(!TempCopy.IsOurFolder(temp + @"sub\Recall-Uninstall-0123456789abcdef0123456789abcdef", temp), "directly in %TEMP% only");
            var args = TempCopy.CleanupArguments(ours);
            Check(args.Contains("rmdir /s /q \"" + ours + "\"") && args.Contains("(1,1,60)") && !args.ToLowerInvariant().Contains("powershell"), args);
        }

        private static void RecycleSettingsAndSubst()
        {
            // settings_read_the_volume_key_and_both_policy_hives
            const string guid = "{6b9a6f8e-84ea-42bb-8615-8aea7bdcfa86}";
            Func<bool, string, string, uint?> both = (machine, key, value) =>
            {
                if (!machine && key.EndsWith(@"BitBucket\Volume\" + guid) && value == "NukeOnDelete") return 0;
                if (!machine && key.EndsWith(@"BitBucket\Volume\" + guid) && value == "MaxCapacity") return 49703;
                if (key.EndsWith(@"Policies\Explorer") && value == "NoRecycleFiles") return machine ? (uint?)1 : 0;
                if (key.EndsWith(@"Policies\Explorer") && value == "RecycleBinSize") return machine ? (uint?)5 : 10;
                return null;
            };
            var s = new BinFacts();
            RecycleBin.ReadSettings(s, guid, both);
            Check(s.Nuke == 0 && s.MaxMb == 49703 && s.PolicyOff && s.PolicyPercent == 5, "HKLM wins");
            Func<bool, string, string, uint?> userOnly = (machine, key, value) => machine ? null : both(false, key, value);
            s = new BinFacts();
            RecycleBin.ReadSettings(s, guid, userOnly);
            Check(!s.PolicyOff && s.PolicyPercent == 10, "HKCU applies without a machine policy");
            s = new BinFacts();
            RecycleBin.ReadSettings(s, guid, (m, k, v) => null);
            Check(s.Nuke == null && s.MaxMb == null && !s.PolicyOff && s.PolicyPercent == null, "no key, no policy: Windows defaults");
            // a_subst_drive_is_refused_before_it_is_resolved
            Func<char, string> substOfL = letter => letter == 'Q' ? @"\??\L:\clips" : @"\Device\HarddiskVolume3";
            Func<char, string> realVolume = letter => @"\Device\HarddiskVolume3";
            Check(RecycleBin.IsSubst(@"Q:\Clips", substOfL), "Q: is a subst");
            Check(!RecycleBin.IsSubst(@"C:\Clips", substOfL), "C: is a volume");
            Check(!RecycleBin.IsSubst(@"Q:\Clips", realVolume), "Q: as a real volume");
            Check(!RecycleBin.IsSubst(@"\\nas\clips", substOfL), "no drive letter");
            // this_machine_reports_its_temp_volume_bin (read only)
            var temp = RecycleBin.Facts(System.IO.Path.GetTempPath());
            Check(temp.DriveType == 3 && temp.VolumeId != null && temp.VolumeId.StartsWith(@"\\?\Volume{") && temp.TotalBytes > 0 && temp.BinUsed.HasValue,
                "temp volume facts: type " + temp.DriveType + ", " + temp.VolumeId + ", used " + temp.BinUsed);
        }
    }
}
