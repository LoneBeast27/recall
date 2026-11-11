using System;
using System.Collections.Generic;
using System.Drawing;
using System.Windows.Forms;

namespace Recall.Setup
{
    /// Every control's bounds in 96-DPI layout units, kept for the window's lifetime, and the one
    /// way to turn them into pixels. A rescale (a move to a monitor at another scale) always starts
    /// from these units, never from pixel bounds an earlier scale rounded, so 1.0 -> 1.5 -> 1.0 gives
    /// back the exact pixels and 1.0 -> 2.0 equals a window laid out at 2.0 directly.
    internal sealed class UnitLayout
    {
        private readonly Dictionary<Control, Rectangle> units = new Dictionary<Control, Rectangle>();

        /// The scale the controls' pixel bounds are at now (1 before the first Apply).
        public float Scale { get; private set; }

        public UnitLayout() { Scale = 1f; }

        public bool Captured { get { return units.Count > 0; } }

        /// Records the current bounds of every control under root as layout units (once per control).
        public void Capture(Control root)
        {
            foreach (Control child in root.Controls)
            {
                if (!units.ContainsKey(child)) units[child] = child.Bounds;
                Capture(child);
            }
        }

        public Rectangle UnitsOf(Control control)
        {
            Rectangle b;
            return units.TryGetValue(control, out b) ? b : control.Bounds;
        }

        /// Moves a control in layout units; the caller applies the scale again.
        public void SetTop(Control control, int top)
        {
            if (!units.ContainsKey(control)) { control.Top = top; return; }
            var b = units[control];
            units[control] = new Rectangle(b.X, top, b.Width, b.Height);
        }

        /// Every control's pixel bounds from its layout units at this scale. Edges are rounded in
        /// root coordinates, so a child that ends at its parent's edge in units still does in pixels.
        public void Apply(Control root, float scale)
        {
            Capture(root);
            Scale = scale;
            ScaleTree(root, PointF.Empty, Point.Empty, scale);
        }

        private void ScaleTree(Control parent, PointF parentUnits, Point parentPixels, float scale)
        {
            foreach (Control child in parent.Controls)
            {
                var b = units[child];
                float x = parentUnits.X + b.Left, y = parentUnits.Y + b.Top;
                int left = (int)Math.Round(x * scale), top = (int)Math.Round(y * scale);
                int right = (int)Math.Round((x + b.Width) * scale), bottom = (int)Math.Round((y + b.Height) * scale);
                var pixels = new Rectangle(left - parentPixels.X, top - parentPixels.Y, right - left, bottom - top);
                if (child.Bounds != pixels) child.Bounds = pixels;
                ScaleTree(child, new PointF(x, y), new Point(left, top), scale);
            }
        }

        /// The page's client size at a scale.
        public static Size PageSize(float scale)
        {
            return new Size((int)Math.Round(SetupWindow.PageWidth * scale), (int)Math.Round(SetupWindow.PageHeight * scale));
        }
    }
}
