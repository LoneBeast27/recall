using System;
using System.Collections.Generic;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Windows.Forms;

namespace Recall.Setup
{
    /// Moving to a monitor at another scale (Per-Monitor V2, SetupDpi.cs): Windows sends
    /// WM_DPICHANGED with the new DPI and a suggested window rectangle. Setup picks the scale for
    /// that monitor (its DPI, capped by its work area: ChooseScale), lays every control out again
    /// from its 96-DPI units, rebuilds the fonts at that scale and sizes the window to fit.
    /// Painted shapes read UiScale when they paint, so they follow on the next paint.
    internal sealed partial class SetupWindow
    {
#if PREVIEW
        private readonly List<string> realDpiChanges = new List<string>();
#endif
        private const uint SwpNoZOrder = 0x0004, SwpNoActivate = 0x0010;

        /// Set while a DPI change is being applied; a change Windows sends meanwhile (the resize can
        /// make it re-evaluate the monitor) waits here and is applied right after, so the last one wins.
        private bool applyingDpi;
        private int pendingDpi;
        private Rectangle pendingRect;

        protected override void WndProc(ref Message m)
        {
            if (m.Msg == SetupDpi.WmDpiChanged && layoutScaled)
            {
                int dpi = (int)((long)m.WParam & 0xFFFF);
                var suggested = ((SetupDpi.RECT)Marshal.PtrToStructure(m.LParam, typeof(SetupDpi.RECT))).ToRectangle();
#if PREVIEW
                realDpiChanges.Add("WM_DPICHANGED " + dpi + " DPI, suggested " + suggested + ", at scale " + UiScale + ", window " + Bounds
                    + (applyingDpi ? " (queued)" : "") + (PreviewForcesDisplay ? " (ignored: the preview forces the display)" : ""));
                if (PreviewForcesDisplay) { m.Result = IntPtr.Zero; return; }
#endif
                pendingDpi = dpi;
                pendingRect = suggested;
                if (!applyingDpi) ApplyPendingDpi();
                m.Result = IntPtr.Zero;
                return; // not base: WinForms' own DPI handling would scale a second time
            }
            base.WndProc(ref m);
        }

        private void ApplyPendingDpi()
        {
            applyingDpi = true;
            try
            {
                for (int round = 0; pendingDpi != 0 && round < 4; round++)
                {
                    int dpi = pendingDpi;
                    var suggested = pendingRect;
                    pendingDpi = 0;
                    try { MoveToDpi(dpi, suggested); }
                    catch (Exception error)
                    {
                        // Never lose the window over a layout problem: take Windows' rectangle as it is.
                        SetupLog.For(null)("WARNING: the move to " + dpi + " DPI could not lay Setup out again: " + error.Message);
                        SetupDpi.SetWindowPos(Handle, IntPtr.Zero, suggested.X, suggested.Y, suggested.Width, suggested.Height,
                            SwpNoZOrder | SwpNoActivate);
                    }
                }
                pendingDpi = 0;
            }
            finally { applyingDpi = false; }
        }

        /// WM_DPICHANGED: the scale for the monitor the suggested rectangle is on, capped by its work area.
        private void MoveToDpi(int dpi, Rectangle suggested)
        {
            var monitor = Screen.FromRectangle(suggested);
            float scale = ChooseScale(dpi / 96f, monitor.WorkingArea.Size, FrameAt(dpi));
            MoveToScale(scale, suggested, monitor.WorkingArea);
        }

        /// The rescale WM_DPICHANGED runs (and the preview's simulated move): controls from units,
        /// fonts rebuilt, then the window sized for the page at its top-left in the suggested rectangle.
        private void MoveToScale(float scale, Rectangle suggested, Rectangle work)
        {
            if (scale != UiScale)
            {
                SuspendLayout();
                UiScale = scale;
                RebuildFonts(scale);
                layout.Apply(this, scale);
                ResumeLayout(false);
            }
            PlaceWindow(UnitLayout.PageSize(scale), suggested.Location, work);
            Invalidate(true);
        }

        /// New fonts at the scale; every control that showed an old one gets its new twin.
        private void RebuildFonts(float scale)
        {
            var old = fonts;
            fonts = new SetupFonts(scale);
            var twins = new List<KeyValuePair<Font, Font>> {
                Twin(old.Title, fonts.Title), Twin(old.Mark, fonts.Mark), Twin(old.Body, fonts.Body),
                Twin(old.Strong, fonts.Strong), Twin(old.SystemText, fonts.SystemText)
            };
            SwapFonts(this, twins);
            old.Dispose();
#if PREVIEW
            retiredFonts.AddRange(new[] { old.Title, old.Mark, old.Body, old.Strong, old.SystemText });
#endif
        }

        private static KeyValuePair<Font, Font> Twin(Font old, Font now) { return new KeyValuePair<Font, Font>(old, now); }

        private static void SwapFonts(Control control, List<KeyValuePair<Font, Font>> twins)
        {
            // By reference: Font.Equals compares values, and two of Setup's fonts can be equal.
            foreach (var pair in twins)
            {
                if (!ReferenceEquals(control.Font, pair.Key)) continue;
                control.Font = pair.Value;
                break;
            }
            foreach (Control child in control.Controls) SwapFonts(child, twins);
        }

        /// The window at a client size: the frame estimated for its DPI, then corrected by what
        /// Windows reports, so the page is exactly UnitLayout.PageSize. Kept inside the work area.
        private void PlaceWindow(Size client, Point topLeft, Rectangle work)
        {
            if (!IsHandleCreated) { ClientSize = client; return; }
            var frame = SetupDpi.Frame(CreateParams.Style, CreateParams.ExStyle, SetupDpi.ForWindow(Handle, Bounds));
            var size = new Size(client.Width + frame.Width, client.Height + frame.Height);
            for (int pass = 0; pass < 2; pass++)
            {
                var at = Inside(new Rectangle(topLeft, size), work);
                SetupDpi.SetWindowPos(Handle, IntPtr.Zero, at.X, at.Y, size.Width, size.Height, SwpNoZOrder | SwpNoActivate);
                SetupDpi.RECT inner;
                if (!SetupDpi.GetClientRect(Handle, out inner) || inner.Size == client) break;
                size += new Size(client.Width - inner.Size.Width, client.Height - inner.Size.Height);
                topLeft = at.Location;
            }
        }

        /// A rectangle moved (not resized) to lie inside an area; top-left aligned when larger.
        internal static Rectangle Inside(Rectangle window, Rectangle area)
        {
            int x = Math.Max(area.Left, Math.Min(window.Left, area.Right - window.Width));
            int y = Math.Max(area.Top, Math.Min(window.Top, area.Bottom - window.Height));
            return new Rectangle(x, y, window.Width, window.Height);
        }

        /// The window was created on its monitor at that monitor's DPI, while WinForms sized it with
        /// the main monitor's frame: correct the client size, keeping the window centred.
        protected override void OnLoad(EventArgs args)
        {
            base.OnLoad(args);
            var page = UnitLayout.PageSize(UiScale);
            if (ClientSize == page) return;
            var bounds = Bounds;
            var centre = new Point(bounds.Left + bounds.Width / 2, bounds.Top + bounds.Height / 2);
            var work = Screen.FromRectangle(bounds).WorkingArea;
            var frame = new Size(bounds.Width - ClientSize.Width, bounds.Height - ClientSize.Height);
            PlaceWindow(page, new Point(centre.X - (page.Width + frame.Width) / 2, centre.Y - (page.Height + frame.Height) / 2), work);
        }
    }
}
