using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Threading;

namespace Recall.Setup
{
    /// The installed Recall app is open and this run may not close it (a first install).
    internal sealed class AppStillOpen : InvalidOperationException
    {
        public AppStillOpen(string message) : base(message) { }
    }

    /// Quits the recorder of one installation before its files are replaced or removed, and
    /// waits for it. Only this Windows session counts, and only the controller running from the
    /// selected installation (or the sandbox's RECALL_CONTROLLER_PATH) is asked to quit, over a
    /// pipe it serves itself as this account; other recorders are named and left alone. One quit
    /// at most: a recorder already finishing a clip is joined, never asked again. Port of
    /// the dev installer's app check, quit and wait.
    internal sealed class RecorderQuitter
    {
        private readonly SetupTargets targets;
        private readonly string installRoot;
        private readonly string controllerPath;
        private readonly bool closeApp;
        private readonly Action<string> log;
        private readonly int self = Process.GetCurrentProcess().Id;
        public readonly RecorderPipe Pipe;
        /// An update's quit: the recorder saves what it records first (recall-hdr record_quit_save.rs).
        public bool SaveFirst;

        public const string QuitLine = "{\"cmd\":\"quit\"}";
        public const string SaveFirstQuitLine = "{\"cmd\":\"quit\",\"save_first\":true}";

        public RecorderQuitter(SetupTargets targets, string installDir, bool closeApp, Action<string> log)
        {
            this.targets = targets;
            this.closeApp = closeApp;
            this.log = log;
            installRoot = PathId.Root(installDir);
            controllerPath = targets.ControllerPath(installDir);
            Pipe = new RecorderPipe(targets, OurControllers, log);
        }

        /// A process listing, tried 3 times; when all 3 fail, the fallback (a hiccup never ends the
        /// run mid-wait).
        private List<SessionProcess> Snapshot()
        {
            for (int attempt = 1; ; attempt++)
            {
                try { return SessionProcesses.List(targets.Session); }
                catch (Exception error)
                {
                    if (attempt >= 3 || !(error is InvalidOperationException || error is System.ComponentModel.Win32Exception)) throw;
                    Thread.Sleep(500);
                }
            }
        }

        private bool InInstall(SessionProcess p)
        {
            return p.Pid != self && p.Path != null && PathId.IsInside(PathId.Normalize(p.Path), installRoot);
        }

        private static bool Named(SessionProcess p, string name)
        {
            return string.Equals(p.Name, name, StringComparison.OrdinalIgnoreCase);
        }

        /// Every process of this session running from the install folder.
        public List<SessionProcess> Installed() { return Snapshot().FindAll(InInstall); }

        public List<SessionProcess> InstalledApp() { return Snapshot().FindAll(p => Named(p, "recall-native.exe") && InInstall(p)); }

        public List<SessionProcess> OurControllers()
        {
            return Snapshot().FindAll(p => Named(p, "recall-hdr.exe") && p.Path != null
                && QuitPolicy.IsOurController(PathId.Normalize(p.Path), controllerPath));
        }

        public List<SessionProcess> OtherControllers()
        {
            return Snapshot().FindAll(p => Named(p, "recall-hdr.exe")
                && !(p.Path != null && QuitPolicy.IsOurController(PathId.Normalize(p.Path), controllerPath)));
        }

        /// Never close the app under the user: refuse while the installed app runs, unless this run
        /// was told it may close it (an update or uninstall), which asks each window to close the way
        /// its own X button does; the wait below covers it.
        public void EnsureAppClosed()
        {
            var app = InstalledApp();
            if (app.Count == 0) return;
            if (!closeApp)
                throw new AppStillOpen("Recall is open (" + SessionProcesses.Describe(app)
                    + "). Close the Recall window, then run setup again.");
            foreach (var p in app)
            {
                try { using (var process = Process.GetProcessById(p.Pid)) process.CloseMainWindow(); }
                catch (ArgumentException) { }
                catch (InvalidOperationException) { }
            }
        }

        public void Stop(int capMinutes)
        {
            Pipe.OwnerRefused = false;
            Pipe.SentTo = null;
            var others = OtherControllers();
            if (others.Count > 0) log("Left running (not " + controllerPath + "): " + SessionProcesses.Describe(others));
            var quit = RequestQuit();
            WaitForQuit(quit, capMinutes);
            var left = Installed();
            foreach (var c in OurControllers()) if (!left.Exists(p => p.Pid == c.Pid)) left.Add(c);
            if (left.Count > 0)
                throw new InvalidOperationException("Recall is still running (" + SessionProcesses.Describe(left)
                    + "). Quit it from the tray, then run Setup again.");
        }

        /// Asks the recorder to quit, once: none is sent while one is already finishing a clip.
        /// Returns the recorder to wait on, else null.
        public QuitTarget RequestQuit()
        {
            var pending = PendingQuit();
            if (pending != null)
            {
                log("Recall (pid " + pending.Pid + ") is already quitting and finishing a clip (an earlier run asked); no second quit sent, waiting for it.");
                return pending;
            }
            bool sent = false;
            if (OurControllers().Count > 0)
            {
                // Either pipe answering counts: this build's session pipe first, then the old bare name.
                var line = SaveFirst ? SaveFirstQuitLine : QuitLine;
                sent = Pipe.Send(line);
                if (!sent && OurControllers().Count > 0) sent = Pipe.Send(line, targets.LegacyPipe, 5);
            }
            return sent ? Pipe.SentTo : null;
        }

        /// The status of this installation's recorder (this account's, on this session's pipe), or null.
        public RecorderStatus ReadOurStatus()
        {
            foreach (var c in OurControllers())
            {
                if (RecorderPipe.OwnerSid(c.Pid) != Pipe.MySid) continue;
                var status = Pipe.ReadStatus(new QuitTarget(c.Pid, c.Created, targets.SessionPipe));
                if (status != null) return status;
            }
            return null;
        }

        private QuitTarget PendingQuit()
        {
            foreach (var c in OurControllers())
            {
                if (RecorderPipe.OwnerSid(c.Pid) != Pipe.MySid) continue;
                var target = new QuitTarget(c.Pid, c.Created, targets.SessionPipe);
                if (QuitPolicy.OnStatus(Pipe.ReadStatus(target)) == QuitAction.JoinSave) return target;
            }
            return null;
        }

        private bool AnyLeft()
        {
            try { return Installed().Count > 0 || OurControllers().Count > 0; }
            catch (Exception) { return true; } // a failing query counts as yes
        }

        /// 8 s for everything this run quits; then, only when a quit reached a recorder, while THAT
        /// recorder runs AND its status (read every 2 s) says finishing_clip, with 2 min of grace once
        /// it does not (or stops answering), one line a minute, and the cap; past either limit, an
        /// error naming the pid. Then 8 s more for its engine and overlay. Never a forced quit.
        public void WaitForQuit(QuitTarget quit, int capMinutes)
        {
            var since = DateTime.Now;
            var deadline = since.AddSeconds(8);
            while (DateTime.Now < deadline && AnyLeft()) Thread.Sleep(300);
            if (quit == null || !SessionProcesses.Alive(quit.Pid, quit.Created)) return;
            var wait = new QuitWait(since, capMinutes);
            DateTime next = DateTime.Now, poll = DateTime.Now;
            RecorderStatus status = null;
            while (true)
            {
                if (DateTime.Now >= poll) { status = Pipe.ReadStatus(quit); poll = DateTime.Now.AddSeconds(2); }
                bool? finishing = status == null ? (bool?)null : status.FinishingClip;
                var step = wait.Step(DateTime.Now, SessionProcesses.Alive(quit.Pid, quit.Created), finishing);
                if (step == QuitStep.Exited) break;
                if (step == QuitStep.Timeout) throw new InvalidOperationException(wait.TimeoutMessage(quit.Pid));
                if (DateTime.Now >= next) { log(QuitWait.Progress(quit.Pid, since, DateTime.Now, status)); next = DateTime.Now.AddMinutes(1); }
                Thread.Sleep(1000);
            }
            deadline = DateTime.Now.AddSeconds(8);
            while (DateTime.Now < deadline && AnyLeft()) Thread.Sleep(300);
        }
    }
}
