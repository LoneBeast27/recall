using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;

namespace Recall.Setup.Tests
{
    /// An update's quit never saves the replay buffer (SetupUpdateQuit.cs). Runs in this process
    /// against an in-memory recorder: no process, pipe, window or file.
    internal static partial class TestRunner
    {
        /// The recorder's pipe as an update sees it, like tests/fake/FakeRecall.cs: it records every
        /// line, reports its long recording in its status, and saves that recording a number of
        /// status polls after `record_stop` (never, when negative).
        private sealed class MemoryRecorder
        {
            public readonly List<string> Lines = new List<string>();
            public bool LongOn;
            public bool Starting;
            public int PollsToSave;
            public bool Alive = true;
            public bool Answers = true;
            private bool stopping;
            /// The status poll that first showed the long recording saved, and the line count then.
            public int SavedAtLine = -1;
            public DateTime Clock = new DateTime(2026, 11, 19, 12, 0, 0, DateTimeKind.Utc);

            public bool Send(string line)
            {
                if (!Alive) return false;
                Lines.Add(line);
                if (line.Contains("\"record_stop\"") && (LongOn || Starting)) stopping = true;
                if (line.Contains("\"quit\"")) Alive = false;
                return true;
            }

            public RecorderStatus Status()
            {
                if (!Alive || !Answers) return null;
                if (stopping && PollsToSave >= 0 && PollsToSave-- == 0)
                {
                    LongOn = Starting = false;
                    stopping = false;
                    SavedAtLine = Lines.Count;
                }
                return new RecorderStatus { Capturing = true, LongActive = LongOn, LongStarting = Starting, LongPath = LongOn ? @"V:\Clips\rec.mp4" : null };
            }

            public UpdateQuit Quit(List<string> log)
            {
                return new UpdateQuit(Send, Status, () => Alive, () => Clock, ms => Clock = Clock.AddMilliseconds(ms), log.Add);
            }
        }

        private static void CheckNoBufferSave(MemoryRecorder r)
        {
            var all = string.Join(" | ", r.Lines.ToArray());
            Check(r.Lines.All(l => !l.Contains("save_clip") && !l.Contains("save_first")), "no buffer save reached the recorder: " + all);
        }

        private static void UpdateQuitLongRecordingSavedFirst()
        {
            var r = new MemoryRecorder { LongOn = true, PollsToSave = 3 };
            var log = new List<string>();
            var q = r.Quit(log);
            Check(q.Run(UpdateQuit.LongOn(r.Status())), "the quit was sent");
            CheckNoBufferSave(r);
            Check(r.Lines.SequenceEqual(new[] { UpdateQuit.RecordStopLine, RecorderQuitter.QuitLine }), "record_stop, then a plain quit: " + string.Join(" | ", r.Lines.ToArray()));
            Check(r.SavedAtLine == 1, "the quit came only after the status showed the recording saved");
            Check(q.StoppedLong, "the restore starts the recording again");
        }

        private static void UpdateQuitPlainWithoutLongRecording()
        {
            var r = new MemoryRecorder();
            var q = r.Quit(new List<string>());
            Check(q.Run(UpdateQuit.LongOn(r.Status())), "the quit was sent");
            CheckNoBufferSave(r);
            Check(r.Lines.SequenceEqual(new[] { RecorderQuitter.QuitLine }), "only a plain quit: " + string.Join(" | ", r.Lines.ToArray()));
            Check(!q.StoppedLong, "nothing to start again");
        }

        private static void UpdateQuitStartingRecordingIsStopped()
        {
            var r = new MemoryRecorder { Starting = true, PollsToSave = 1 };
            var q = r.Quit(new List<string>());
            Check(UpdateQuit.LongOn(r.Status()) && q.Run(true), "a starting recording counts as on");
            CheckNoBufferSave(r);
            Check(r.Lines.SequenceEqual(new[] { UpdateQuit.RecordStopLine, RecorderQuitter.QuitLine }), "record_stop, then a plain quit");
        }

        private static void UpdateQuitStopWaitIsBounded()
        {
            var r = new MemoryRecorder { LongOn = true, PollsToSave = -1 };
            var log = new List<string>();
            var start = r.Clock;
            Check(r.Quit(log).Run(true), "the quit is still sent");
            CheckNoBufferSave(r);
            Check(r.Lines.SequenceEqual(new[] { UpdateQuit.RecordStopLine, RecorderQuitter.QuitLine }), "record_stop, then a plain quit");
            var waited = r.Clock - start;
            Check(waited >= UpdateQuit.LongStopWait && waited < UpdateQuit.LongStopWait.Add(TimeSpan.FromSeconds(2)), "waited the bound, no longer: " + waited);
            Check(log.Any(l => l.StartsWith("WARNING")), "the give-up is logged");

            var silent = new MemoryRecorder { LongOn = true, Answers = false };
            Check(silent.Quit(new List<string>()).Run(true), "no status answer: the wait is bounded too");
            Check(silent.Lines.Last() == RecorderQuitter.QuitLine, "plain quit last");
        }

        private static void UpdateQuitRecorderGoneDuringStop()
        {
            var r = new MemoryRecorder { LongOn = true, PollsToSave = -1 };
            var start = r.Clock;
            var q = new UpdateQuit(line => { var ok = r.Send(line); if (line == UpdateQuit.RecordStopLine) r.Alive = false; return ok; },
                r.Status, () => r.Alive, () => r.Clock, ms => r.Clock = r.Clock.AddMilliseconds(ms), _ => { });
            Check(!q.Run(true), "a recorder that exited gets no quit");
            Check(r.Clock == start, "no wait for a recorder that is gone");
            CheckNoBufferSave(r);
        }

        private static void UpdateQuitStatusParse()
        {
            var s = RecorderStatus.Parse("{\"mod\":\"session\",\"event\":{\"type\":\"status\",\"state\":\"recording\",\"long_recording\":{\"active\":false,\"starting\":true,\"path\":null}}}");
            Check(s != null && s.LongStarting && !s.LongActive && UpdateQuit.LongOn(s), "starting parses and counts as on");
            var idle = RecorderStatus.Parse("{\"mod\":\"session\",\"event\":{\"type\":\"status\",\"state\":\"recording\",\"long_recording\":{\"active\":false,\"path\":null}}}");
            Check(idle != null && !UpdateQuit.LongOn(idle), "idle is off");
            Check(!UpdateQuit.LongOn(null), "no status is off");
        }

        /// No shipped Setup source asks the recorder to save the replay buffer.
        private static void NoBufferSaveInSetup()
        {
            Check(sourceDir != null && Directory.Exists(sourceDir), "build.ps1 -Test passes the source folder");
            int files = 0;
            foreach (var file in Directory.GetFiles(sourceDir, "*.cs"))
            {
                files++;
                var text = File.ReadAllText(file);
                foreach (var word in new[] { "save_first", "save_clip" })
                    Check(text.IndexOf(word, StringComparison.OrdinalIgnoreCase) < 0, Path.GetFileName(file) + " mentions " + word);
            }
            Check(files >= 10, "read the shipped sources (" + files + ")");
        }
    }
}
