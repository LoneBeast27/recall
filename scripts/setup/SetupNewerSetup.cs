using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Linq;
using System.Net;
using System.Security.Cryptography;
using System.Threading;
using System.Threading.Tasks;
using System.Windows.Forms;

namespace Recall.Setup
{
    /// A Setup opened by hand that is not the latest published build updates in place: it fetches
    /// the Setup the signed update file names (each address in order: GitLab, the update site,
    /// GitHub), keeps it only when its size and SHA-256 match the signed values, and starts it. The
    /// newer Setup then installs over the installed Recall as any update does. It never sends the
    /// user to a web page: an earlier Setup opened GitHub's Releases page here instead.
    internal static class NewerSetup
    {
        /// Set on the newer Setup this one starts: it never offers another download (no loop).
        public const string HandedOffVariable = "RECALL_SETUP_HANDED_OFF";
        /// Folders under %TEMP% that hold a fetched Setup (one per fetch).
        public const string FolderPrefix = "Recall-NewerSetup-";
        private const int TimeoutMs = 30000;

        /// The Setup a signed update file names.
        internal sealed class File
        {
            public readonly string Version;
            public readonly long Size;
            public readonly string Sha256;
            public readonly List<string> Urls;
            public File(string version, long size, string sha256, List<string> urls)
            {
                Version = version; Size = size; Sha256 = sha256; Urls = urls;
            }
        }

        /// This Setup was started by an older one that already offered the update.
        public static bool HandedOff()
        {
            return Environment.GetEnvironmentVariable(HandedOffVariable) == "1";
        }

        /// The first address whose body has the signed size and SHA-256, else null and why (plain
        /// words). <paramref name="get"/> returns at most limit bytes, null for a refused answer, or
        /// throws; https addresses only.
        internal static byte[] Fetch(File file, Func<string, long, Action<long>, byte[]> get, Action<long> progress, out string why)
        {
            why = "no download address was published";
            if (file == null || file.Urls == null) return null;
            foreach (var url in file.Urls)
            {
                if (!url.StartsWith("https://", StringComparison.Ordinal)) continue;
                byte[] body;
                try { body = get(url, file.Size, progress); }
                catch (WebException error) { why = "the download server did not answer (" + error.Status + ")"; continue; }
                catch (IOException) { why = "the download was cut off"; continue; }
                catch (InvalidOperationException) { why = "the download server refused the request"; continue; }
                catch (NotSupportedException) { why = "the download server refused the request"; continue; }
                if (body == null) { why = "the download server refused the request"; continue; }
                if (body.Length != file.Size || !Sha256Of(body).Equals(file.Sha256, StringComparison.OrdinalIgnoreCase))
                {
                    why = "the downloaded file did not match the signed update";
                    continue;
                }
                return body;
            }
            return null;
        }

        /// Saves a checked Setup into a new folder under <paramref name="tempRoot"/>; its path.
        internal static string Save(byte[] body, string tempRoot)
        {
            var folder = Path.Combine(tempRoot, FolderPrefix + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(folder);
            var path = Path.Combine(folder, AppsEntry.UninstallerName);
            System.IO.File.WriteAllBytes(path, body);
            return path;
        }

        /// The start of the newer Setup: no arguments (opened by hand, as this one was), marked handed off.
        internal static ProcessStartInfo StartInfo(string path)
        {
            var info = new ProcessStartInfo(path) { UseShellExecute = false, WorkingDirectory = Path.GetDirectoryName(path) };
            info.EnvironmentVariables[HandedOffVariable] = "1";
            return info;
        }

        /// Fetches, checks and starts the newer Setup while a small progress window shows. True: it
        /// runs, and this Setup should close. False: <paramref name="why"/> says what failed.
        public static bool Run(File file, Action<string> log, out string why)
        {
            string failure = null, path = null;
            long total = file == null ? 0 : Math.Max(1, file.Size), done = 0;
            using (var form = ProgressWindow(file == null ? "" : file.Version))
            {
                var bar = (ProgressBar)form.Controls[1];
                var task = Task.Run(() =>
                {
                    try
                    {
                        string reason;
                        var body = Fetch(file, Get, n => Interlocked.Exchange(ref done, n), out reason);
                        if (body == null) { failure = reason; return; }
                        path = Save(body, Path.GetTempPath());
                    }
                    catch (IOException error) { failure = "the file could not be saved (" + error.Message + ")"; }
                    catch (UnauthorizedAccessException error) { failure = "the file could not be saved (" + error.Message + ")"; }
                });
                var timer = new System.Windows.Forms.Timer { Interval = 100 };
                timer.Tick += (s, e) =>
                {
                    bar.Value = (int)Math.Min(1000, Interlocked.Read(ref done) * 1000 / total);
                    if (task.IsCompleted) { timer.Stop(); form.Close(); }
                };
                form.Shown += (s, e) => timer.Start();
                Application.Run(form);
                timer.Dispose();
                task.Wait();
            }
            why = failure;
            if (path == null) { log("Newer Setup not fetched: " + why); return false; }
            try
            {
                using (Process.Start(StartInfo(path))) { }
                log("Newer Setup " + file.Version + " started from " + path);
                return true;
            }
            catch (System.ComponentModel.Win32Exception error)
            {
                why = "the newer Setup could not start (" + error.Message + ")";
                log("Newer Setup not started: " + why);
                return false;
            }
        }

        private static Form ProgressWindow(string version)
        {
            var form = new Form
            {
                Text = "Recall Setup", FormBorderStyle = FormBorderStyle.FixedDialog, MaximizeBox = false, MinimizeBox = false,
                StartPosition = FormStartPosition.CenterScreen, AutoScaleMode = AutoScaleMode.Dpi, AutoScaleDimensions = new SizeF(96, 96),
                ClientSize = new Size(380, 96), ShowInTaskbar = true, ControlBox = false
            };
            form.Controls.Add(new Label { Text = "Downloading Recall " + version + "...", AutoSize = false, Location = new Point(16, 16), Size = new Size(348, 24) });
            form.Controls.Add(new ProgressBar { Minimum = 0, Maximum = 1000, Location = new Point(16, 52), Size = new Size(348, 22) });
            return form;
        }

        private static byte[] Get(string url, long limit, Action<long> progress)
        {
            var request = (HttpWebRequest)WebRequest.Create(url);
            request.UserAgent = "RecallSetup";
            request.Timeout = request.ReadWriteTimeout = TimeoutMs;
            using (var response = (HttpWebResponse)request.GetResponse())
            {
                // Redirects stay on HTTPS; the signed SHA-256 decides whether the bytes are kept.
                if (response.StatusCode != HttpStatusCode.OK || response.ResponseUri.Scheme != Uri.UriSchemeHttps) return null;
                using (var body = response.GetResponseStream())
                using (var memory = new MemoryStream())
                {
                    var buffer = new byte[65536];
                    int read;
                    while ((read = body.Read(buffer, 0, buffer.Length)) > 0)
                    {
                        memory.Write(buffer, 0, read);
                        if (memory.Length > limit) return null;
                        progress(memory.Length);
                    }
                    return memory.ToArray();
                }
            }
        }

        internal static string Sha256Of(byte[] body)
        {
            using (var sha = SHA256.Create()) return string.Concat(sha.ComputeHash(body).Select(b => b.ToString("x2")));
        }
    }
}
