using System.Drawing;
using System.Drawing.Drawing2D;
using System.Windows.Forms;

namespace Recall.Setup
{
    internal sealed partial class SetupWindow
    {
        private sealed class Rail : Panel
        {
            public int Step;
            public Rail() { BackColor = Color.FromArgb(3, 3, 3); DoubleBuffered = true; }

            protected override void OnPaint(PaintEventArgs args)
            {
                base.OnPaint(args);
                var g = args.Graphics;
                g.SmoothingMode = SmoothingMode.AntiAlias;
                // Drawn in 96-DPI units, scaled to the screen like the controls around it.
                float s = SetupWindow.Scale(g);
                g.ScaleTransform(s, s);
                float w = Width / s, h = Height / s;
                using (var border = new Pen(Border))
                using (var ring = new Pen(Ink, 1.5f))
                using (var muted = new Pen(Muted, 1.3f))
                using (var red = new SolidBrush(Signal))
                {
                    g.DrawLine(border, w - 1 / s, 0, w - 1 / s, h);
                    g.DrawEllipse(ring, 24, 34, 17, 17);
                    g.FillEllipse(red, 29, 39, 7, 7);
                    g.DrawLine(border, 30, 141, 30, 225);
                    for (int i = 0; i < 3; i++)
                    {
                        int y = 135 + i * 43;
                        g.FillEllipse(Brushes.Black, 24, y, 12, 12);
                        if (i == Step) g.FillEllipse(red, 25, y + 1, 10, 10);
                        else g.DrawEllipse(muted, 26, y + 2, 8, 8);
                    }
                }
            }
        }

        /// Screen pixels per 96-DPI unit (1 at 100 % scaling, 1.5 at 150 %).
        internal static float Scale(Graphics g) { return g.DpiX / 96f; }

        /// Text on the dark page: grayscale antialiasing. GDI+'s default (ClearType grid-fitting)
        /// draws the private fonts thin and jagged on #0A0A0A, the "pixelated" look; large display
        /// text skips grid-fitting so its curves stay smooth.
        internal static void Smooth(Graphics g, Font font)
        {
            g.TextRenderingHint = font.SizeInPoints >= 16 ? System.Drawing.Text.TextRenderingHint.AntiAlias
                : System.Drawing.Text.TextRenderingHint.AntiAliasGridFit;
        }

        private sealed class SmoothLabel : Label
        {
            protected override void OnPaint(PaintEventArgs args) { Smooth(args.Graphics, Font); base.OnPaint(args); }
        }

        private sealed class SmoothCheckBox : CheckBox
        {
            protected override void OnPaint(PaintEventArgs args) { Smooth(args.Graphics, Font); base.OnPaint(args); }
        }

        private sealed class SmoothRadioButton : RadioButton
        {
            protected override void OnPaint(PaintEventArgs args) { Smooth(args.Graphics, Font); base.OnPaint(args); }
        }

        /// A flat button whose disabled state stays readable on the dark window (WinForms draws a
        /// disabled flat button's text etched grey, nearly invisible on #121212).
        private sealed class FlatButton : Button
        {
            protected override void OnPaint(PaintEventArgs args)
            {
                Smooth(args.Graphics, Font);
                base.OnPaint(args);
                if (Enabled) return;
                var g = args.Graphics;
                using (var fill = new SolidBrush(Raised))
                using (var edge = new Pen(Border))
                using (var ink = new SolidBrush(Muted))
                using (var format = new StringFormat { Alignment = StringAlignment.Center, LineAlignment = StringAlignment.Center, FormatFlags = StringFormatFlags.NoWrap })
                {
                    g.FillRectangle(fill, ClientRectangle);
                    g.DrawRectangle(edge, 0, 0, Width - 1, Height - 1);
                    g.DrawString(Text, Font, ink, ClientRectangle, format);
                }
            }
        }

        /// A thin indeterminate bar: a red segment sweeping a dark track (the native marquee bar
        /// drew a light-grey trough that read as a stray mark on the dark page).
        private sealed class Activity : Control
        {
            private readonly Timer timer = new Timer { Interval = 16 };
            private float phase;

            public Activity()
            {
                DoubleBuffered = true;
                SetStyle(ControlStyles.Selectable, false);
                TabStop = false;
                timer.Tick += (sender, args) => { phase = (phase + 0.012f) % 1.4f; Invalidate(); };
            }

            protected override void OnVisibleChanged(System.EventArgs args)
            {
                base.OnVisibleChanged(args);
                timer.Enabled = Visible;
            }

            protected override void OnPaint(PaintEventArgs args)
            {
                var g = args.Graphics;
                g.Clear(Canvas);
                using (var track = new SolidBrush(Border))
                using (var red = new SolidBrush(Signal))
                {
                    g.FillRectangle(track, 0, 0, Width, Height);
                    float span = Width * 0.28f, x = (phase - 0.2f) * Width;
                    g.FillRectangle(red, x, 0, span, Height);
                }
            }

            protected override void Dispose(bool disposing)
            {
                if (disposing) timer.Dispose();
                base.Dispose(disposing);
            }
        }

        private sealed class StatusPill : Control
        {
            public StatusPill(string text, Font font) { Text = text; Font = font; DoubleBuffered = true; }

            protected override void OnPaint(PaintEventArgs args)
            {
                var g = args.Graphics;
                g.SmoothingMode = SmoothingMode.AntiAlias;
                Smooth(g, Font);
                float s = SetupWindow.Scale(g);
                using (var path = new GraphicsPath())
                using (var fill = new SolidBrush(Raised))
                using (var edge = new Pen(Border))
                using (var dot = new SolidBrush(Muted))
                using (var textFormat = new StringFormat { LineAlignment = StringAlignment.Center, FormatFlags = StringFormatFlags.NoWrap })
                {
                    float h = Height - 1;
                    path.AddArc(0, 0, h, h, 90, 180);
                    path.AddArc(Width - h - 1, 0, h, h, 270, 180);
                    path.CloseFigure();
                    g.FillPath(fill, path);
                    g.DrawPath(edge, path);
                    g.FillEllipse(dot, 12 * s, (Height - 6 * s) / 2f, 6 * s, 6 * s);
                    g.DrawString(Text, Font, dot, new RectangleF(25 * s, 0, Width - 29 * s, Height), textFormat);
                }
            }
        }
    }
}
