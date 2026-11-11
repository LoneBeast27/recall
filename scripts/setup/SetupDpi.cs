using System;
using System.Drawing;
using System.Runtime.InteropServices;

namespace Recall.Setup
{
    /// Windows' per-monitor DPI, with a fallback for every API older Windows lacks.
    ///
    /// Per-Monitor V2 (Windows 10 1703+): Windows hands each window the DPI of the monitor it is on
    /// and sends WM_DPICHANGED when it moves to another, instead of stretching a bitmap (blurry text
    /// on a monitor whose scale differs from the main one's). Setup lays itself out again for the new
    /// scale (SetupWindow.Dpi.cs).
    internal static class SetupDpi
    {
        public const int WmDpiChanged = 0x02E0;
        private static readonly IntPtr PerMonitorV2 = new IntPtr(-4);
        private static readonly IntPtr SystemAware = new IntPtr(-2);
        private const int ProcessPerMonitorDpiAware = 2;
        private const int MdtEffectiveDpi = 0;
        private const uint MonitorDefaultToNearest = 2;

        /// Which awareness this process got ("per-monitor v2", "per-monitor", "system"), for the log.
        public static string Mode { get; private set; }

        /// Before any window: Per-Monitor V2, else per-monitor (8.1), else system aware (Vista).
        public static void EnablePerMonitor()
        {
            try { if (SetProcessDpiAwarenessContext(PerMonitorV2)) { Mode = "per-monitor v2"; return; } }
            catch (EntryPointNotFoundException) { }
            try
            {
                // S_OK, or E_ACCESSDENIED when a manifest already chose: either way it is decided.
                int hr = SetProcessDpiAwareness(ProcessPerMonitorDpiAware);
                if (hr == 0) { Mode = "per-monitor"; return; }
            }
            catch (DllNotFoundException) { }
            catch (EntryPointNotFoundException) { }
            SetProcessDPIAware();
            Mode = "system";
        }

        /// The effective DPI of the monitor that holds a point (the nearest one), else the system DPI.
        public static int ForMonitorAt(Point point)
        {
            try
            {
                var monitor = MonitorFromPoint(new POINT { X = point.X, Y = point.Y }, MonitorDefaultToNearest);
                uint x, y;
                if (monitor != IntPtr.Zero && GetDpiForMonitor(monitor, MdtEffectiveDpi, out x, out y) == 0 && x > 0) return (int)x;
            }
            catch (DllNotFoundException) { }
            catch (EntryPointNotFoundException) { }
            return SystemDpi();
        }

        /// A window's DPI (1607+), else that of the monitor under its centre.
        public static int ForWindow(IntPtr window, Rectangle bounds)
        {
            try { int dpi = GetDpiForWindow(window); if (dpi > 0) return dpi; }
            catch (EntryPointNotFoundException) { }
            return ForMonitorAt(new Point(bounds.Left + bounds.Width / 2, bounds.Top + bounds.Height / 2));
        }

        public static int SystemDpi()
        {
            using (var screen = Graphics.FromHwnd(IntPtr.Zero)) return (int)Math.Round(screen.DpiX);
        }

        /// A window's frame (title bar and borders) at a DPI: AdjustWindowRectExForDpi (1607+),
        /// else the system-DPI frame grown in proportion.
        public static Size Frame(int style, int exStyle, int dpi)
        {
            var rect = new RECT();
            try { if (AdjustWindowRectExForDpi(ref rect, style, false, exStyle, (uint)dpi)) return rect.Size; }
            catch (EntryPointNotFoundException) { }
            rect = new RECT();
            if (!AdjustWindowRectEx(ref rect, style, false, exStyle)) return Size.Empty;
            float grow = dpi / (float)SystemDpi();
            return new Size((int)Math.Ceiling(rect.Size.Width * grow), (int)Math.Ceiling(rect.Size.Height * grow));
        }

        /// Runs a window that lays itself out at the system DPI (the uninstall question) as before:
        /// Windows stretches it on a monitor at another scale instead of showing it unscaled.
        public static T SystemAwareWindow<T>(Func<T> show)
        {
            IntPtr previous = IntPtr.Zero;
            try { previous = SetThreadDpiAwarenessContext(SystemAware); }
            catch (EntryPointNotFoundException) { }
            try { return show(); }
            finally
            {
                if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous);
            }
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct RECT
        {
            public int Left, Top, Right, Bottom;
            public Size Size { get { return new Size(Right - Left, Bottom - Top); } }
            public Rectangle ToRectangle() { return Rectangle.FromLTRB(Left, Top, Right, Bottom); }
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct POINT { public int X, Y; }

        [DllImport("user32.dll")] private static extern bool SetProcessDpiAwarenessContext(IntPtr context);
        [DllImport("user32.dll")] private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
        [DllImport("user32.dll")] private static extern bool SetProcessDPIAware();
        [DllImport("shcore.dll")] private static extern int SetProcessDpiAwareness(int value);
        [DllImport("shcore.dll")] private static extern int GetDpiForMonitor(IntPtr monitor, int type, out uint x, out uint y);
        [DllImport("user32.dll")] private static extern IntPtr MonitorFromPoint(POINT point, uint flags);
        [DllImport("user32.dll")] private static extern bool AdjustWindowRectExForDpi(ref RECT rect, int style, bool menu, int exStyle, uint dpi);
        [DllImport("user32.dll")] private static extern bool AdjustWindowRectEx(ref RECT rect, int style, bool menu, int exStyle);
        [DllImport("user32.dll")] private static extern int GetDpiForWindow(IntPtr window);
        [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr window, out RECT rect);
        [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out RECT rect);
        [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int w, int h, uint flags);
    }
}
