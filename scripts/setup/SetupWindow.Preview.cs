using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Windows.Forms;

namespace Recall.Setup
{
    internal sealed partial class SetupWindow
    {
#if PREVIEW
        private string previewPath;

        public void SavePreview(string path)
        {
            previewPath = path;
            FormBorderStyle = FormBorderStyle.None;
            ShowInTaskbar = false;
            Opacity = 0;
            string originalPath = destination.Text;
            destination.Text = @"C:\Users\Player\AppData\Local\Programs\Recall";
            ActiveControl = primary;
            Show();
            Application.DoEvents();
            File.WriteAllText(path + ".dpi.txt", "start " + UiScale + ", " + openingNote + Environment.NewLine
                + string.Join(Environment.NewLine, realDpiChanges) + Environment.NewLine);
            CheckMoved(UiScale);
            SaveFrame(path);
            SimulateMoves(path);
            ShowOptions(true);
            Application.DoEvents();
            SaveFrame(Path.ChangeExtension(path, ".advanced.png"));
            ShowOptions(false);
            quickLaunch.Shown = false;
            PlaceQuickRows(false);
            Application.DoEvents();
            SaveFrame(Path.ChangeExtension(path, ".update.png"));
            quickLaunch.Shown = true;
            PlaceQuickRows(true);
            destination.Text = originalPath;
        }

        private void SaveFrame(string path)
        {
            using (var bitmap = new Bitmap(ClientSize.Width, ClientSize.Height))
            {
                DrawToBitmap(bitmap, new Rectangle(Point.Empty, ClientSize));
                bitmap.Save(path, ImageFormat.Png);
            }
        }

        /// Every visible control lies inside its parent, and the page inside the window.
        private void CheckFits(Control parent)
        {
            foreach (Control child in parent.Controls)
            {
                if (!child.Visible) continue;
                var inside = new Rectangle(Point.Empty, parent.ClientSize);
                if (!inside.Contains(child.Bounds))
                    throw new InvalidOperationException("At scale " + UiScale + ", " + child.GetType().Name + " '" + child.Text
                        + "' " + child.Bounds + " leaves its parent " + parent.GetType().Name + " " + inside);
                CheckFits(child);
            }
        }

        public void CheckPreview()
        {
            CheckFits(this);
            ShowOptions(true);
            CheckFits(this);
            ShowOptions(false);
            SetupPrerequisites.CheckPolicy();
            if (!preview || advanced || !quick.Visible || launch.Checked || frameTiming.Checked)
                throw new InvalidOperationException("Quick setup defaults changed");
            if (!quickBrowse.Visible || !quickBrowse.Enabled || quickPath.Text != SetupPackage.InstallPath(destination.Text))
                throw new InvalidOperationException("Quick setup does not show the current folder choice");
            quickLaunch.Value = true;
            quickTiming.Value = true;
            mode.PerformClick();
            if (!advanced || !options.Visible || !launch.Checked || !frameTiming.Checked)
                throw new InvalidOperationException("Advanced lost the Yes choices");
            launch.Checked = false;
            frameTiming.Checked = false;
            shortcut.Checked = false;
            string folder = destination.Text;
            destination.Text = "relative";
            if (primary.Enabled) throw new InvalidOperationException("Invalid path did not disable setup");
            mode.PerformClick();
            if (primary.Enabled || quickLaunch.Value || quickTiming.Value || !quickBrowse.Visible || !quickBrowse.Enabled)
                throw new InvalidOperationException("Quick setup lost the Advanced choices or the folder picker");
            mode.PerformClick();
            destination.Text = folder;
            if (!primary.Enabled || shortcut.Checked)
                throw new InvalidOperationException("Advanced lost the selected shortcut preference");
            mode.PerformClick();
            primary.PerformClick();
            if (!busy || step != 1 || primary.Enabled) throw new InvalidOperationException("Install state was not engaged");
            resultTitle.Text = "Downloading verified FFmpeg components…";
            Application.DoEvents();
            if (previewPath != null) SaveFrame(Path.ChangeExtension(previewPath, ".installing.png"));
            var deadline = DateTime.UtcNow.AddSeconds(3);
            while (busy && DateTime.UtcNow < deadline)
            {
                Application.DoEvents();
                System.Threading.Thread.Sleep(15);
            }
            if (busy || !complete || step != 2 || primary.Text != "Done")
                throw new InvalidOperationException("Preview did not reach its finished state");
        }
#endif
    }
}
