using System;
using System.Drawing;

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
    }
}
