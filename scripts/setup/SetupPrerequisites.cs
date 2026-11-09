using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Security.Cryptography;
using Microsoft.Win32;

namespace Recall.Setup
{
    internal enum SetupDependency { VisualCpp, WindowsMedia }

    internal sealed class SetupRestartRequired : Exception
    {
        public SetupRestartRequired() : base("Restart Windows, then run Recall Setup again.") { }
    }

    internal sealed class SetupPrerequisites
    {
        internal static readonly Version VisualCppMinimum = new Version(14, 44, 35207, 0);
        private const string VcResource = "Recall.VcRedist.x64.exe";
        private static readonly string SystemDir = Environment.SystemDirectory;
        public readonly bool VisualCpp;
        public readonly bool WindowsMedia;
        public readonly bool WindowsN;
        public readonly bool NvidiaDriver;

        internal SetupPrerequisites(bool visualCpp, bool windowsMedia, bool windowsN, bool nvidiaDriver)
        {
            VisualCpp = visualCpp;
            WindowsMedia = windowsMedia;
            WindowsN = windowsN;
            NvidiaDriver = nvidiaDriver;
        }

        public static SetupPrerequisites Detect()
        {
            string edition = Registry.GetValue(@"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
                "EditionID", "") as string ?? "";
            bool media = File.Exists(Path.Combine(SystemDir, "mfplat.dll"))
                && File.Exists(Path.Combine(SystemDir, "mfplay.dll"))
                && File.Exists(Path.Combine(SystemDir, "mfreadwrite.dll"));
            return new SetupPrerequisites(HasVisualCpp(), media, edition.EndsWith("N", StringComparison.OrdinalIgnoreCase),
                File.Exists(Path.Combine(SystemDir, "nvEncodeAPI64.dll")));
        }

        private static bool HasVisualCpp()
        {
            string path = Path.Combine(SystemDir, "vcruntime140.dll");
            if (!File.Exists(path)) return false;
            try
            {
                var file = FileVersionInfo.GetVersionInfo(path);
                var version = new Version(file.FileMajorPart, file.FileMinorPart, file.FileBuildPart, file.FilePrivatePart);
                return version >= VisualCppMinimum;
            }
            catch (FileNotFoundException) { return false; }
        }

        public string Summary()
        {
            if (!WindowsMedia && !WindowsN) return "Windows media features need repair before Recall can install.";
            if (!VisualCpp && !WindowsMedia) return "Setup will add Visual C++ and the Windows Media Feature Pack.";
            if (!VisualCpp) return "Setup will install Microsoft Visual C++. Windows may ask for permission.";
            if (!WindowsMedia) return "Setup will add the Media Feature Pack using Windows Update.";
            return NvidiaDriver ? "Windows components ready. NVIDIA recording driver detected."
                : "NVIDIA recording driver missing. Add it before recording.";
        }

        public static void Provision(string folder, Assembly assembly, IProgress<string> progress)
        {
            Ensure(Detect, dependency =>
            {
                bool vc = dependency == SetupDependency.VisualCpp;
                string name = vc ? "Microsoft Visual C++" : "Windows Media Feature Pack";
                if (progress != null) progress.Report("Installing " + name + "…");
                try
                {
                    return vc ? RunVisualCpp(folder, assembly) : RunElevated(Path.Combine(SystemDir, "dism.exe"),
                        "/Online /Add-Capability /CapabilityName:Media.MediaFeaturePack~~~~0.0.1.0 /Quiet /NoRestart");
                }
                catch (Win32Exception error)
                {
                    if (error.NativeErrorCode != 1223) throw;
                    throw new InvalidOperationException("Windows permission for " + name + " was declined. Return to setup to try again.");
                }
            });
        }

        internal static void Ensure(Func<SetupPrerequisites> detect, Func<SetupDependency, int> install)
        {
            var status = detect();
            if (!status.WindowsMedia && !status.WindowsN)
                throw new InvalidOperationException("Windows media features are missing. Restore Media Foundation through Windows servicing, then run Recall Setup again.");

            bool restart = false;
            if (!status.VisualCpp)
            {
                int code = install(SetupDependency.VisualCpp);
                restart = code == 3010 || code == 1641;
                bool alreadyInstalled = code == 1638 || code == unchecked((int)0x80070666);
                if (!restart && ((code != 0 && !alreadyInstalled) || !detect().VisualCpp))
                    throw new InvalidOperationException("Microsoft Visual C++ could not be installed (code " + Code(code) + "). Install or repair the x64 redistributable, then try again.");
            }
            if (!status.WindowsMedia)
            {
                int code = install(SetupDependency.WindowsMedia);
                if (code != 0 && code != 3010)
                    throw new InvalidOperationException("Windows could not install the Media Feature Pack (code " + Code(code) + "). Add it in Windows Optional features, then try again.");
                // Windows N requires a restart even if servicing reports ordinary success.
                restart = true;
            }
            if (restart) throw new SetupRestartRequired();
        }

#if PREVIEW
        internal static void CheckPolicy()
        {
            var ready = new SetupPrerequisites(true, true, false, true);
            Ensure(() => ready, _ => { throw new InvalidOperationException("Ready dependencies requested a system change"); });

            var missing = new SetupPrerequisites(false, true, false, true);
            var detected = missing;
            Ensure(() => detected, step =>
            {
                if (step != SetupDependency.VisualCpp) throw new InvalidOperationException("Unexpected dependency");
                detected = ready;
                return 0;
            });

            CheckBlocked(() => Ensure(() => missing, _ => 0), "could not be installed");
            CheckBlocked(() => Ensure(() => missing, _ => 1603), "1603");
            CheckBlocked(() => Ensure(() => missing, _ => 3010), "Restart Windows");
            CheckBlocked(() => Ensure(() => new SetupPrerequisites(true, false, true, true), step =>
            {
                if (step != SetupDependency.WindowsMedia) throw new InvalidOperationException("Unexpected dependency");
                return 0;
            }), "Restart Windows");
            CheckBlocked(() => Ensure(() => new SetupPrerequisites(true, false, true, true), _ => 87), "Media Feature Pack");
            CheckBlocked(() => Ensure(() => new SetupPrerequisites(false, false, false, true),
                _ => { throw new InvalidOperationException("Servicing on a non-N edition"); }), "Windows media features are missing");
            CheckBlocked(() => Ensure(() => missing,
                _ => { throw new InvalidOperationException("Permission was declined"); }), "declined");
        }

        private static void CheckBlocked(Action check, string expected)
        {
            try { check(); }
            catch (Exception error)
            {
                if (error.Message.Contains(expected)) return;
                throw;
            }
            throw new InvalidOperationException("Prerequisite unexpectedly passed: " + expected);
        }
#endif

        private static string Code(int code)
        {
            return code < 0 ? "0x" + unchecked((uint)code).ToString("X8") : code.ToString();
        }

        private static int RunVisualCpp(string folder, Assembly assembly)
        {
            string path = Path.Combine(folder, "vc_redist.x64.exe");
            using (var payload = assembly.GetManifestResourceStream(VcResource))
            using (var hash = SHA256.Create())
            {
                if (payload == null) throw new InvalidOperationException("The installer is missing its Microsoft Visual C++ package.");
                var expected = hash.ComputeHash(payload);
                payload.Position = 0;
                using (var file = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                    payload.CopyTo(file);
                using (var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read))
                {
                    if (!expected.SequenceEqual(hash.ComputeHash(file)))
                        throw new InvalidOperationException("The Visual C++ package could not be verified.");
                    return RunElevated(path, "/install /quiet /norestart");
                }
            }
        }

        private static int RunElevated(string path, string args)
        {
            var start = new ProcessStartInfo(path, args) {
                UseShellExecute = true, Verb = "runas", WorkingDirectory = SystemDir,
                WindowStyle = ProcessWindowStyle.Hidden
            };
            using (var process = Process.Start(start))
            {
                if (process == null) throw new InvalidOperationException("Windows could not start " + Path.GetFileName(path) + ".");
                process.WaitForExit();
                return process.ExitCode;
            }
        }
    }
}
