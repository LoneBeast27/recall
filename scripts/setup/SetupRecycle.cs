using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32;

namespace Recall.Setup
{
    /// What Windows says about one volume's Recycle Bin (the app's win_recycle.rs BinFacts).
    internal sealed class BinFacts
    {
        public bool Subst;
        public bool ResolvedUnc;
        public string VolumeId;
        public string Label = "the drive";
        public uint DriveType;
        public long? TotalBytes;
        public long? BinUsed;
        public uint? Nuke;
        public uint? MaxMb;
        public bool PolicyOff;
        public uint? PolicyPercent;
    }

    /// Can these files really go to the Recycle Bin? Same rules as the app's win_recycle.rs: a
    /// local fixed volume with a volume GUID (not subst, not a share), the bin not turned off (the
    /// NoRecycleFiles policy or the volume's NukeOnDelete), and the bytes fit the bin's FREE room
    /// with a margin of min(1 GiB, 10% of the bin). A refusal moves nothing: the files stay.
    internal static class RecycleBin
    {
        private const long Mib = 1L << 20;
        private const string PolicyKey = @"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
        /// Sandbox only: makes the precheck refuse, to prove a refusal deletes nothing.
        public const string RefuseVariable = "RECALL_SETUP_SANDBOX_REFUSE_BIN";

        public static long DefaultMaxMb(long totalBytes)
        {
            long mib = totalBytes / Mib;
            return mib <= 40 * 1024 ? mib / 10 : 4096 + (mib - 40 * 1024) / 20;
        }

        public static long Margin(long max) { return Math.Min(max / 10, 1L << 30); }

        /// Null when <paramref name="bytes"/> may go to this bin, else why not.
        public static string Decide(BinFacts f, long bytes)
        {
            if (f.Subst) return "the folder is on a subst drive, which has no Recycle Bin";
            if (f.ResolvedUnc) return "the folder is on a network share, which has no Recycle Bin";
            if (f.VolumeId == null || !f.VolumeId.StartsWith(@"\\?\Volume{", StringComparison.Ordinal))
                return "the drive has no Recycle Bin (a network share or an unknown drive)";
            if (f.DriveType != 3) return f.Label + " has no Recycle Bin (a removable or network drive)";
            if (f.PolicyOff) return "the Recycle Bin is turned off by a policy (files are deleted right away)";
            if (f.Nuke == 1) return "the Recycle Bin is turned off for " + f.Label + " (files are deleted right away)";
            long? max = f.PolicyPercent.HasValue ? (f.TotalBytes.HasValue ? f.TotalBytes / 100 * Math.Min(100, f.PolicyPercent.Value) : null)
                : f.MaxMb.HasValue ? (long?)(f.MaxMb.Value * Mib)
                : f.TotalBytes.HasValue ? (long?)(DefaultMaxMb(f.TotalBytes.Value) * Mib) : null;
            if (!max.HasValue) return "the Recycle Bin size for " + f.Label + " is unknown";
            if (bytes > max.Value) return "the clip is bigger than the Recycle Bin on " + f.Label + " (" + max.Value / Mib + " MB)";
            if (!f.BinUsed.HasValue) return "couldn't read how full the Recycle Bin on " + f.Label + " is";
            long room = Math.Max(0, max.Value - f.BinUsed.Value);
            long spare = Margin(max.Value);
            if (bytes + spare > room)
                return "not enough room left in the Recycle Bin on " + f.Label + ": " + CeilMib(bytes) + " MB needs " + CeilMib(bytes + spare)
                    + " MB free (" + spare / Mib + " MB spare, so Windows never deletes anything for good), and it has " + room / Mib
                    + " MB free of " + max.Value / Mib + " MB; empty the Recycle Bin or make it bigger";
            return null;
        }

        private static long CeilMib(long bytes) { return (bytes + Mib - 1) / Mib; }

        /// A drive letter that is a `subst` onto a folder (its DOS device target is a `\??\` path).
        public static bool IsSubst(string path, Func<char, string> dosTarget)
        {
            if (path == null || path.Length < 2 || path[1] != ':' || !char.IsLetter(path[0])) return false;
            var target = dosTarget(char.ToUpperInvariant(path[0]));
            return target != null && target.StartsWith(@"\??\", StringComparison.Ordinal);
        }

        /// The volume's bin settings: NukeOnDelete and MaxCapacity from HKCU BitBucket\Volume\{GUID};
        /// the NoRecycleFiles and RecycleBinSize policies from HKLM, else HKCU (a machine policy wins).
        /// <paramref name="read"/>(machine, key, value) reads one DWORD (tests inject one).
        public static void ReadSettings(BinFacts f, string guid, Func<bool, string, string, uint?> read)
        {
            var key = @"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\" + guid;
            f.Nuke = read(false, key, "NukeOnDelete");
            f.MaxMb = read(false, key, "MaxCapacity");
            f.PolicyOff = (read(true, PolicyKey, "NoRecycleFiles") ?? read(false, PolicyKey, "NoRecycleFiles")) == 1;
            f.PolicyPercent = read(true, PolicyKey, "RecycleBinSize") ?? read(false, PolicyKey, "RecycleBinSize");
        }

        private static string DosTarget(char letter)
        {
            var target = new StringBuilder(1024);
            return Native.QueryDosDeviceW(letter + ":", target, 1024) > 0 ? target.ToString() : null;
        }

        public static string Check(string folder, long bytes)
        {
            if (Environment.GetEnvironmentVariable(SetupTargets.SandboxVariable) != null
                && Environment.GetEnvironmentVariable(RefuseVariable) == "1")
                return "the Recycle Bin check was set to refuse (sandbox test)";
            return Decide(Facts(folder), bytes);
        }

        public static BinFacts Facts(string folder)
        {
            var f = new BinFacts();
            var full = Path.GetFullPath(folder);
            if (IsSubst(full, DosTarget)) { f.Subst = true; return f; }
            if (PathId.Normalize(full).StartsWith(@"\\", StringComparison.Ordinal)) { f.ResolvedUnc = true; return f; }
            var mount = new StringBuilder(1024);
            if (!Native.GetVolumePathNameW(full, mount, 1024)) return f;
            var root = mount.ToString();
            f.Label = root.TrimEnd('\\');
            var volume = new StringBuilder(1024);
            if (Native.GetVolumeNameForVolumeMountPointW(root, volume, 1024)) f.VolumeId = volume.ToString();
            f.DriveType = Native.GetDriveTypeW(root);
            ulong free, total, totalFree;
            if (Native.GetDiskFreeSpaceExW(root, out free, out total, out totalFree)) f.TotalBytes = (long)total;
            var info = new Native.QueryRbInfo { Size = Marshal.SizeOf(typeof(Native.QueryRbInfo)) };
            if (Native.SHQueryRecycleBinW(root, ref info) == 0) f.BinUsed = info.Bytes;
            var guid = f.VolumeId == null ? "" : f.VolumeId.Substring(@"\\?\Volume".Length).TrimEnd('\\');
            ReadSettings(f, guid, (machine, key, value) => Dword(machine ? Registry.LocalMachine : Registry.CurrentUser, key, value));
            return f;
        }

        private static uint? Dword(RegistryKey hive, string key, string name)
        {
            using (var k = hive.OpenSubKey(key))
            {
                var value = k == null ? null : k.GetValue(name);
                if (value is int) return unchecked((uint)(int)value);
                return null;
            }
        }

        /// Moves the files to the Recycle Bin in one shell call (FOF_ALLOWUNDO). FOF_WANTNUKEWARNING
        /// is the second guard: Windows asks instead of deleting anything for good. Null on success.
        public static string Move(IList<string> files)
        {
            if (files.Count == 0) return null;
            var from = new StringBuilder();
            foreach (var f in files) from.Append(f).Append('\0');
            from.Append('\0');
            var op = new Native.FileOp {
                Func = 3, // FO_DELETE
                From = from.ToString(),
                // FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING
                Flags = 0x0040 | 0x0010 | 0x0004 | 0x0400 | 0x4000
            };
            int code = Native.SHFileOperationW(ref op);
            if (code != 0) return "the Recycle Bin move failed (code " + code + ")";
            return op.Aborted ? "the Recycle Bin move was cancelled" : null;
        }

        private static class Native
        {
            [StructLayout(LayoutKind.Sequential)]
            public struct QueryRbInfo { public int Size; public long Bytes; public long Items; }
            [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
            public struct FileOp
            {
                public IntPtr Window; public uint Func; public string From; public string To; public ushort Flags;
                [MarshalAs(UnmanagedType.Bool)] public bool Aborted; public IntPtr NameMappings; public string Title;
            }
            [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
            public static extern uint QueryDosDeviceW(string device, StringBuilder target, int max);
            [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
            public static extern bool GetVolumePathNameW(string path, StringBuilder mount, int size);
            [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
            public static extern bool GetVolumeNameForVolumeMountPointW(string mount, StringBuilder name, int size);
            [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
            public static extern uint GetDriveTypeW(string root);
            [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
            public static extern bool GetDiskFreeSpaceExW(string root, out ulong free, out ulong total, out ulong totalFree);
            [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
            public static extern int SHQueryRecycleBinW(string root, ref QueryRbInfo info);
            [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
            public static extern int SHFileOperationW(ref FileOp op);
        }
    }
}
