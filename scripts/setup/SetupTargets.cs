using System;
using System.IO;

namespace Recall.Setup
{
    /// Everything Setup touches outside the install folder: the Start menu folder, the HKCU keys
    /// (Apps & Features entry, Run value) and the recorder's control pipes. A test run sets
    /// RECALL_SETUP_SANDBOX_ROOT: then all of it moves under that folder, under
    /// HKCU\Software\RecallSandbox, and onto sandbox pipe names, so it never reaches the real
    /// shortcut, the real keys or the live recorder.
    internal sealed class SetupTargets
    {
        public const string SandboxVariable = "RECALL_SETUP_SANDBOX_ROOT";
        public const string ControllerVariable = "RECALL_CONTROLLER_PATH";
        public const string LegacyPipeVariable = "RECALL_LEGACY_PIPE";
        public const string PipeVariable = "RECALL_SETUP_PIPE";
        public const string RealRegistryBase = @"Software\Microsoft\Windows\CurrentVersion";
        public const string SandboxRegistryBase = @"Software\RecallSandbox\CurrentVersion";

        public readonly string SandboxRoot;
        public readonly string RegistryBase;
        public readonly string StartMenuPrograms;
        public readonly string SessionPipe;
        public readonly string LegacyPipe;
        public readonly string ControllerOverride;
        public readonly int Session;

        private SetupTargets(string sandbox, string controller, int session, string pipe, string legacy)
        {
            Session = session;
            SandboxRoot = sandbox;
            ControllerOverride = controller;
            if (sandbox == null)
            {
                RegistryBase = RealRegistryBase;
                StartMenuPrograms = Environment.GetFolderPath(Environment.SpecialFolder.Programs);
                // The recorder's control pipe is per session (recall-hdr control_pipe.rs); a recorder
                // from before that listens on the bare name.
                SessionPipe = "recall-hdr-control-" + session;
                LegacyPipe = "recall-hdr-control";
            }
            else
            {
                RegistryBase = SandboxRegistryBase;
                StartMenuPrograms = Path.Combine(sandbox, @"StartMenu\Programs");
                SessionPipe = string.IsNullOrEmpty(pipe) ? "recall-sandbox-control-" + session : pipe;
                LegacyPipe = string.IsNullOrEmpty(legacy) ? "recall-sandbox-control" : legacy;
            }
        }

        public bool IsSandbox { get { return SandboxRoot != null; } }
        public string UninstallKey { get { return RegistryBase + @"\Uninstall\Recall"; } }
        public string RunKey { get { return RegistryBase + @"\Run"; } }
        public string Shortcut { get { return Path.Combine(StartMenuPrograms, "Recall.lnk"); } }

        /// The real per-user install folder (%LOCALAPPDATA%\Programs\Recall).
        public static string DefaultInstallDir
        {
            get { return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Programs", "Recall"); }
        }

        public static SetupTargets FromEnvironment()
        {
            var sandbox = Environment.GetEnvironmentVariable(SandboxVariable);
            var controller = Environment.GetEnvironmentVariable(ControllerVariable);
            if (string.IsNullOrEmpty(sandbox)) sandbox = null;
            if (string.IsNullOrEmpty(controller)) controller = null;
            if (controller != null && sandbox == null)
                throw new InvalidOperationException(ControllerVariable + " is set without " + SandboxVariable
                    + ": a sandboxed run must name its sandbox folder. Nothing was closed or replaced.");
            if (sandbox != null) sandbox = PathId.Normalize(Path.GetFullPath(sandbox));
            if (controller != null) controller = PathId.Normalize(Path.GetFullPath(controller));
            return new SetupTargets(sandbox, controller, SessionProcesses.CurrentSession,
                Environment.GetEnvironmentVariable(PipeVariable), Environment.GetEnvironmentVariable(LegacyPipeVariable));
        }

        /// The controller this run may quit and talk to: the install folder's recall-hdr.exe, or
        /// the sandbox's RECALL_CONTROLLER_PATH.
        public string ControllerPath(string installDir)
        {
            return PathId.Normalize(ControllerOverride ?? Path.Combine(installDir, "recall-hdr.exe"));
        }

        /// Refuses before anything is closed or replaced (see SandboxPolicy.Refusal).
        public void CheckInstallDir(string installDir)
        {
            var why = SandboxPolicy.Refusal(PathId.Root(installDir), SandboxRoot == null ? null : PathId.Root(SandboxRoot),
                PathId.Root(DefaultInstallDir), ControllerOverride == null ? null : PathId.Normalize(ControllerOverride));
            if (why != null) throw new InvalidOperationException(why);
        }
    }

    /// Pure checks on normalized roots (each ending in '\').
    internal static class SandboxPolicy
    {
        public static string Refusal(string installRoot, string sandboxRoot, string defaultRoot, string controller)
        {
            if (sandboxRoot == null) return null;
            // A sandbox must stay inside its own folder, and never name the real install folder or a
            // folder holding it (%LOCALAPPDATA%\Programs): the app close, the wait and the swap act
            // on everything under the install folder.
            if (!PathId.IsInside(installRoot, sandboxRoot))
                return "A sandboxed Setup (" + SetupTargets.SandboxVariable + ") must install inside " + sandboxRoot
                    + "; " + installRoot + " is not. Nothing was closed or replaced.";
            if (PathId.IsInside(defaultRoot, installRoot) || PathId.IsInside(defaultRoot, sandboxRoot))
                return "A sandboxed Setup can't use the real install folder or a folder that holds it (" + installRoot
                    + "). Nothing was closed or replaced.";
            if (controller != null && !PathId.IsInside(controller, installRoot))
                return SetupTargets.ControllerVariable + " (" + controller + ") is not inside the install folder ("
                    + installRoot + "). Nothing was closed or replaced.";
            return null;
        }
    }
}
