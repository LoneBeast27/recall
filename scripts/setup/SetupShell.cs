using System;
using System.Collections;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;

namespace Recall.Setup
{
    /// Start menu shortcut through the shell's own IShellLink (no script host).
    internal static class Shortcut
    {
        public static void Write(string lnk, string target, string workingDirectory, string icon, string description)
        {
            Directory.CreateDirectory(Path.GetDirectoryName(lnk));
            var link = (IShellLinkW)new ShellLink();
            try
            {
                link.SetPath(target);
                link.SetWorkingDirectory(workingDirectory);
                link.SetIconLocation(icon, 0);
                link.SetDescription(description);
                ((IPersistFile)link).Save(lnk, true);
            }
            finally { Marshal.ReleaseComObject(link); }
        }

        /// The shortcut's target path, or null when it can't be read.
        public static string Target(string lnk)
        {
            if (!File.Exists(lnk)) return null;
            var link = (IShellLinkW)new ShellLink();
            try
            {
                ((IPersistFile)link).Load(lnk, 0);
                var path = new StringBuilder(1024);
                link.GetPath(path, path.Capacity, IntPtr.Zero, 0x4); // SLGP_RAWPATH
                return path.ToString();
            }
            catch (COMException) { return null; }
            finally { Marshal.ReleaseComObject(link); }
        }

        [ComImport, Guid("00021401-0000-0000-C000-000000000046")]
        private class ShellLink { }

        [ComImport, InterfaceType(ComInterfaceType.InterfaceIsIUnknown), Guid("000214F9-0000-0000-C000-000000000046")]
        private interface IShellLinkW
        {
            void GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder file, int size, IntPtr findData, uint flags);
            void GetIDList(out IntPtr list);
            void SetIDList(IntPtr list);
            void GetDescription([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder name, int size);
            void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string name);
            void GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder dir, int size);
            void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string dir);
            void GetArguments([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder args, int size);
            void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string args);
            void GetHotkey(out short hotkey);
            void SetHotkey(short hotkey);
            void GetShowCmd(out int show);
            void SetShowCmd(int show);
            void GetIconLocation([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int size, out int icon);
            void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string path, int icon);
            void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string path, uint reserved);
            void Resolve(IntPtr window, uint flags);
            void SetPath([MarshalAs(UnmanagedType.LPWStr)] string file);
        }
    }

    /// A program started so it outlives Setup: CreateProcess with this session's explorer.exe as
    /// its parent (PROC_THREAD_ATTRIBUTE_PARENT_PROCESS), so it is not Setup's child and sits in no
    /// job of Setup's, as if started from the Start menu. Same method as the start-detached tool.
    internal sealed class DetachedProcess : IDisposable
    {
        public readonly int Pid;
        public readonly int ParentPid;
        private IntPtr handle;

        private DetachedProcess(int pid, int parent, IntPtr handle)
        {
            Pid = pid; ParentPid = parent; this.handle = handle;
        }

        /// Exited, and with which code.
        public bool HasExited(out int code)
        {
            uint value;
            code = 0;
            if (handle == IntPtr.Zero || !Native.GetExitCodeProcess(handle, out value)) return false;
            if (value == 259) return false; // STILL_ACTIVE
            code = (int)value;
            return true;
        }

        public void Dispose()
        {
            if (handle != IntPtr.Zero) Native.CloseHandle(handle);
            handle = IntPtr.Zero;
        }

        public static DetachedProcess Start(string exe, string arguments, IDictionary<string, string> extraEnvironment)
        {
            int session = SessionProcesses.CurrentSession;
            int shell = Explorer(session);
            if (shell == 0) throw new InvalidOperationException("Windows Explorer is not running in this session, so Recall can't be started on its own. Start Recall from the Start menu.");
            IntPtr parent = Native.OpenProcess(0x0080, false, (uint)shell); // PROCESS_CREATE_PROCESS
            if (parent == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "open explorer.exe");
            IntPtr size = IntPtr.Zero;
            Native.InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref size);
            IntPtr list = Marshal.AllocHGlobal(size);
            IntPtr value = Marshal.AllocHGlobal(IntPtr.Size);
            IntPtr environment = Marshal.StringToHGlobalUni(EnvironmentBlock(extraEnvironment));
            bool listReady = false;
            try
            {
                if (!Native.InitializeProcThreadAttributeList(list, 1, 0, ref size)) throw new Win32Exception(Marshal.GetLastWin32Error(), "attribute list");
                listReady = true;
                Marshal.WriteIntPtr(value, parent);
                if (!Native.UpdateProcThreadAttribute(list, 0, (IntPtr)0x00020000, value, (IntPtr)IntPtr.Size, IntPtr.Zero, IntPtr.Zero))
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "parent attribute");
                var info = new Native.StartupInfoEx();
                info.StartupInfo.cb = Marshal.SizeOf(typeof(Native.StartupInfoEx));
                info.AttributeList = list;
                Native.ProcessInformation process;
                var command = new StringBuilder("\"" + exe + "\"" + (string.IsNullOrEmpty(arguments) ? "" : " " + arguments));
                // EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NEW_PROCESS_GROUP | CREATE_DEFAULT_ERROR_MODE
                const uint flags = 0x00080000 | 0x00000400 | 0x00000200 | 0x04000000;
                if (!Native.CreateProcessW(exe, command, IntPtr.Zero, IntPtr.Zero, false, flags, environment,
                    Path.GetDirectoryName(exe), ref info, out process))
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "start " + exe);
                Native.CloseHandle(process.Thread);
                return new DetachedProcess(process.ProcessId, shell, process.Process);
            }
            finally
            {
                if (listReady) Native.DeleteProcThreadAttributeList(list);
                Marshal.FreeHGlobal(list);
                Marshal.FreeHGlobal(value);
                Marshal.FreeHGlobal(environment);
                Native.CloseHandle(parent);
            }
        }

        /// This session's longest-running explorer.exe (the shell), or 0.
        private static int Explorer(int session)
        {
            int best = 0;
            DateTime oldest = DateTime.MaxValue;
            foreach (var p in Process.GetProcessesByName("explorer"))
            {
                try
                {
                    if (p.SessionId == session && p.StartTime < oldest) { oldest = p.StartTime; best = p.Id; }
                }
                catch (Win32Exception) { }
                catch (InvalidOperationException) { }
                finally { p.Dispose(); }
            }
            return best;
        }

        /// This process's environment plus the extra values, as a sorted, double-NUL-terminated block.
        internal static string EnvironmentBlock(IDictionary<string, string> extra)
        {
            var values = new SortedDictionary<string, string>(StringComparer.OrdinalIgnoreCase);
            foreach (DictionaryEntry e in Environment.GetEnvironmentVariables()) values[(string)e.Key] = (string)e.Value;
            if (extra != null) foreach (var e in extra) values[e.Key] = e.Value;
            var block = new StringBuilder();
            foreach (var e in values) block.Append(e.Key).Append('=').Append(e.Value).Append('\0');
            return block.Append('\0').ToString();
        }

        private static class Native
        {
            [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
            public struct StartupInfo
            {
                public int cb; public string Reserved, Desktop, Title;
                public int X, Y, XSize, YSize, XCountChars, YCountChars, FillAttribute, Flags;
                public short ShowWindow, Reserved2; public IntPtr Reserved3, StdInput, StdOutput, StdError;
            }
            [StructLayout(LayoutKind.Sequential)]
            public struct StartupInfoEx { public StartupInfo StartupInfo; public IntPtr AttributeList; }
            [StructLayout(LayoutKind.Sequential)]
            public struct ProcessInformation { public IntPtr Process, Thread; public int ProcessId, ThreadId; }
            [DllImport("kernel32.dll", SetLastError = true)]
            public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
            [DllImport("kernel32.dll", SetLastError = true)]
            public static extern bool InitializeProcThreadAttributeList(IntPtr list, int count, int flags, ref IntPtr size);
            [DllImport("kernel32.dll", SetLastError = true)]
            public static extern bool UpdateProcThreadAttribute(IntPtr list, uint flags, IntPtr attribute, IntPtr value, IntPtr size, IntPtr previous, IntPtr returned);
            [DllImport("kernel32.dll")]
            public static extern void DeleteProcThreadAttributeList(IntPtr list);
            [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
            public static extern bool CreateProcessW(string application, StringBuilder command, IntPtr processAttributes, IntPtr threadAttributes,
                bool inherit, uint flags, IntPtr environment, string directory, ref StartupInfoEx info, out ProcessInformation process);
            [DllImport("kernel32.dll", SetLastError = true)]
            public static extern bool GetExitCodeProcess(IntPtr process, out uint code);
            [DllImport("kernel32.dll")]
            public static extern bool CloseHandle(IntPtr h);
        }
    }
}
