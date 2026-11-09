using System;
using System.IO;
using System.Windows.Forms;

namespace Recall.Setup
{
    /// Setup's command-line modes besides the window:
    ///   /uninstall [/quiet] [/remove-cache]           Apps & Features (UninstallString / QuietUninstallString)
    ///   /uninstall-from <dir> <pid> [/quiet] [/remove-cache]   the %TEMP% copy that does the work
    ///   /add-perflog-user <sid>                        the elevated copy started by the frame-timing checkbox
    ///   /sandbox-install <dir> [/update] [/launch] [/autostart] [/no-shortcut]
    ///                                                  headless install for tests; only with RECALL_SETUP_SANDBOX_ROOT
    internal static class SetupModes
    {
        public const string SandboxInstallArgument = "/sandbox-install";

        public static bool TryRun(string[] args, out int code)
        {
            code = 0;
#if UNINSTALLER
            if (args.Length == 0) { code = Uninstaller.Begin(false, false); return true; }
#endif
            if (args.Length == 0) return false;
            var mode = args[0].ToLowerInvariant();
            bool quiet = Has(args, Uninstaller.QuietArgument), removeCache = Has(args, Uninstaller.RemoveCacheArgument);
            if (mode == Uninstaller.Argument) { code = Uninstaller.Begin(quiet, removeCache); return true; }
            if (mode == Uninstaller.FromArgument && args.Length >= 3)
            {
                int pid;
                if (!int.TryParse(args[2], out pid)) pid = 0;
                code = Uninstaller.Run(args[1], pid, quiet, removeCache);
                return true;
            }
            if (mode == FrameTiming.ElevatedArgument && args.Length == 2) { code = FrameTiming.AddMember(args[1]); return true; }
            if (mode == SandboxInstallArgument && args.Length >= 2) { code = SandboxInstall(args); return true; }
            return false;
        }

        private static bool Has(string[] args, string flag)
        {
            return Array.Exists(args, a => a.Equals(flag, StringComparison.OrdinalIgnoreCase));
        }

        private static int SandboxInstall(string[] args)
        {
            Action<string> log = SetupLog.For(null);
            try
            {
                if (string.IsNullOrEmpty(Environment.GetEnvironmentVariable(SetupTargets.SandboxVariable)))
                {
                    MessageBox.Show(SandboxInstallArgument + " runs only with " + SetupTargets.SandboxVariable + " set.", "Recall Setup",
                        MessageBoxButtons.OK, MessageBoxIcon.Error);
                    return 2;
                }
                log = SetupLog.For(SetupTargets.FromEnvironment());
                var path = SetupPackage.InstallPath(args[1]);
                if (path == null) { log("Sandbox install: invalid folder " + args[1]); return 2; }
                var options = new InstallOptions(path, !Has(args, "/no-shortcut"), Has(args, "/launch"), false,
                    Has(args, "/update"), Has(args, "/autostart"));
                var warning = SetupPackage.Install(options, null);
                log("Sandbox install: OK" + (warning == null ? "" : " with a note: " + warning.Replace("\n", " | ")));
                return 0;
            }
            catch (AppStillOpen error) { log("Sandbox install: refused: " + error.Message); return 3; }
            catch (Exception error)
            {
                log("Sandbox install: FAILED: " + error.GetBaseException().Message);
                return 1;
            }
        }
    }
}
