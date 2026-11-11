using System;
using System.Collections.Generic;
using System.Drawing;
using System.Globalization;
using System.IO;
using System.Reflection;
using System.Windows.Forms;

namespace Recall.Setup
{
    /// The preview's simulated moves between monitors: RECALL_SETUP_DPI_MOVE=1.5,1.25 moves the
    /// window to a monitor at 1.5 (144 DPI), then to one at 1.25 (120 DPI), each through the same
    /// rescale WM_DPICHANGED runs (MoveToScale). After each step: every visible control inside its
    /// parent, every font rebuilt at the new scale, the old fonts disposed; a frame per step
    /// (<png>.move-1.png, .move-2.png ...). Then back to the start scale, where every pixel bound
    /// must be exactly as before, and through the sequence again so the remaining frames and checks
    /// run at the last scale.
    internal sealed partial class SetupWindow
    {
#if PREVIEW
        private readonly List<Font> retiredFonts = new List<Font>();

        /// The preview stands in for the display: real WM_DPICHANGED messages leave its scale alone.
        private static bool PreviewForcesDisplay
        {
            get
            {
                return Environment.GetEnvironmentVariable("RECALL_SETUP_SCALE") != null
                    || Environment.GetEnvironmentVariable("RECALL_SETUP_DPI_MOVE") != null
                    || Environment.GetEnvironmentVariable("RECALL_SETUP_WORKAREA") != null;
            }
        }

        private void SimulateMoves(string path)
        {
            var text = Environment.GetEnvironmentVariable("RECALL_SETUP_DPI_MOVE");
            if (text == null) return;
            var steps = Array.ConvertAll(text.Split(','), t => float.Parse(t.Trim(), NumberStyles.Float, CultureInfo.InvariantCulture));
            float start = UiScale;
            var before = CollectBounds(this, new List<Rectangle>());
            var log = new List<string> { "start " + start + " page " + ClientSize };
            for (int i = 0; i < steps.Length; i++)
            {
                MoveTo(PreviewScaleFor(steps[i]));
                CheckMoved(UiScale);
                log.Add("step " + (i + 1) + ": display " + steps[i] + " -> scale " + UiScale + " page " + ClientSize
                    + " title font " + fonts.Title.Size + " px, " + retiredFonts.Count + " old fonts disposed");
                SaveFrame(Path.ChangeExtension(path, ".move-" + (i + 1) + ".png"));
            }
            MoveTo(start);
            CheckMoved(start);
            var back = CollectBounds(this, new List<Rectangle>());
            for (int i = 0; i < before.Count; i++)
                if (before[i] != back[i]) throw new InvalidOperationException("Control " + i + " is " + back[i]
                    + " back at " + start + " after " + text + ", was " + before[i]);
            log.Add("back at " + start + ": all " + before.Count + " control bounds identical");
            foreach (var step in steps) { MoveTo(PreviewScaleFor(step)); CheckMoved(UiScale); }
            File.AppendAllText(path + ".dpi.txt", string.Join(Environment.NewLine, log) + Environment.NewLine);
        }

        /// ChooseScale for a stand-in monitor at this display scale (and RECALL_SETUP_WORKAREA).
        private float PreviewScaleFor(float display)
        {
            var frame = new Size((int)Math.Round(6 * display), (int)Math.Round(29 * display));
            var work = Screen.FromControl(this).WorkingArea.Size;
            var areaText = Environment.GetEnvironmentVariable("RECALL_SETUP_WORKAREA");
            if (areaText != null)
            {
                var parts = areaText.Split('x');
                work = new Size(int.Parse(parts[0], CultureInfo.InvariantCulture), int.Parse(parts[1], CultureInfo.InvariantCulture));
            }
            return ChooseScale(display, work, frame);
        }

        private void MoveTo(float scale)
        {
            MoveToScale(scale, Bounds, Screen.FromControl(this).WorkingArea);
            Application.DoEvents();
        }

        private static List<Rectangle> CollectBounds(Control parent, List<Rectangle> into)
        {
            foreach (Control child in parent.Controls) { into.Add(child.Bounds); CollectBounds(child, into); }
            return into;
        }

        /// The page at its size for the scale, everything inside it, and only fonts made at it.
        private void CheckMoved(float scale)
        {
            if (UiScale != scale || ClientSize != UnitLayout.PageSize(scale))
                throw new InvalidOperationException("At scale " + scale + " the page is " + ClientSize + " (scale " + UiScale + ")");
            CheckFits(this);
            CheckFonts(this, scale);
            var nativeFont = typeof(Font).GetField("nativeFont", BindingFlags.Instance | BindingFlags.NonPublic);
            foreach (var old in retiredFonts)
                if (nativeFont != null && (IntPtr)nativeFont.GetValue(old) != IntPtr.Zero)
                    throw new InvalidOperationException("A font from an earlier scale was not disposed: " + old);
        }

        /// Each control's font is one of this scale's (never an old one) at the pixel size its role has.
        private void CheckFonts(Control control, float scale)
        {
            var font = control.Font;
            float points = ReferenceEquals(font, fonts.Title) ? 29 : ReferenceEquals(font, fonts.Mark) ? 18
                : ReferenceEquals(font, fonts.Body) || ReferenceEquals(font, fonts.Strong) || ReferenceEquals(font, fonts.SystemText) ? 10.5f : 0;
            if (points == 0)
                throw new InvalidOperationException(control.GetType().Name + " '" + control.Text + "' kept a font from another scale");
            float expected = points * 96f / 72f * scale;
            if (Math.Abs(font.Size - expected) > 0.001f || font.Unit != GraphicsUnit.Pixel)
                throw new InvalidOperationException(control.GetType().Name + " '" + control.Text + "' font " + font.Size + " px, expected " + expected);
            foreach (var old in retiredFonts)
                if (ReferenceEquals(font, old)) throw new InvalidOperationException("Old font still shown on " + control.GetType().Name);
            foreach (Control child in control.Controls) CheckFonts(child, scale);
        }
#endif
    }
}
