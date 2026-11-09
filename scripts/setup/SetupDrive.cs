using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace Recall.Setup
{
    internal enum DriveKind { Ssd, Hdd, Unknown }

    /// The install folder must be on an SSD: the app keeps its cache in &lt;install&gt;\cache, so the
    /// clip hard drive stays cold storage. Asked of the volume's disk with
    /// IOCTL_STORAGE_QUERY_PROPERTY (seek penalty and bus type), what Get-PhysicalDisk reports.
    internal static class InstallDrive
    {
        private const int BusNvme = 17;

        /// Pure: no seek penalty or an NVMe bus is an SSD; a seek penalty is a hard drive.
        public static DriveKind Classify(bool? seekPenalty, int busType)
        {
            if (busType == BusNvme || seekPenalty == false) return DriveKind.Ssd;
            if (seekPenalty == true) return DriveKind.Hdd;
            return DriveKind.Unknown;
        }

        /// Null when the folder may be used, else why not. A drive Windows can't classify (USB,
        /// RAID, Storage Spaces, VM disk) is refused as before (Setup never allowed one).
        public static string Refusal(DriveKind kind, string model, string installDir)
        {
            if (kind == DriveKind.Ssd) return null;
            return "Recall must be installed on an SSD so its cache loads instantly. '" + installDir + "' is on "
                + model + " (" + (kind == DriveKind.Hdd ? "HDD" : "Unknown") + "). Choose a folder on an SSD.";
        }

        public static void Check(string installDir, Action<string> log)
        {
            string model;
            var kind = Query(installDir, out model);
            var why = Refusal(kind, model, installDir);
            if (why != null) throw new InvalidOperationException(why);
            log("Install folder: " + installDir + "  (" + model + ", SSD)");
        }

        public static DriveKind Query(string installDir, out string model)
        {
            model = "?";
            var root = Path.GetPathRoot(Path.GetFullPath(installDir));
            if (root == null || root.Length < 2 || root[1] != ':') return DriveKind.Unknown;
            var handle = Native.CreateFileW(@"\\.\" + root.Substring(0, 2), 0, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
            if (handle == new IntPtr(-1)) return DriveKind.Unknown;
            try
            {
                int bus = -1;
                var device = Property(handle, 0, 1024); // StorageDeviceProperty
                if (device != null && device.Length >= 32)
                {
                    bus = BitConverter.ToInt32(device, 28);
                    model = Text(device, BitConverter.ToInt32(device, 16)) ?? "?";
                }
                bool? penalty = null;
                var seek = Property(handle, 7, 12); // StorageDeviceSeekPenaltyProperty
                if (seek != null && seek.Length >= 9) penalty = seek[8] != 0;
                return Classify(penalty, bus);
            }
            finally { Native.CloseHandle(handle); }
        }

        private static byte[] Property(IntPtr handle, int id, int size)
        {
            var query = new byte[12];
            BitConverter.GetBytes(id).CopyTo(query, 0); // PropertyId; QueryType 0 = PropertyStandardQuery
            var output = new byte[size];
            int returned;
            return Native.DeviceIoControl(handle, 0x2D1400, query, query.Length, output, output.Length, out returned, IntPtr.Zero)
                ? output : null;
        }

        private static string Text(byte[] data, int offset)
        {
            if (offset <= 0 || offset >= data.Length) return null;
            int end = Array.IndexOf(data, (byte)0, offset);
            if (end < 0) end = data.Length;
            var value = Encoding.ASCII.GetString(data, offset, end - offset).Trim();
            return value.Length == 0 ? null : value;
        }

        private static class Native
        {
            [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
            public static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
            [DllImport("kernel32.dll", SetLastError = true)]
            public static extern bool DeviceIoControl(IntPtr device, uint code, byte[] input, int inputSize, byte[] output, int outputSize,
                out int returned, IntPtr overlapped);
            [DllImport("kernel32.dll")]
            public static extern bool CloseHandle(IntPtr h);
        }
    }
}
