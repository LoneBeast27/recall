using System;

namespace Recall.Setup
{
    /// What an update sends to quit the recorder. An update never saves the replay buffer: the user
    /// did not ask for that clip. A long recording the user started is stopped first
    /// (`record_stop`), and Setup waits until the recorder's status shows no long recording any more
    /// (the recorder clears it on `recording_stopped`, once the file is saved), bounded by
    /// <see cref="LongStopWait"/>. Then a plain quit is sent.
    ///
    /// Setup never sets the quit's save-first flag: a recorder of 0.1.8 or older answers it by
    /// saving the replay buffer as a clip, and the update from those versions runs that recorder.
    /// Stopping the recording with its own command works the same for every recorder version.
    internal sealed class UpdateQuit
    {
        public const string RecordStopLine = "{\"cmd\":\"record_stop\"}";
        /// How long Setup waits for a stopped long recording to be saved before it quits anyway
        /// (the recorder's quit then ends it, and Setup recovers a `.part` it leaves). Tests shorten it.
        internal static TimeSpan LongStopWait = TimeSpan.FromMinutes(2);

        private readonly Func<string, bool> send;
        private readonly Func<RecorderStatus> status;
        private readonly Func<bool> alive;
        private readonly Func<DateTime> now;
        private readonly Action<int> sleep;
        private readonly Action<string> log;

        /// <paramref name="send"/>: one line to the recorder's pipe, true when it got it.
        /// <paramref name="status"/>: the recorder's status now, null without an answer.
        /// <paramref name="alive"/>: the recorder that got the stop still runs.
        public UpdateQuit(Func<string, bool> send, Func<RecorderStatus> status, Func<bool> alive,
            Func<DateTime> now, Action<int> sleep, Action<string> log)
        {
            this.send = send;
            this.status = status;
            this.alive = alive;
            this.now = now;
            this.sleep = sleep;
            this.log = log;
        }

        /// A long recording runs or is starting.
        public static bool LongOn(RecorderStatus s)
        {
            return s != null && (s.LongActive || s.LongStarting);
        }

        /// A `record_stop` was sent (the restore after the update starts the recording again).
        public bool StoppedLong { get; private set; }

        /// Stops and saves a long recording when <paramref name="longOn"/>, then sends the plain
        /// quit. Returns whether the quit reached the recorder.
        public bool Run(bool longOn)
        {
            if (longOn) StopLong();
            return send(RecorderQuitter.QuitLine);
        }

        private void StopLong()
        {
            if (!send(RecordStopLine))
            {
                log("WARNING: the long recording could not be stopped before the update; the quit ends it.");
                return;
            }
            StoppedLong = true;
            log("Update: stopping the long recording; waiting until it is saved.");
            var until = now() + LongStopWait;
            while (now() < until)
            {
                if (!alive()) return;
                var s = status();
                if (s != null && !LongOn(s)) { log("Update: the long recording is saved."); return; }
                sleep(500);
            }
            log("WARNING: the long recording was not saved within " + LongStopWait.TotalSeconds.ToString("0")
                + " s; the quit ends it and Setup recovers what it wrote.");
        }
    }
}
