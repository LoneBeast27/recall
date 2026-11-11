using System;
using System.Drawing;
using System.Globalization;
using System.Windows.Forms;

namespace Recall.Setup
{
    /// One scale for the whole window, applied from 96-DPI units (again after each DPI change).
    ///
    /// The layout is written in 96-DPI units (a 764 x 490 page). Setup picks one factor: the display
    /// scale, made smaller when the page would not fit the screen's work area. Every bound, font and
    /// painted shape uses that factor, so the page looks the same at 100 %, 125 %, 175 % or 300 %
    /// and on a 1366 x 768 laptop: nothing can grow past the window.
    ///
    /// WinForms' own AutoScaleMode.Dpi did not do this: it scaled the nested panels (quick setup,
    /// advanced, result) a second time, so above 100 % their content spilled past the right and
    /// bottom edges and over the footer buttons. It also left the fonts at the display scale even
    /// when the page had to shrink to fit.
    internal sealed partial class SetupWindow
    {
        internal const int PageWidth = 764;
        internal const int PageHeight = 490;

        /// Screen pixels per 96-DPI layout unit for this window.
        internal static float UiScale = 1f;

        /// The display scale, capped so the window (with its title bar and frame) fits the work area.
        internal static float ChooseScale(float displayScale, Size workArea, Size frame)
        {
            if (displayScale <= 0 || float.IsNaN(displayScale) || float.IsInfinity(displayScale)) displayScale = 1f;
            float margin = 24 * displayScale;
            float fitWidth = (workArea.Width - frame.Width - margin) / PageWidth;
            float fitHeight = (workArea.Height - frame.Height - margin) / PageHeight;
            float scale = Math.Min(displayScale, Math.Min(fitWidth, fitHeight));
            // A screen smaller than the page at half size cannot show it readably anyway.
            return Math.Max(0.5f, scale);
        }

        private readonly UnitLayout layout = new UnitLayout();
#if PREVIEW
        private string openingNote;
#endif

        /// The scale for the monitor the window opens on: the one under the cursor (where
        /// CenterScreen puts it), its own DPI, capped by its own work area.
        private float ScaleForThisScreen()
        {
            var cursor = Cursor.Position;
            int dpi = SetupDpi.ForMonitorAt(cursor);
#if PREVIEW
            openingNote = "cursor " + cursor + " monitor DPI " + dpi + " (" + SetupDpi.Mode + "), work " + Screen.FromPoint(cursor).WorkingArea
                + ", frame " + FrameAt(dpi) + ", screens: " + string.Join("; ", Array.ConvertAll(Screen.AllScreens,
                    m => m.Bounds + " at " + SetupDpi.ForMonitorAt(new Point(m.Bounds.Left + 1, m.Bounds.Top + 1))));
#endif
            return ScaleForMonitor(dpi / 96f, Screen.FromPoint(cursor).WorkingArea.Size, FrameAt(dpi));
        }

        /// This window's title bar and frame at a DPI (a fixed dialog's, from its own window styles).
        private Size FrameAt(int dpi)
        {
            var styles = CreateParams;
            return SetupDpi.Frame(styles.Style, styles.ExStyle, dpi);
        }

        /// ChooseScale, with the preview's stand-in display for the opening monitor
        /// (RECALL_SETUP_SCALE=1.75) and work area (RECALL_SETUP_WORKAREA=1366x728), so checks render
        /// any display on any PC. Simulated moves: SetupWindow.PreviewMove.cs.
        private static float ScaleForMonitor(float display, Size work, Size frame)
        {
#if PREVIEW
            float forced;
            var scaleText = Environment.GetEnvironmentVariable("RECALL_SETUP_SCALE");
            if (scaleText != null && float.TryParse(scaleText, NumberStyles.Float, CultureInfo.InvariantCulture, out forced))
            {
                display = forced;
                frame = new Size((int)Math.Round(6 * forced), (int)Math.Round(29 * forced));
            }
            var areaText = Environment.GetEnvironmentVariable("RECALL_SETUP_WORKAREA");
            if (areaText != null)
            {
                var parts = areaText.Split('x');
                work = new Size(int.Parse(parts[0], CultureInfo.InvariantCulture), int.Parse(parts[1], CultureInfo.InvariantCulture));
            }
#endif
            return ChooseScale(display, work, frame);
        }

        /// Layout units to pixels at UiScale (the first time, and after every DPI change).
        private void ApplyScale()
        {
            layout.Apply(this, UiScale);
            layoutScaled = true;
            ClientSize = UnitLayout.PageSize(UiScale);
        }
    }
}
