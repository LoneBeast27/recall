using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Pipes;
using System.Security.Cryptography;
using System.Text;
using System.Threading;

namespace Recall.Setup.Fake
{
    /// A stand-in for Recall's programs in Setup's sandboxed update tests (build.ps1 -Test builds it as
    /// a window-less exe; copies are named recall-hdr.exe, recall-native.exe, ...). Everything it sees
    /// goes to FAKE_LOG as "&lt;exe&gt; &lt;pid&gt; &lt;text&gt;". It opens no device, no window and no network.
    ///   recall-native.exe: logs its arguments and RECALL_SETUP, then exits.
    ///   recall-hdr.exe: serves RECALL_SETUP_PIPE like the recorder's control pipe. After a quit it
    ///     answers finishing_clip=true for FAKE_FINISHING_POLLS status polls, then exits, logging
    ///     whether every file beside it was still unchanged ("intact"). FAKE_LONG_PATH: a long
    ///     recording runs to that path; FAKE_FINALIZE=1 renames its .part on a save_first quit, else
    ///     the .part stays (a quit that gave up). FAKE_TOUCH: a file it overwrites on quit (a
    ///     settings write after the backup). Exits after 20 s without a client.
    internal static class FakeRecall
    {
        private static string me;
        private static readonly Dictionary<string, string> Siblings = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);

        private static int Main(string[] args)
        {
            var exe = System.Reflection.Assembly.GetExecutingAssembly().Location;
            me = Path.GetFileName(exe).ToLowerInvariant();
            Log("start " + string.Join(" ", args));
            if (me != "recall-hdr.exe")
            {
                Log("env RECALL_SETUP=" + (Environment.GetEnvironmentVariable("RECALL_SETUP") ?? "-"));
                return 0;
            }
            foreach (var f in Directory.GetFiles(Path.GetDirectoryName(exe))) Siblings[f] = Hash(f);
            Serve(Environment.GetEnvironmentVariable("RECALL_SETUP_PIPE"));
            return 0;
        }

        private static void Serve(string name)
        {
            int polls = 0, finishing = Number("FAKE_FINISHING_POLLS");
            bool quitting = false;
            string longPath = Environment.GetEnvironmentVariable("FAKE_LONG_PATH");
            bool longOn = !string.IsNullOrEmpty(longPath);
            while (true)
            {
                using (var pipe = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous))
                {
                    var wait = pipe.BeginWaitForConnection(null, null);
                    if (!wait.AsyncWaitHandle.WaitOne(20000)) { Log("idle exit"); return; }
                    pipe.EndWaitForConnection(wait);
                    var reader = new StreamReader(pipe, new UTF8Encoding(false));
                    var writer = new StreamWriter(pipe, new UTF8Encoding(false)) { AutoFlush = true };
                    string line;
                    try { line = reader.ReadLine(); }
                    catch (IOException) { continue; }
                    if (line == null) continue;
                    line = line.Trim();
                    if (line.Contains("\"status\""))
                    {
                        if (quitting && ++polls > finishing) { Exit(); return; }
                        var fin = quitting ? "true" : "false";
                        var lr = longOn ? "{\"active\":true,\"path\":\"" + longPath.Replace("\\", "\\\\") + "\"}" : "{\"active\":false,\"path\":null}";
                        Write(writer, "{\"mod\":\"session\",\"event\":{\"type\":\"status\",\"state\":\"recording\",\"finishing_clip\":" + fin + ",\"long_recording\":" + lr + "}}");
                        continue;
                    }
                    Log("line " + line);
                    if (line.Contains("\"record_start\"")) longOn = true;
                    if (!line.Contains("\"quit\"")) continue;
                    quitting = true;
                    OnQuit(line, longOn, longPath);
                    if (finishing == 0) { Exit(); return; }
                }
            }
        }

        private static void OnQuit(string line, bool longOn, string longPath)
        {
            var touch = Environment.GetEnvironmentVariable("FAKE_TOUCH");
            if (!string.IsNullOrEmpty(touch)) File.WriteAllText(touch, "written by the recorder after the backup");
            if (longOn && line.Contains("\"save_first\":true") && Environment.GetEnvironmentVariable("FAKE_FINALIZE") == "1"
                && File.Exists(longPath + ".part"))
                File.Move(longPath + ".part", longPath);
        }

        private static void Exit()
        {
            var changed = new List<string>();
            foreach (var kv in Siblings)
                if (!File.Exists(kv.Key) || Hash(kv.Key) != kv.Value) changed.Add(Path.GetFileName(kv.Key));
            Log(changed.Count == 0 ? "exit intact" : "exit changed " + string.Join(",", changed.ToArray()));
        }

        private static void Write(StreamWriter writer, string text)
        {
            try { writer.WriteLine(text); Thread.Sleep(100); }
            catch (IOException) { }
        }

        private static int Number(string name)
        {
            int n;
            return int.TryParse(Environment.GetEnvironmentVariable(name), out n) ? n : 0;
        }

        private static string Hash(string path)
        {
            try
            {
                using (var s = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete))
                using (var h = SHA256.Create()) return BitConverter.ToString(h.ComputeHash(s));
            }
            catch (IOException) { return "locked"; }
        }

        private static void Log(string text)
        {
            var log = Environment.GetEnvironmentVariable("FAKE_LOG");
            if (string.IsNullOrEmpty(log)) return;
            var line = me + " " + System.Diagnostics.Process.GetCurrentProcess().Id + " " + text + Environment.NewLine;
            for (int i = 0; i < 20; i++)
            {
                try { File.AppendAllText(log, line); return; }
                catch (IOException) { Thread.Sleep(50); }
            }
        }
    }
}
