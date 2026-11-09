using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.IO.Compression;
using System.Linq;
using System.Reflection;
using System.Security.Cryptography;
using System.Net;

namespace Recall.Setup
{
    internal sealed class InstallOptions
    {
        public readonly string Path;
        public readonly bool Shortcut;
        public readonly bool Launch;
        public readonly bool FrameTiming;
        public readonly bool Update;

        public InstallOptions(string path, bool shortcut, bool launch, bool frameTiming, bool update)
        {
            Path = path;
            Shortcut = shortcut;
            Launch = launch;
            FrameTiming = frameTiming;
            Update = update;
        }
    }

    internal static class SetupPackage
    {
        private const string Payload = "Recall.Payload.zip";
        private const string FfmpegUrl = "https://github.com/GyanD/codexffmpeg/releases/download/7.1.1/ffmpeg-7.1.1-full_build-shared.zip";
        private const string FfmpegSha256 = "9F28727E8B472A04C1D2E520AAA425DCA82721B995139B35710091130EA6E699";
        private const string FfmpegRoot = "ffmpeg-7.1.1-full_build-shared/";
        private static readonly string[] FfmpegRuntime = {
            "ffmpeg.exe", "ffprobe.exe", "avcodec-61.dll", "avdevice-61.dll", "avfilter-10.dll",
            "avformat-61.dll", "avutil-59.dll", "postproc-58.dll", "swresample-5.dll", "swscale-8.dll"
        };
        private static readonly string[] FfmpegRuntimeSha256 = {
            "82813A6B5E4C3590C6FC36553D74E1812F51DD48D2A46CD0F966785FDE9609EB",
            "21757B5DF8F2247A2C7176E279CDC4F80039E07A19288D374C164F7BEF00468E",
            "0209F3620970037BBADE73939E53BD51F4422B3A79F290B130C2EB1224087CF7",
            "53144E6FC84C0E2D977CDD10C347D35CA886E30B6EB72F339ABAA3C570262661",
            "910321E4A32329DCD8057CE189A003F4BE3C631A645DAA34C9DCC344A0AA767E",
            "233CD46F0F5860C032D65D7BD124AE945470883B85E1D1BC248991FE8115A28E",
            "2E6CD0A65A892C3AF3D0CC1B6588E2ED8304C0E22E7488B7629E2B3E98CD8573",
            "079D7DA925D44D399DAAC6D4F0503154E4FF655CF4CB026246CF56FB3F4A3246",
            "D0294107447A14168CE4343FC54105886D2B595EFD5D52F51FE04177FBD230CA",
            "C01DD5D2995EC445315B9E67AD444E349D3186B5C0EFD31EA599B48594B51250"
        };
        private const string FfmpegLicenseSha256 = "8CEB4B9EE5ADEDDE47B31E975C1D90C73AD27B6B165A1DCD80C7C545EB65B903";
        private static readonly Assembly Assembly = typeof(SetupPackage).Assembly;
        public static bool HasPayload { get { return Assembly.GetManifestResourceInfo(Payload) != null; } }
#if ONLINE_DEPENDENCIES
        private static bool OnlineDependencies() { return true; }
#else
        private static bool OnlineDependencies() { return false; }
#endif

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

        public static string ExistingInstallPath(string value)
        {
            var path = InstallPath(value);
            if (path == null || !Directory.Exists(path)) return null;
            return File.Exists(Path.Combine(path, "recall-native.exe"))
                && File.Exists(Path.Combine(path, "recall-hdr.exe"))
                && File.Exists(Path.Combine(path, "recall.exe")) ? path : null;
        }

        public static string Install(InstallOptions options, IProgress<string> progress)
        {
            var parent = Path.Combine(Path.GetTempPath(), "Recall-Setup-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(parent);
            try
            {
                if (progress != null) progress.Report("Preparing program files…");
                var zip = Path.Combine(parent, "payload.zip");
                using (var payload = Assembly.GetManifestResourceStream(Payload))
                {
                    if (payload == null) throw new InvalidOperationException("This build is a setup preview.");
                    using (var file = File.Create(zip)) payload.CopyTo(file);
                }
                var package = Path.Combine(parent, "package");
                ZipFile.ExtractToDirectory(zip, package);
                if (OnlineDependencies()) AddFfmpeg(package, parent, options, progress);
                SetupPrerequisites.Provision(parent, Assembly, progress);
                return RunInstaller(package, options);
            }
            finally
            {
                try { Directory.Delete(parent, true); }
                catch (IOException) { }
                catch (UnauthorizedAccessException) { }
            }
        }

        private static void AddFfmpeg(string package, string parent, InstallOptions options, IProgress<string> progress)
        {
            if (options.Update && ReuseInstalledFfmpeg(package, options.Path, progress)) return;
            if (progress != null) progress.Report("Downloading verified FFmpeg components…");
            var archive = Path.Combine(parent, "ffmpeg-7.1.1-full_build-shared.zip");
            ServicePointManager.SecurityProtocol |= (SecurityProtocolType)3072;
            using (var client = new WebClient()) client.DownloadFile(FfmpegUrl, archive);
            if (!HasSha256(archive, FfmpegSha256))
                throw new InvalidOperationException("The FFmpeg download failed its SHA-256 check. Recall was not changed.");
            var target = Path.Combine(package, @"tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared\bin");
            Directory.CreateDirectory(target);
            using (var zip = ZipFile.OpenRead(archive))
            {
                foreach (var name in FfmpegRuntime)
                    ExtractRequired(zip, FfmpegRoot + "bin/" + name, Path.Combine(target, name));
                ExtractRequired(zip, FfmpegRoot + "LICENSE", Path.Combine(package, "FFMPEG-LICENSE.txt"));
            }
        }

        private static bool ReuseInstalledFfmpeg(string package, string install, IProgress<string> progress)
        {
            for (int i = 0; i < FfmpegRuntime.Length; i++)
                if (!HasSha256(Path.Combine(install, FfmpegRuntime[i]), FfmpegRuntimeSha256[i])) return false;
            var license = Path.Combine(install, "FFMPEG-LICENSE.txt");
            if (!HasSha256(license, FfmpegLicenseSha256)) return false;
            if (progress != null) progress.Report("Reusing verified FFmpeg components…");
            var target = Path.Combine(package, @"tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared\bin");
            Directory.CreateDirectory(target);
            foreach (var name in FfmpegRuntime) File.Copy(Path.Combine(install, name), Path.Combine(target, name));
            var stagedLicense = Path.Combine(package, "FFMPEG-LICENSE.txt");
            File.Copy(license, stagedLicense);
            for (int i = 0; i < FfmpegRuntime.Length; i++)
            {
                if (HasSha256(Path.Combine(target, FfmpegRuntime[i]), FfmpegRuntimeSha256[i])) continue;
                RemoveStagedFfmpeg(target, stagedLicense);
                return false;
            }
            if (!HasSha256(stagedLicense, FfmpegLicenseSha256))
            {
                RemoveStagedFfmpeg(target, stagedLicense);
                return false;
            }
            return true;
        }

        private static void RemoveStagedFfmpeg(string target, string license)
        {
            foreach (var name in FfmpegRuntime)
            {
                var path = Path.Combine(target, name);
                if (File.Exists(path)) File.Delete(path);
            }
            if (File.Exists(license)) File.Delete(license);
        }

        private static bool HasSha256(string path, string expected)
        {
            if (!File.Exists(path)) return false;
            using (var stream = File.OpenRead(path))
            using (var hash = SHA256.Create())
            {
                var actual = BitConverter.ToString(hash.ComputeHash(stream)).Replace("-", "");
                return actual.Equals(expected, StringComparison.OrdinalIgnoreCase);
            }
        }

        private static void ExtractRequired(ZipArchive archive, string name, string destination)
        {
            var entry = archive.GetEntry(name);
            if (entry == null) throw new InvalidOperationException("The verified FFmpeg archive is missing " + name + ".");
            using (var input = entry.Open())
            using (var output = new FileStream(destination, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                input.CopyTo(output);
        }

        public static void OpenGuidedSetup(string path, bool recordAfter)
        {
            var app = Path.Combine(path, "recall-native.exe");
            var info = new ProcessStartInfo(app) { WorkingDirectory = path, UseShellExecute = false };
            info.EnvironmentVariables["RECALL_SETUP"] = recordAfter ? "record" : "paused";
            using (Process.Start(info)) { }
        }

        public static void OpenRecall(string path)
        {
            var app = Path.Combine(path, "recall-native.exe");
            var info = new ProcessStartInfo(app) { WorkingDirectory = path, UseShellExecute = false };
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
                    + (options.Update ? " -CloseApp -UpdateExisting" : "")
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
