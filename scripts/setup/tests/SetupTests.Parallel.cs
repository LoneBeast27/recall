using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using Microsoft.Win32;

namespace Recall.Setup.Tests
{
    /// How the tests run. Default: the in-process tests one after another in this process, then
    /// each isolated (sandbox) test in a process of its own, a few at a time. Each such process
    /// has its own environment variables and static hooks, its own %TEMP% folder, and its own
    /// HKCU (RegOverridePredefKey onto a throwaway key), so two sandboxes never share the
    /// HKCU\Software\RecallSandbox entries. --serial runs everything in this process, in order.
    ///   RecallSetupTests.exe <source dir> <fake exe> [--serial] [--jobs N]
    internal static partial class TestRunner
    {
        private sealed class TestCase
        {
            public string Name;
            public Action Body;
            public bool Isolated;
        }

        private static readonly List<TestCase> cases = new List<TestCase>();
        private const int IsolatedTimeoutMs = 300000;

        /// A test that runs in the runner's own process: no fake processes, nothing process-wide
        /// it leaves changed. They stay one after another: the swap tests set static hooks
        /// (SwapTransaction.BeforeMove, BeforeRecover) and all of them together take about a second.
        private static void Add(string name, Action body) { cases.Add(new TestCase { Name = name, Body = body }); }

        /// A sandbox test: alone in its own process (see the class comment).
        private static void Isolated(string name, Action body) { cases.Add(new TestCase { Name = name, Body = body, Isolated = true }); }

        private static int RunAll(string[] args)
        {
            var rest = args.Skip(2).ToList();
            var one = rest.IndexOf("--one");
            if (one >= 0) return RunOne(int.Parse(rest[one + 1]), rest[one + 2]);
            var serial = rest.Contains("--serial") || Environment.GetEnvironmentVariable("RECALL_SETUP_TESTS_SERIAL") == "1";
            var jobsAt = rest.IndexOf("--jobs");
            var jobs = jobsAt >= 0 ? int.Parse(rest[jobsAt + 1]) : Math.Max(2, Math.Min(6, Environment.ProcessorCount));
            var clock = Stopwatch.StartNew();
            if (serial)
            {
                foreach (var c in cases) Run(c.Name, c.Body);
            }
            else
            {
                foreach (var c in cases.Where(c => !c.Isolated)) Run(c.Name, c.Body);
                RunIsolated(Math.Max(1, jobs));
            }
            var how = serial ? "serial" : jobs + " isolated processes at a time";
            Console.WriteLine(passed + " passed, " + failed + " failed (" + how + ", " + clock.Elapsed.TotalSeconds.ToString("0") + " s)");
            return failed == 0 ? 0 : 1;
        }

        private static void RunIsolated(int jobs)
        {
            var self = Process.GetCurrentProcess().MainModule.FileName;
            var work = Path.Combine(Path.GetDirectoryName(Path.GetFullPath(fakeExe)), "isolated-" + Guid.NewGuid().ToString("N").Substring(0, 12));
            Directory.CreateDirectory(work);
            var queue = new Queue<int>(Enumerable.Range(0, cases.Count).Where(i => cases[i].Isolated));
            var order = queue.ToArray();
            var results = new string[cases.Count];
            var workers = Enumerable.Range(0, Math.Min(jobs, order.Length)).Select(_ => new Thread(() =>
            {
                while (true)
                {
                    int i;
                    lock (queue) { if (queue.Count == 0) return; i = queue.Dequeue(); }
                    results[i] = Child(self, work, i);
                }
            })).ToList();
            foreach (var w in workers) w.Start();
            foreach (var w in workers) w.Join();
            // Results in registration order, each test's lines together.
            foreach (var i in order)
            {
                foreach (var line in results[i].Split(new[] { "\r\n", "\n" }, StringSplitOptions.None))
                {
                    if (line.StartsWith("PASS ")) passed++;
                    else if (line.StartsWith("FAIL ")) failed++;
                    Console.WriteLine(line);
                }
            }
            try { Directory.Delete(work, true); } catch (Exception) { }
        }

        /// One isolated test in a new process; returns its output (one PASS or FAIL line).
        private static string Child(string self, string work, int index)
        {
            var name = cases[index].Name;
            var temp = Path.Combine(work, "temp-" + index);
            var output = Path.Combine(work, "out-" + index + ".txt");
            Directory.CreateDirectory(temp);
            try
            {
                var info = new ProcessStartInfo(self, Quote(sourceDir) + " " + Quote(fakeExe) + " --one " + index + " " + Quote(output))
                {
                    UseShellExecute = false,
                    CreateNoWindow = true
                };
                info.EnvironmentVariables["TEMP"] = temp;
                info.EnvironmentVariables["TMP"] = temp;
                var clock = Stopwatch.StartNew();
                using (var p = Process.Start(info))
                {
                    if (!p.WaitForExit(IsolatedTimeoutMs))
                    {
                        try { p.Kill(); } catch (Exception) { }
                        return "FAIL " + name + ": its process ran past " + IsolatedTimeoutMs / 1000 + " s";
                    }
                    var text = File.Exists(output) ? File.ReadAllText(output).TrimEnd() : "";
                    var lines = text.Split(new[] { "\r\n", "\n" }, StringSplitOptions.None);
                    var results = lines.Count(l => l.StartsWith("PASS ") || l.StartsWith("FAIL "));
                    if (results != 1)
                        return "FAIL " + name + ": its process (exit " + p.ExitCode + ") wrote " + results + " results: " + text.Replace("\n", " | ");
                    if (p.ExitCode != 0 && lines.Any(l => l.StartsWith("PASS ")))
                        return "FAIL " + name + ": passed, but its process exited " + p.ExitCode + ": " + text.Replace("\n", " | ");
                    return text + (clock.Elapsed.TotalSeconds >= 1 ? "  (" + clock.Elapsed.TotalSeconds.ToString("0") + " s)" : "");
                }
            }
            catch (Exception e)
            {
                return "FAIL " + name + ": its process did not run: " + e.Message;
            }
            finally
            {
                try { Directory.Delete(temp, true); } catch (Exception) { }
            }
        }

        /// The --one side: run one test, its lines into `output`, under an isolated HKCU.
        private static int RunOne(int index, string output)
        {
            using (var writer = new StreamWriter(output, false, new UTF8Encoding(false)))
            {
                writer.AutoFlush = true;
                Console.SetOut(writer);
                var c = cases[index];
                IsolatedHive hive;
                try { hive = new IsolatedHive(); }
                catch (Exception e) { Console.WriteLine("FAIL " + c.Name + ": no isolated HKCU: " + e.Message); return 1; }
                using (hive) Run(c.Name, c.Body);
            }
            return failed == 0 && passed == 1 ? 0 : 1;
        }

        private static string Quote(string arg)
        {
            // A trailing backslash would escape the closing quote.
            return "\"" + (arg.EndsWith("\\") ? arg + "\\" : arg) + "\"";
        }

        /// HKCU of this process mapped onto HKCU\Software\RecallSetupTests\<guid>, removed after.
        private sealed class IsolatedHive : IDisposable
        {
            private const string Parent = @"Software\RecallSetupTests";
            private static readonly IntPtr CurrentUser = new IntPtr(unchecked((int)0x80000001));
            private readonly string path = Parent + @"\" + Guid.NewGuid().ToString("N");
            private readonly RegistryKey key;

            [DllImport("advapi32.dll")]
            private static extern int RegOverridePredefKey(IntPtr key, IntPtr newKey);

            public IsolatedHive()
            {
                key = Registry.CurrentUser.CreateSubKey(path);
                var code = RegOverridePredefKey(CurrentUser, key.Handle.DangerousGetHandle());
                if (code != 0) throw new Win32Exception(code, "RegOverridePredefKey");
                // The instrument check: a value written under the throwaway key reads back at the HKCU root.
                key.SetValue("isolated", path);
                if (Registry.CurrentUser.GetValue("isolated") as string != path)
                    throw new InvalidOperationException("HKCU did not move to " + path);
            }

            public void Dispose()
            {
                RegOverridePredefKey(CurrentUser, IntPtr.Zero);
                key.Dispose();
                Registry.CurrentUser.DeleteSubKeyTree(path, false);
                using (var parent = Registry.CurrentUser.OpenSubKey(Parent, true))
                    if (parent != null && parent.SubKeyCount == 0 && parent.ValueCount == 0)
                        try { Registry.CurrentUser.DeleteSubKey(Parent, false); } catch (Exception) { }
            }
        }
    }
}
