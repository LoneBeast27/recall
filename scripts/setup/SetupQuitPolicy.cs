using System;

namespace Recall.Setup
{
    /// The recorder that got a quit (or is already quitting): its pid, start time and the pipe it listens on.
    internal sealed class QuitTarget
    {
        public readonly int Pid;
        public readonly long Created;
        public readonly string Pipe;

        public QuitTarget(int pid, long created, string pipe)
        {
            Pid = pid; Created = created; Pipe = pipe;
        }
    }

    internal enum QuitStep { Exited, Wait, Timeout }

    internal enum QuitAction { SendQuit, JoinSave }

    /// The decisions behind quitting the recorder, with no I/O, so they are unit-tested
    /// (scripts/setup/tests). Same decisions as the dev installer scripts.
    internal static class QuitPolicy
    {
        /// The engine's "another engine already records" exit code (recall-hdr engine_policy.rs
        /// EXIT_SLOT_BUSY): a recorder that exits with it stepped aside for one already recording.
        public const int SlotBusyExit = 86;

        /// Null when a line may be written to the pipe's server, else why not. Only a server whose
        /// pid is one of this run's own controllers (exe = the install folder's recall-hdr.exe)
        /// and that runs as this account gets anything.
        public static string MayWrite(int serverPid, string serverSid, string mySid, bool serverIsOurController)
        {
            if (serverPid == 0) return "the pipe's server is unknown";
            if (serverSid == null) return "can't tell which account runs the pipe";
            if (!string.Equals(serverSid, mySid, StringComparison.OrdinalIgnoreCase)) return "another account runs the pipe";
            if (!serverIsOurController) return "the pipe's server is not this installation's recorder";
            return null;
        }

        /// One of ours? Compared by what the two paths name, case-insensitively (normalized by the caller).
        public static bool IsOurController(string normalizedExe, string controllerPath)
        {
            return normalizedExe != null && controllerPath != null
                && string.Equals(normalizedExe.TrimEnd('\\'), controllerPath.TrimEnd('\\'), StringComparison.OrdinalIgnoreCase);
        }

        /// A controller of ours that already says finishing_clip got its quit from an earlier run: a
        /// second quit could force it and lose the clip, so the run joins that wait. One quitting
        /// only for a held sign-out (quit_for_sign_out) is not pending: it gets one plain quit (a
        /// held sign-out's quit is called off if the sign-out is cancelled; ours outlives that).
        public static QuitAction OnStatus(RecorderStatus status)
        {
            if (status == null) return QuitAction.SendQuit;
            if (status.QuitForSignOut) return QuitAction.SendQuit;
            return status.FinishingClip ? QuitAction.JoinSave : QuitAction.SendQuit;
        }

        /// What a just-started recorder's state means. Null = it runs; otherwise a note for the
        /// user. Exit 86 is not a failure: another recorder already holds the engine slot.
        public static string StartOutcome(bool exited, int exitCode, bool answered)
        {
            if (!exited) return answered ? null : "The recorder did not answer on its control pipe; start Recall from the Start menu.";
            if (exitCode == SlotBusyExit)
                return "Another Recall recorder is already recording, so the new one stepped aside (exit 86). Quit Recall from the tray and start it again to use this version.";
            return "The recorder exited (code " + exitCode + "). Start Recall from the Start menu.";
        }

        /// Start with Windows is opt-in: only an explicit choice on a new install sends the setting;
        /// an update never turns it on.
        public static bool SendAutostart(bool optedIn, bool update, bool recorderAnswered)
        {
            return optedIn && !update && recorderAnswered;
        }
    }

    /// The long wait's state: 2 min of grace once the recorder is not finishing a clip (or not
    /// answering), and an optional cap counted from the start. No I/O: one decision per poll.
    internal sealed class QuitWait
    {
        public static readonly TimeSpan Grace = TimeSpan.FromMinutes(2);
        public readonly DateTime Since;
        public readonly int CapMinutes;
        private readonly DateTime? cap;
        private DateTime? graceFrom;
        public bool Silent;
        public string Why;

        public QuitWait(DateTime since, int capMinutes)
        {
            Since = since;
            CapMinutes = capMinutes;
            cap = capMinutes > 0 ? since.AddMinutes(capMinutes) : (DateTime?)null;
        }

        /// <paramref name="finishing"/>: true / false from the status, null = no answer.
        public QuitStep Step(DateTime now, bool alive, bool? finishing)
        {
            if (!alive) return QuitStep.Exited;
            if (finishing == true) graceFrom = null;
            else if (graceFrom == null) graceFrom = now;
            if (finishing != true) Silent = finishing == null;
            if (cap.HasValue && now >= cap.Value) { Why = "cap"; return QuitStep.Timeout; }
            if (graceFrom.HasValue && now - graceFrom.Value >= Grace) { Why = "grace"; return QuitStep.Timeout; }
            return QuitStep.Wait;
        }

        public string TimeoutMessage(int pid)
        {
            if (Why == "cap")
                return "Recall (pid " + pid + ") got the quit but still runs after " + CapMinutes
                    + " minutes; it keeps saving and then quits. Nothing was replaced: run Setup again once it has quit "
                    + "(Setup sends no second quit while it saves).";
            return "Recall (pid " + pid + ") got the quit but has not exited, and for 2 min it "
                + (Silent ? "has not answered on its pipe" : "says it is not finishing a clip")
                + ". Nothing was replaced. Quit Recall from the tray, then run Setup again.";
        }

        public static string Progress(int pid, DateTime since, DateTime now, RecorderStatus s)
        {
            int minutes = (int)Math.Floor((now - since).TotalMinutes);
            if (s != null && s.FinishingClip)
            {
                string what = s.Bytes > 0 ? Math.Round(s.Bytes / 1048576.0) + " MB written"
                    : s.Saves == 1 ? "1 clip still being written"
                    : s.Saves > 1 ? s.Saves + " clips still being written" : "still being written";
                return "Recall is finishing a clip (" + what + ", " + minutes + " min so far); waiting.";
            }
            return "Waiting for Recall (pid " + pid + ") to exit (" + minutes + " min so far)…";
        }
    }
}
