using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
using System.Threading;
using System.Web.Script.Serialization;

namespace Recall.Setup
{
    /// What the recorder says about itself (its status event on the control pipe).
    internal sealed class RecorderStatus
    {
        public bool FinishingClip;
        public bool QuitForSignOut;
        public long Bytes;
        public long Saves;
        /// A long recording is running (`long_recording.active`), to `LongPath`.
        public bool LongActive;
        public string LongPath;
        /// The recorder captures a game or the desktop (`state`).
        public bool Capturing;

        /// A `{"mod":"session","event":{"type":"status",...}}` line, else null.
        public static RecorderStatus Parse(string line)
        {
            if (string.IsNullOrWhiteSpace(line)) return null;
            Dictionary<string, object> message;
            try { message = new JavaScriptSerializer().DeserializeObject(line) as Dictionary<string, object>; }
            catch (ArgumentException) { return null; }
            catch (InvalidOperationException) { return null; }
            if (message == null || !"session".Equals(Get(message, "mod"))) return null;
            var e = Get(message, "event") as Dictionary<string, object>;
            if (e == null || !"status".Equals(Get(e, "type"))) return null;
            var status = new RecorderStatus {
                FinishingClip = true.Equals(Get(e, "finishing_clip")),
                QuitForSignOut = true.Equals(Get(e, "quit_for_sign_out"))
            };
            var state = Get(e, "state") as string;
            status.Capturing = state == "recording" || state == "starting" || state == "degraded" || state == "recovering";
            var longRec = Get(e, "long_recording") as Dictionary<string, object>;
            if (longRec != null)
            {
                status.LongActive = true.Equals(Get(longRec, "active"));
                status.LongPath = Get(longRec, "path") as string;
            }
            var progress = Get(e, "finishing_progress") as Dictionary<string, object>;
            if (progress != null)
            {
                status.Bytes = Number(Get(progress, "bytes"));
                status.Saves = Number(Get(progress, "saves"));
            }
            return status;
        }

        private static object Get(Dictionary<string, object> map, string key)
        {
            object value;
            return map.TryGetValue(key, out value) ? value : null;
        }

        private static long Number(object value)
        {
            if (value is int) return (int)value;
            if (value is long) return (long)value;
            if (value is decimal) return (long)(decimal)value;
            if (value is double) return (long)(double)value;
            return 0;
        }
    }

    /// The recorder's control pipe, as a client that writes only to a server it owns: the pipe's
    /// server process must be one of the controllers this run may quit (its exe in the install
    /// folder) and must run as this account. Same rules as the dev installer's pipe client.
    internal sealed class RecorderPipe
    {
        private readonly SetupTargets targets;
        private readonly Func<List<SessionProcess>> controllers;
        private readonly Action<string> log;
        private readonly HashSet<string> warned = new HashSet<string>();
        public readonly string MySid = WindowsIdentity.GetCurrent().User.Value;

        /// A refusal ends the status probe after a start.
        public bool OwnerRefused;
        /// The recorder that got the last line, with its start time.
        public QuitTarget SentTo;

        public RecorderPipe(SetupTargets targets, Func<List<SessionProcess>> controllers, Action<string> log)
        {
            this.targets = targets;
            this.controllers = controllers;
            this.log = log;
        }

        public bool Send(string line) { return Send(line, targets.SessionPipe, 5); }

        public bool Send(string line, string name, int tries)
        {
            // The controller serves one pipe instance at a time: retry a connect that lands between
            // instances, and keep the client open briefly so the line is read before it disconnects.
            bool legacy = name == targets.LegacyPipe;
            for (int attempt = 0; attempt < tries; attempt++)
            {
                try
                {
                    using (var pipe = new NamedPipeClientStream(".", name, PipeDirection.InOut, PipeOptions.Asynchronous))
                    {
                        pipe.Connect(1500);
                        int server = ServerPid(pipe);
                        string owner = server == 0 ? null : OwnerSid(server);
                        var ours = server == 0 ? null : controllers().Find(p => p.Pid == server);
                        var decision = QuitPolicy.MayWrite(server, owner, MySid, ours != null);
                        if (decision != null)
                        {
                            if (owner != MySid && server != 0)
                            {
                                OwnerRefused = true;
                                var why = owner != null
                                    ? "another user owns the Recall pipe (" + name + ", pid " + server + " runs as " + owner + ")"
                                    : "can't tell which account runs the Recall pipe (" + name + ", pid " + server + ")";
                                if (warned.Add(why)) log("WARNING: " + why + ": nothing sent.");
                            }
                            // The bare legacy name is machine-wide: another server may serve its next
                            // instance, and ours the one after. This session's own pipe has one server.
                            if (legacy) { Thread.Sleep(300); continue; }
                            return false;
                        }
                        WriteLine(pipe, line);
                        SentTo = new QuitTarget(server, ours.Created, name);
                        Thread.Sleep(300);
                        return true;
                    }
                }
                catch (TimeoutException) { Thread.Sleep(400); }
                catch (AggregateException) { Thread.Sleep(400); }
                catch (IOException) { Thread.Sleep(400); }
                catch (UnauthorizedAccessException) { Thread.Sleep(400); }
            }
            return false;
        }

        /// Its status, read from the pipe the quit went to; null when it does not answer within 2 s.
        /// Only the recorder named by <paramref name="target"/> is read: the pipe's server must be that pid.
        public RecorderStatus ReadStatus(QuitTarget target)
        {
            try
            {
                using (var pipe = new NamedPipeClientStream(".", target.Pipe, PipeDirection.InOut, PipeOptions.Asynchronous))
                {
                    pipe.Connect(1000);
                    if (ServerPid(pipe) != target.Pid) return null;
                    // The hub answers a new client with its status unasked; ask anyway (an older build).
                    WriteLine(pipe, "{\"cmd\":\"status\"}");
                    var reader = new StreamReader(pipe, new UTF8Encoding(false));
                    var until = DateTime.UtcNow.AddSeconds(2);
                    System.Threading.Tasks.Task<string> read = null;
                    while (DateTime.UtcNow < until)
                    {
                        if (read == null) read = reader.ReadLineAsync();
                        if (!read.Wait(250)) continue;
                        var line = read.Result;
                        read = null;
                        if (line == null) break;
                        var status = RecorderStatus.Parse(line);
                        if (status != null) return status;
                    }
                    return null;
                }
            }
            catch (Exception error)
            {
                if (error is TimeoutException || error is IOException || error is AggregateException
                    || error is UnauthorizedAccessException || error is ObjectDisposedException) return null;
                throw;
            }
        }

        /// One line, written within 2 s: a server that stops reading never holds Setup.
        private static void WriteLine(NamedPipeClientStream pipe, string line)
        {
            var bytes = new UTF8Encoding(false).GetBytes(line + "\r\n");
            var write = pipe.WriteAsync(bytes, 0, bytes.Length);
            if (!write.Wait(2000)) throw new TimeoutException("the recorder did not take the line");
        }

        public static int ServerPid(NamedPipeClientStream pipe)
        {
            uint id;
            return Native.GetNamedPipeServerProcessId(pipe.SafePipeHandle.DangerousGetHandle(), out id) ? (int)id : 0;
        }

        /// The account a process runs as: its token user SID, else the SID the session list
        /// (WTSEnumerateProcessesW) gives for it, or null. A same-account elevated recorder's token
        /// can't be opened from a non-elevated run; the list still names its account.
        public static string OwnerSid(int pid)
        {
            return TokenSid(pid) ?? WtsSid(pid);
        }

        private static string TokenSid(int pid)
        {
            IntPtr process = Native.OpenProcess(0x1000, false, (uint)pid);
            if (process == IntPtr.Zero) return null;
            try
            {
                IntPtr token;
                if (!Native.OpenProcessToken(process, 8, out token)) return null; // TOKEN_QUERY
                try { using (var id = new WindowsIdentity(token)) return id.User.Value; }
                finally { Native.CloseHandle(token); }
            }
            catch (ArgumentException) { return null; }
            catch (System.Security.SecurityException) { return null; }
            finally { Native.CloseHandle(process); }
        }

        private static string WtsSid(int pid)
        {
            IntPtr list;
            uint count;
            if (!Native.WTSEnumerateProcessesW(IntPtr.Zero, 0, 1, out list, out count)) return null;
            try
            {
                int size = Marshal.SizeOf(typeof(Native.WtsProcessInfo));
                for (long i = 0; i < count; i++)
                {
                    var info = (Native.WtsProcessInfo)Marshal.PtrToStructure(new IntPtr(list.ToInt64() + i * size), typeof(Native.WtsProcessInfo));
                    if (info.ProcessId == (uint)pid)
                        return info.pUserSid == IntPtr.Zero ? null : new SecurityIdentifier(info.pUserSid).Value;
                }
                return null;
            }
            finally { Native.WTSFreeMemory(list); }
        }

        private static class Native
        {
            [DllImport("kernel32.dll", SetLastError = true)]
            public static extern bool GetNamedPipeServerProcessId(IntPtr pipe, out uint pid);
            [DllImport("kernel32.dll", SetLastError = true)]
            public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
            [DllImport("advapi32.dll", SetLastError = true)]
            public static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
            [DllImport("kernel32.dll")]
            public static extern bool CloseHandle(IntPtr h);
            [StructLayout(LayoutKind.Sequential)]
            public struct WtsProcessInfo { public uint SessionId; public uint ProcessId; public IntPtr pProcessName; public IntPtr pUserSid; }
            [DllImport("wtsapi32.dll", SetLastError = true)]
            public static extern bool WTSEnumerateProcessesW(IntPtr server, uint reserved, uint version, out IntPtr list, out uint count);
            [DllImport("wtsapi32.dll")]
            public static extern void WTSFreeMemory(IntPtr memory);
        }
    }
}
