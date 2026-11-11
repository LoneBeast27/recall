using System;
using System.Collections.Generic;
using System.Drawing;
using System.Windows.Forms;

namespace Recall.Setup.Tests
{
    /// The window's one scale: the display scale, never larger than the work area allows.
    internal static partial class TestRunner
    {
        private static void WindowFitsEveryScreen()
        {
            var screens = new[] {
                new Size(1024, 728), new Size(1280, 680), new Size(1366, 728), new Size(1536, 824),
                new Size(1920, 1032), new Size(2560, 1392), new Size(3840, 2112), new Size(960, 1659)
            };
            foreach (var display in new[] { 1f, 1.25f, 1.5f, 1.75f, 2f, 2.25f, 2.5f, 3f, 3.5f })
            {
                // A fixed dialog's frame at that scale (6 x 29 units at 100 %).
                var frame = new Size((int)Math.Round(6 * display), (int)Math.Round(29 * display));
                foreach (var area in screens)
                {
                    float s = SetupWindow.ChooseScale(display, area, frame);
                    int w = (int)Math.Round(SetupWindow.PageWidth * s) + frame.Width;
                    int h = (int)Math.Round(SetupWindow.PageHeight * s) + frame.Height;
                    Check(s <= display, "never larger than the display scale: " + display + " on " + area);
                    Check(s >= 0.5f, "never below half size: " + display + " on " + area);
                    Check(w <= area.Width && h <= area.Height,
                        "window " + w + "x" + h + " fits " + area + " at " + display);
                }
            }
            // Room to spare: the display scale itself (the page looks as designed).
            Check(SetupWindow.ChooseScale(1.5f, new Size(2560, 1392), new Size(9, 44)) == 1.5f, "1.5 kept on a 1440p screen");
            Check(SetupWindow.ChooseScale(1f, new Size(1366, 728), new Size(6, 29)) == 1f, "1.0 kept on a 1366x768 laptop");
            Check(SetupWindow.ChooseScale(float.NaN, new Size(1920, 1032), new Size(6, 29)) == 1f, "a bad DPI reading counts as 100 %");
        }

        /// A page shaped like Setup's: nested panels with odd unit bounds that round differently.
        private static Panel ScaleFixture()
        {
            var root = new Panel { Bounds = new Rectangle(0, 0, SetupWindow.PageWidth, SetupWindow.PageHeight) };
            var rail = new Panel { Bounds = new Rectangle(0, 0, 181, 490) };
            rail.Controls.Add(new Label { Bounds = new Rectangle(49, 26, 106, 35) });
            var options = new Panel { Bounds = new Rectangle(210, 138, 525, 282) };
            var field = new Panel { Bounds = new Rectangle(0, 34, 420, 37) };
            field.Controls.Add(new TextBox { Bounds = new Rectangle(10, 9, 400, 23) });
            options.Controls.Add(field);
            options.Controls.Add(new Button { Bounds = new Rectangle(430, 34, 95, 37) });
            options.Controls.Add(new Label { Bounds = new Rectangle(26, 216, 499, 22) });
            root.Controls.Add(rail);
            root.Controls.Add(options);
            root.Controls.Add(new Button { Bounds = new Rectangle(592, 441, 143, 34) });
            return root;
        }

        private static List<Rectangle> PixelBounds(Control parent, List<Rectangle> into)
        {
            foreach (Control child in parent.Controls) { into.Add(child.Bounds); PixelBounds(child, into); }
            return into;
        }

        private static bool Same(List<Rectangle> a, List<Rectangle> b)
        {
            if (a.Count != b.Count) return false;
            for (int i = 0; i < a.Count; i++) if (a[i] != b[i]) return false;
            return true;
        }

        /// A move to a monitor at another scale lays out from the 96-DPI units, never from the
        /// pixels an earlier scale rounded.
        private static void RescaleFromUnits()
        {
            var units = PixelBounds(ScaleFixture(), new List<Rectangle>());
            var page = ScaleFixture();
            var layout = new UnitLayout();
            layout.Apply(page, 1f);
            Check(Same(PixelBounds(page, new List<Rectangle>()), units), "1.0 is the unit layout itself");
            layout.Apply(page, 1.5f);
            var at15 = PixelBounds(page, new List<Rectangle>());
            Check(!Same(at15, units), "1.5 moved the controls");
            layout.Apply(page, 1f);
            Check(Same(PixelBounds(page, new List<Rectangle>()), units), "1.0 -> 1.5 -> 1.0 gives back the exact pixels");
            foreach (var s in new[] { 1.25f, 1.75f, 2f, 3f, 0.5f })
            {
                layout.Apply(page, s);
                layout.Apply(page, s);
                var direct = ScaleFixture();
                new UnitLayout().Apply(direct, s);
                Check(Same(PixelBounds(page, new List<Rectangle>()), PixelBounds(direct, new List<Rectangle>())),
                    "1.0 -> ... -> " + s + " equals a direct " + s + " layout (applied twice: no double scaling)");
            }
            // A row moved in units after scaling lands where a page laid out with it there would.
            var label = page.Controls[1].Controls[2];
            layout.SetTop(label, 150);
            layout.Apply(page, 1.75f);
            var moved = ScaleFixture();
            moved.Controls[1].Controls[2].Top = 150;
            new UnitLayout().Apply(moved, 1.75f);
            Check(Same(PixelBounds(page, new List<Rectangle>()), PixelBounds(moved, new List<Rectangle>())), "a unit move rescales like the layout");
            Check(UnitLayout.PageSize(1.5f) == new Size(1146, 735), "the page at 1.5 is 1146 x 735");
            // The window kept inside the target monitor's work area.
            var work = new Rectangle(1920, 0, 2560, 1392);
            Check(SetupWindow.Inside(new Rectangle(4300, 1200, 1146, 780), work) == new Rectangle(3334, 612, 1146, 780), "pulled back inside");
            Check(SetupWindow.Inside(new Rectangle(2000, 40, 1146, 780), work) == new Rectangle(2000, 40, 1146, 780), "left where it fits");
        }
    }
}
