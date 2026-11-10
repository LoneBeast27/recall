using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace Recall.Setup
{
    /// Paths compared by what they name, not how they are spelled: a \\?\ prefix is stripped,
    /// junctions, symlinks, subst drives and 8.3 names resolve (GetFinalPathNameByHandleW when the
    /// path opens, else GetLongPathNameW, else the parent folder resolved plus the name). Callers
    /// compare case-insensitively. Same rules as the dev installer's PathId.
    internal static class PathId
    {
        private static readonly Dictionary<string, string> Cache = new Dictionary<string, string>(StringComparer.Ordinal);

        public static string Strip(string p)
        {
            if (p.StartsWith(@"\\?\UNC\", StringComparison.Ordinal)) return @"\\" + p.Substring(8);
            if (p.StartsWith(@"\\?\", StringComparison.Ordinal) || p.StartsWith(@"\??\", StringComparison.Ordinal)) return p.Substring(4);
            return p;
        }

        /// Each spelling is resolved once per run: the waits poll every process several times a second.
        public static string Normalize(string path)
        {
            if (string.IsNullOrEmpty(path)) return path;
            string known;
            lock (Cache)
                if (Cache.TryGetValue(path, out known)) return known;
            var resolved = Resolve(path);
            lock (Cache) Cache[path] = resolved;
            return resolved;
        }

        private static string Resolve(string path)
        {
            string full;
            try { full = Path.GetFullPath(Strip(path)); }
            catch (Exception) { return Strip(path).TrimEnd('\\'); }
            // FILE_READ_ATTRIBUTES, share read/write/delete, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS (folders open too).
            IntPtr file = Native.CreateFileW(full, 0x80, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero);
            if (file != new IntPtr(-1))
            {
                try
                {
                    var buffer = new StringBuilder(1024);
                    uint n = Native.GetFinalPathNameByHandleW(file, buffer, 1024, 0);
                    if (n > 0 && n < 1024) return Strip(buffer.ToString()).TrimEnd('\\');
                }
                finally { Native.CloseHandle(file); }
            }
            var longName = new StringBuilder(1024);
            uint m = Native.GetLongPathNameW(full, longName, 1024);
            if (m > 0 && m < 1024) return longName.ToString().TrimEnd('\\');
            // Not there yet (a first install): resolve the folder that holds it.
            string parent = Path.GetDirectoryName(full);
            if (parent == null) return full.TrimEnd('\\');
            return Resolve(parent).TrimEnd('\\') + "\\" + Path.GetFileName(full);
        }

        /// The folder as a prefix: normalized, ending in '\', so Programs\Recall2 is not inside Programs\Recall.
        public static string Root(string folder)
        {
            return Normalize(folder).TrimEnd('\\') + "\\";
        }

        /// Is <paramref name="path"/> the folder <paramref name="root"/> (from Root) or inside it? Pure on the spellings given.
        public static bool IsInside(string path, string root)
        {
            if (string.IsNullOrEmpty(path) || string.IsNullOrEmpty(root)) return false;
            var p = path.TrimEnd('\\') + "\\";
            return p.StartsWith(root, StringComparison.OrdinalIgnoreCase);
        }

        public static bool Same(string a, string b)
        {
            return a != null && b != null
                && string.Equals(Normalize(a).TrimEnd('\\'), Normalize(b).TrimEnd('\\'), StringComparison.OrdinalIgnoreCase);
        }

        private static class Native
        {
            [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
            public static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
            [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
            public static extern uint GetFinalPathNameByHandleW(IntPtr file, StringBuilder path, uint size, uint flags);
            [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
            public static extern uint GetLongPathNameW(string shortPath, StringBuilder longPath, uint size);
            [DllImport("kernel32.dll")]
            public static extern bool CloseHandle(IntPtr h);
        }
    }

    /// One process of this Windows session: pid, image name, image path (null when unreadable) and
    /// start time (a reused pid is not the same process).
    internal sealed class SessionProcess
    {
        public readonly int Pid;
        public readonly string Name;
        public readonly string Path;
        public readonly long Created;

        public SessionProcess(int pid, string name, string path, long created)
        {
            Pid = pid; Name = name; Path = path; Created = created;
        }

        public override string ToString()
        {
            return Name + " (pid " + Pid + ", " + (Path ?? "path unreadable") + ")";
        }
    }

    internal static class SessionProcesses
    {
        public static int CurrentSession { get { using (var me = Process.GetCurrentProcess()) return me.SessionId; } }

        /// Every process of <paramref name="session"/>, with its image path read through
        /// QueryFullProcessImageNameW (works on this account's elevated processes too).
        public static List<SessionProcess> List(int session)
        {
            var list = new List<SessionProcess>();
            foreach (var p in Process.GetProcesses())
            {
                try
                {
                    if (p.SessionId != session) continue;
                    var path = ImagePath(p.Id);
                    var name = path != null ? System.IO.Path.GetFileName(path) : p.ProcessName + ".exe";
                    list.Add(new SessionProcess(p.Id, name, path, CreationTime(p.Id)));
                }
                catch (InvalidOperationException) { } // exited meanwhile
                finally { p.Dispose(); }
            }
            return list;
        }

        public static string ImagePath(int pid)
        {
            // PROCESS_QUERY_LIMITED_INFORMATION: granted on this account's elevated processes too.
            IntPtr process = Native.OpenProcess(0x1000, false, (uint)pid);
            if (process == IntPtr.Zero) return null;
            try
            {
                var name = new StringBuilder(1024);
                uint size = 1024;
                return Native.QueryFullProcessImageNameW(process, 0, name, ref size) ? name.ToString() : null;
            }
            finally { Native.CloseHandle(process); }
        }

        /// The process start time as a FILETIME, or 0 when it can't be read.
        public static long CreationTime(int pid)
        {
            IntPtr process = Native.OpenProcess(0x1000, false, (uint)pid);
            if (process == IntPtr.Zero) return 0;
            try
            {
                long created, exited, kernel, user;
                return Native.GetProcessTimes(process, out created, out exited, out kernel, out user) ? created : 0;
            }
            finally { Native.CloseHandle(process); }
        }

        /// The process with this pid and start time still runs (a reused pid is not it). An
        /// unreadable start time (0) matches any.
        public static bool Alive(int pid, long created)
        {
            IntPtr process = Native.OpenProcess(0x1000, false, (uint)pid);
            if (process == IntPtr.Zero) return false;
            try
            {
                uint code;
                if (Native.GetExitCodeProcess(process, out code) && code != 259) return false; // STILL_ACTIVE
                if (created == 0) return true;
                long c, e, k, u;
                return !Native.GetProcessTimes(process, out c, out e, out k, out u) || c == created;
            }
            finally { Native.CloseHandle(process); }
        }

        public static string Describe(IEnumerable<SessionProcess> processes)
        {
            var parts = new List<string>();
            foreach (var p in processes) parts.Add(p.ToString());
            return string.Join(", ", parts.ToArray());
        }

        private static class Native
        {
            [DllImport("kernel32.dll", SetLastError = true)]
            public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
            [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
            public static extern bool QueryFullProcessImageNameW(IntPtr process, uint flags, StringBuilder name, ref uint size);
            [DllImport("kernel32.dll", SetLastError = true)]
            public static extern bool GetProcessTimes(IntPtr process, out long created, out long exited, out long kernel, out long user);
            [DllImport("kernel32.dll", SetLastError = true)]
            public static extern bool GetExitCodeProcess(IntPtr process, out uint code);
            [DllImport("kernel32.dll")]
            public static extern bool CloseHandle(IntPtr h);
        }
    }
}
