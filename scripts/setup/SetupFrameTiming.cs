using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Threading.Tasks;

namespace Recall.Setup
{
    /// Frame-timing stats (FPS and 1% low on the stats island) read the present events Windows
    /// itself logs, which needs the local group Performance Log Users (S-1-5-32-559). Opt-in from
    /// Setup's checkbox: when ticked, Setup starts ITSELF elevated with /add-perflog-user &lt;sid&gt;
    /// through the normal, visible administrator prompt; that copy calls NetLocalGroupAddMembers for
    /// exactly that account and exits. Nothing hidden, nothing encoded, no script host.
    internal static class FrameTiming
    {
        public const string GroupSid = "S-1-5-32-559";
        public const string ElevatedArgument = "/add-perflog-user";
        private const int MemberInAlias = 1378; // ERROR_MEMBER_IN_ALIAS: already a member
        private const int Cancelled = 1223;     // ERROR_CANCELLED: the prompt was declined

        public static bool InToken()
        {
            foreach (var group in WindowsIdentity.GetCurrent().Groups)
                if (group.Value == GroupSid) return true;
            return false;
        }

        /// Only a well-formed account SID is accepted on the elevated command line.
        public static bool IsAccountSid(string value)
        {
            if (string.IsNullOrEmpty(value) || !value.StartsWith("S-1-5-21-", StringComparison.Ordinal)) return false;
            try { return new SecurityIdentifier(value).IsAccountSid(); }
            catch (ArgumentException) { return false; }
        }

        public static string ElevatedArguments(string userSid) { return ElevatedArgument + " " + userSid; }

        /// Setup still has something to offer: neither this sign-in's token nor the group lists the
        /// account. A member waiting for the next sign-in is not asked again.
        public static bool Offer(bool inToken, bool member) { return !inToken && !member; }

        /// <see cref="Offer"/> for this account; false in a sandboxed Setup (tests never prompt).
        public static bool Needed()
        {
            if (Environment.GetEnvironmentVariable(SetupTargets.SandboxVariable) != null) return false;
            try { return Offer(InToken(), IsMember(WindowsIdentity.GetCurrent().User.Value)); }
            catch (Exception) { return false; }
        }

        /// Runs on the UI thread (the prompt belongs to Setup's window); the wait runs off it.
        public static async Task<string> Request(IntPtr owner)
        {
            var me = WindowsIdentity.GetCurrent().User.Value;
            IntPtr process;
            string early = Start(owner, me, out process);
            if (early != null) return early;
            int code = await Task.Run(() => WaitAndClose(process));
            return Outcome(code, me);
        }

        /// <see cref="Request"/> without a window: the "already installed" notice's offer.
        public static string RequestNow()
        {
            var me = WindowsIdentity.GetCurrent().User.Value;
            IntPtr process;
            string early = Start(IntPtr.Zero, me, out process);
            return early ?? Outcome(WaitAndClose(process), me);
        }

        /// A finished answer (nothing to do, or the prompt failed), else null with the elevated copy started.
        private static string Start(IntPtr owner, string me, out IntPtr process)
        {
            process = IntPtr.Zero;
            if (Environment.GetEnvironmentVariable(SetupTargets.SandboxVariable) != null)
                return "Frame timing: skipped in a sandboxed Setup (it would change this PC's Performance Log Users group).";
            if (InToken()) return "Frame timing: you are in Performance Log Users; the stats island shows FPS.";
            if (IsMember(me)) return "Frame timing: you are in Performance Log Users; FPS shows after your next sign-in.";
            int error = StartElevated(owner, ElevatedArguments(me), out process);
            if (error == Cancelled)
                return "Frame timing: the administrator prompt was declined. FPS shows \"-\" until you run Setup again and allow it.";
            if (error != 0) return "Frame timing: Windows could not start the administrator prompt (error " + error + ").";
            return null;
        }

        private static string Outcome(int code, string me)
        {
            if (code == 0 && IsMember(me))
                return "Frame timing: added you to Performance Log Users. FPS shows after your next sign-in.";
            return "Frame timing: adding you to Performance Log Users failed (code " + code + "). FPS shows \"-\" until it is done.";
        }

        /// The elevated copy: adds exactly <paramref name="userSid"/>. 0 = member now (or already).
        public static int AddMember(string userSid)
        {
            if (!IsAccountSid(userSid)) return 87; // ERROR_INVALID_PARAMETER
            var sid = new SecurityIdentifier(userSid);
            var bytes = new byte[sid.BinaryLength];
            sid.GetBinaryForm(bytes, 0);
            IntPtr buffer = Marshal.AllocHGlobal(bytes.Length);
            try
            {
                Marshal.Copy(bytes, 0, buffer, bytes.Length);
                var member = new Native.MembersInfo0 { Sid = buffer };
                int status = Native.NetLocalGroupAddMembers(null, GroupName(), 0, ref member, 1);
                return status == 0 || status == MemberInAlias ? 0 : status;
            }
            finally { Marshal.FreeHGlobal(buffer); }
        }

        /// Already in the group (whether or not this sign-in's token has it yet).
        public static bool IsMember(string userSid)
        {
            IntPtr buffer;
            int read, total;
            IntPtr resume = IntPtr.Zero;
            if (Native.NetLocalGroupGetMembers(null, GroupName(), 0, out buffer, -1, out read, out total, ref resume) != 0) return false;
            try
            {
                int size = Marshal.SizeOf(typeof(Native.MembersInfo0));
                for (int i = 0; i < read; i++)
                {
                    var info = (Native.MembersInfo0)Marshal.PtrToStructure(new IntPtr(buffer.ToInt64() + i * size), typeof(Native.MembersInfo0));
                    if (info.Sid != IntPtr.Zero && new SecurityIdentifier(info.Sid).Value == userSid) return true;
                }
                return false;
            }
            finally { Native.NetApiBufferFree(buffer); }
        }

        /// The group's local name (it is translated on non-English Windows).
        private static string GroupName()
        {
            var account = new SecurityIdentifier(GroupSid).Translate(typeof(NTAccount)).Value;
            int slash = account.IndexOf('\\');
            return slash >= 0 ? account.Substring(slash + 1) : account;
        }

        private static int StartElevated(IntPtr owner, string arguments, out IntPtr process)
        {
            var info = new Native.ShellExecuteInfo {
                Size = Marshal.SizeOf(typeof(Native.ShellExecuteInfo)),
                Mask = 0x40, // SEE_MASK_NOCLOSEPROCESS
                Window = owner, Verb = "runas", File = typeof(FrameTiming).Assembly.Location,
                Parameters = arguments, Show = 1 // SW_SHOWNORMAL
            };
            process = IntPtr.Zero;
            if (!Native.ShellExecuteExW(ref info)) return Marshal.GetLastWin32Error();
            process = info.Process;
            return process == IntPtr.Zero ? 1 : 0;
        }

        private static int WaitAndClose(IntPtr process)
        {
            try
            {
                Native.WaitForSingleObject(process, 5 * 60 * 1000);
                uint code;
                return Native.GetExitCodeProcess(process, out code) ? (int)code : -1;
            }
            finally { Native.CloseHandle(process); }
        }

        private static class Native
        {
            [StructLayout(LayoutKind.Sequential)]
            public struct MembersInfo0 { public IntPtr Sid; }
            [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
            public struct ShellExecuteInfo
            {
                public int Size; public uint Mask; public IntPtr Window; public string Verb, File, Parameters, Directory;
                public int Show; public IntPtr InstApp, IdList; public string Class; public IntPtr ClassKey; public uint HotKey;
                public IntPtr Icon, Process;
            }
            [DllImport("netapi32.dll", CharSet = CharSet.Unicode)]
            public static extern int NetLocalGroupAddMembers(string server, string group, int level, ref MembersInfo0 members, int count);
            [DllImport("netapi32.dll", CharSet = CharSet.Unicode)]
            public static extern int NetLocalGroupGetMembers(string server, string group, int level, out IntPtr buffer, int maxLength,
                out int read, out int total, ref IntPtr resume);
            [DllImport("netapi32.dll")]
            public static extern int NetApiBufferFree(IntPtr buffer);
            [DllImport("shell32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
            public static extern bool ShellExecuteExW(ref ShellExecuteInfo info);
            [DllImport("kernel32.dll")]
            public static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
            [DllImport("kernel32.dll", SetLastError = true)]
            public static extern bool GetExitCodeProcess(IntPtr process, out uint code);
            [DllImport("kernel32.dll")]
            public static extern bool CloseHandle(IntPtr h);
        }
    }
}
