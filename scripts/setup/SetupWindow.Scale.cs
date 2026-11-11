using System;
using System.Drawing;
using System.Globalization;
using System.Windows.Forms;

namespace Recall.Setup
{
    /// One scale for the whole window, applied once.
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

        private static float ScaleForThisScreen()
        {
            float display;
            using (var screen = Graphics.FromHwnd(IntPtr.Zero)) display = screen.DpiX / 96f;
            Size work = Screen.FromPoint(Cursor.Position).WorkingArea.Size;
            // Title bar and frame of a fixed dialog, at the display scale (Setup is DPI aware).
            var border = SystemInformation.FixedFrameBorderSize;
            var frame = new Size(border.Width * 2, SystemInformation.CaptionHeight + border.Height * 2);
#if PREVIEW
            // Checks render the page as a given display on any PC: RECALL_SETUP_SCALE=1.75,
            // RECALL_SETUP_WORKAREA=1366x728.
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

        /// Every control's bounds from layout units to pixels. Edges are rounded in window
        /// coordinates, so a child that ends at its parent's edge in units still does in pixels.
        private static void ScaleTree(Control parent, PointF parentUnits, Point parentPixels, float scale)
        {
            foreach (Control child in parent.Controls)
            {
                var b = child.Bounds;
                float x = parentUnits.X + b.Left, y = parentUnits.Y + b.Top;
                int left = (int)Math.Round(x * scale), top = (int)Math.Round(y * scale);
                int right = (int)Math.Round((x + b.Width) * scale), bottom = (int)Math.Round((y + b.Height) * scale);
                child.Bounds = new Rectangle(left - parentPixels.X, top - parentPixels.Y, right - left, bottom - top);
                ScaleTree(child, new PointF(x, y), new Point(left, top), scale);
            }
        }

        private void ApplyScale()
        {
            ScaleTree(this, PointF.Empty, Point.Empty, UiScale);
            layoutScaled = true;
            ClientSize = new Size((int)Math.Round(PageWidth * UiScale), (int)Math.Round(PageHeight * UiScale));
        }
    }
}
