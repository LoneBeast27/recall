using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.IO.Compression;
using System.Linq;
using System.Reflection;

namespace Recall.Setup
{
    internal sealed class InstallOptions
    {
        public readonly string Path;
        public readonly bool Shortcut;
        public readonly bool Launch;
        public readonly bool FrameTiming;

        public InstallOptions(string path, bool shortcut, bool launch, bool frameTiming)
        {
            Path = path;
            Shortcut = shortcut;
            Launch = launch;
            FrameTiming = frameTiming;
        }
    }

    internal static class SetupPackage
    {
        private const string Payload = "Recall.Payload.zip";
        private static readonly Assembly Assembly = typeof(SetupPackage).Assembly;
        public static bool HasPayload { get { return Assembly.GetManifestResourceInfo(Payload) != null; } }

        public static string InstallPath(string value)
        {
            var path = value.Trim().Replace('/', '\\');
            if (path.Length < 4 || !char.IsLetter(path[0]) || path[1] != ':' || path[2] != '\\'
                || path.Substring(2).IndexOfAny(new[] { '"', ':', '*', '?', '<', '>', '|' }) >= 0)
                return null;
            try
            {
                var full = Path.GetFullPath(path);
                return full.Length > 3 ? full.TrimEnd('\\') : null;
            }
            catch (ArgumentException) { return null; }
            catch (NotSupportedException) { return null; }
            catch (PathTooLongException) { return null; }
        }

        public static string Install(InstallOptions options, IProgress<string> progress)
        {
            var parent = Path.Combine(Path.GetTempPath(), "Recall-Setup-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(parent);
            try
            {
                SetupPrerequisites.Provision(parent, Assembly, progress);
                if (progress != null) progress.Report("Preparing program files…");
                var zip = Path.Combine(parent, "payload.zip");
                using (var payload = Assembly.GetManifestResourceStream(Payload))
                {
                    if (payload == null) throw new InvalidOperationException("This build is a setup preview.");
                    using (var file = File.Create(zip)) payload.CopyTo(file);
                }
                var package = Path.Combine(parent, "package");
                ZipFile.ExtractToDirectory(zip, package);
                return RunInstaller(package, options);
            }
            finally
            {
                try { Directory.Delete(parent, true); }
                catch (IOException) { }
                catch (UnauthorizedAccessException) { }
            }
        }

        public static void OpenGuidedSetup(string path, bool recordAfter)
        {
            var app = Path.Combine(path, "recall-native.exe");
            var info = new ProcessStartInfo(app) { WorkingDirectory = path, UseShellExecute = false };
            info.EnvironmentVariables["RECALL_SETUP"] = recordAfter ? "record" : "paused";
            using (Process.Start(info)) { }
        }

        private static string RunInstaller(string package, InstallOptions options)
        {
            var script = Path.Combine(package, "scripts", "install.ps1");
            var powershell = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System),
                "WindowsPowerShell", "v1.0", "powershell.exe");
            var info = new ProcessStartInfo(powershell) {
                WorkingDirectory = package,
                UseShellExecute = false, CreateNoWindow = true,
                RedirectStandardOutput = true, RedirectStandardError = true,
                Arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"" + script
                    + "\" -NoBuild -NoAutostart -InstallDir \"" + options.Path + "\""
                    + (options.Shortcut ? "" : " -NoShortcut")
                    + (options.FrameTiming ? "" : " -NoFrameTiming")
                    + (options.Launch ? "" : " -NoStart")
            };
            var output = new List<string>();
            var errors = new List<string>();
            using (var process = new Process { StartInfo = info })
            {
                process.OutputDataReceived += (sender, args) => KeepLast(output, args.Data);
                process.ErrorDataReceived += (sender, args) => KeepLast(errors, args.Data);
                process.Start();
                process.BeginOutputReadLine();
                process.BeginErrorReadLine();
                process.WaitForExit();
                if (process.ExitCode == 0)
                {
                    const string prefix = "WARNING:";
                    var warning = output.Concat(errors).FirstOrDefault(line => line.StartsWith(prefix, StringComparison.OrdinalIgnoreCase));
                    return warning == null ? null : warning.Substring(prefix.Length).Trim();
                }
                if (process.ExitCode == 3)
                    throw new InvalidOperationException("Close the Recall window, then run setup again.");
                string detail = errors.Concat(output).FirstOrDefault(line => !string.IsNullOrWhiteSpace(line));
                throw new InvalidOperationException(detail ?? "The installer exited with code " + process.ExitCode + ".");
            }
        }

        private static void KeepLast(List<string> lines, string value)
        {
            if (string.IsNullOrWhiteSpace(value)) return;
            if (lines.Count == 18) lines.RemoveAt(0);
            lines.Add(value.Trim());
        }
    }
}
