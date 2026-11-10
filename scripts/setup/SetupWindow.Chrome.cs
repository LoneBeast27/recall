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
                using (var border = new Pen(Border))
                using (var ring = new Pen(Ink, 1.5f))
                using (var muted = new Pen(Muted, 1.3f))
                using (var red = new SolidBrush(Signal))
                {
                    g.DrawLine(border, Width - 1, 0, Width - 1, Height);
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

        private sealed class StatusPill : Control
        {
            public StatusPill(string text, Font font) { Text = text; Font = font; DoubleBuffered = true; }

            protected override void OnPaint(PaintEventArgs args)
            {
                var g = args.Graphics;
                g.SmoothingMode = SmoothingMode.AntiAlias;
                using (var path = new GraphicsPath())
                using (var fill = new SolidBrush(Raised))
                using (var edge = new Pen(Border))
                using (var dot = new SolidBrush(Muted))
                using (var textFormat = new StringFormat { LineAlignment = StringAlignment.Center, FormatFlags = StringFormatFlags.NoWrap })
                {
                    int h = Height - 1;
                    path.AddArc(0, 0, h, h, 90, 180);
                    path.AddArc(Width - h - 1, 0, h, h, 270, 180);
                    path.CloseFigure();
                    g.FillPath(fill, path);
                    g.DrawPath(edge, path);
                    g.FillEllipse(dot, 12, (Height - 6) / 2f, 6, 6);
                    g.DrawString(Text, Font, dot, new RectangleF(25, 0, Width - 29, Height), textFormat);
                }
            }
        }
    }
}
