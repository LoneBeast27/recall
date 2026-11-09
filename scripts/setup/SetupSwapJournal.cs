using System;
using System.Collections.Generic;
using System.IO;
using System.Text;

namespace Recall.Setup
{
    /// `&lt;dir&gt;\.swap-journal`: what the swap has moved, written (write-through) BEFORE each move, so a
    /// swap cut off at any point (a crash, a power cut, a throw) can be put back to all-old:
    ///   m &lt;name&gt;    the installed file is about to move into .old\
    ///   n1 &lt;name&gt;   the new copy is about to move in over a file that existed
    ///   n0 &lt;name&gt;   the new copy is about to move in where no file existed
    /// Rolling back reads only these lines: a stale file left in .old\ by an earlier update is never
    /// moved back over a file this swap did not move. Recall (recall-native swap_journal.rs) reads the
    /// same lines.
    internal sealed class SwapJournal : IDisposable
    {
        public const string Name = ".swap-journal";
        public const string Header = "recall-swap-journal 1";
        private readonly FileStream stream;

        private SwapJournal(FileStream stream) { this.stream = stream; }

        public static string PathIn(string dir) { return System.IO.Path.Combine(dir, Name); }

        /// A new journal (an old one must have been recovered first).
        public static SwapJournal Create(string dir)
        {
            var stream = new FileStream(PathIn(dir), FileMode.CreateNew, FileAccess.Write, FileShare.Read, 4096, FileOptions.WriteThrough);
            var journal = new SwapJournal(stream);
            journal.Write(Header);
            return journal;
        }

        public void Moving(string name) { Write("m " + name); }

        public void Placing(string name, bool had) { Write((had ? "n1 " : "n0 ") + name); }

        private void Write(string line)
        {
            var bytes = new UTF8Encoding(false).GetBytes(line + "\n");
            stream.Write(bytes, 0, bytes.Length);
            stream.Flush(true);
        }

        public void Dispose() { stream.Dispose(); }

        /// One file the journal names: was it moved to .old\, was its new copy (maybe) placed.
        internal sealed class Entry
        {
            public string Name;
            public bool Moved;
            public bool Placed;
            public bool Had;
        }

        /// The entries in the order they were first written; null when the file is not a journal.
        public static List<Entry> Read(string path)
        {
            string[] lines;
            using (var reader = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite))
            using (var text = new StreamReader(reader, new UTF8Encoding(false)))
                lines = text.ReadToEnd().Split('\n');
            if (lines.Length == 0 || lines[0].Trim() != Header) return null;
            var order = new List<Entry>();
            var byName = new Dictionary<string, Entry>(StringComparer.OrdinalIgnoreCase);
            for (int i = 1; i < lines.Length; i++)
            {
                var line = lines[i].TrimEnd('\r');
                int space = line.IndexOf(' ');
                if (space <= 0) continue; // a line cut off mid-write names nothing
                var kind = line.Substring(0, space);
                var name = line.Substring(space + 1);
                if (name.Length == 0 || name.IndexOfAny(new[] { '\\', '/', ':' }) >= 0) continue;
                Entry entry;
                if (!byName.TryGetValue(name, out entry))
                {
                    entry = new Entry { Name = name };
                    byName[name] = entry;
                    order.Add(entry);
                }
                if (kind == "m") { entry.Moved = true; entry.Had = true; }
                else if (kind == "n1") { entry.Placed = true; entry.Had = true; }
                else if (kind == "n0") entry.Placed = true;
            }
            return order;
        }

        /// Puts every entry back to its old file, newest first, going on past a step that fails.
        /// Returns the failures (empty = the folder is all old again).
        public static List<string> RollBack(string dir, List<Entry> entries, Action<string, string> move)
        {
            var oldDir = System.IO.Path.Combine(dir, ".old");
            var failures = new List<string>();
            for (int i = entries.Count - 1; i >= 0; i--)
            {
                var e = entries[i];
                var target = System.IO.Path.Combine(dir, e.Name);
                var backup = System.IO.Path.Combine(oldDir, e.Name);
                try
                {
                    if (e.Had)
                    {
                        // Only a file this swap moved comes back; a target still in place was never moved.
                        if (e.Moved && File.Exists(backup) && (e.Placed || !File.Exists(target))) move(backup, target);
                    }
                    else if (e.Placed && File.Exists(target)) File.Delete(target);
                }
                catch (Exception error)
                {
                    failures.Add(e.Name + ": " + error.Message);
                }
            }
            return failures;
        }
    }
}
