using System;
using System.Windows.Forms;

namespace Recall.Setup
{
    /// An update the Recall app asked for (`--update <dir> --resume <file>`): the user already chose
    /// "Save and update" in the app, so the update starts as the window opens (it stays visible with
    /// its progress) and the window closes once the recorder and the app are back (UpdateFlow). A
    /// failure stays on screen; the old Recall is already reopened and says so.
    internal sealed partial class SetupWindow
    {
        private readonly string resumePath;

        protected override void OnShown(EventArgs args)
        {
            base.OnShown(args);
            if (resumePath != null && !preview) BeginInvoke(new Action(() => primary.PerformClick()));
        }

        /// After a successful resumed update: close (Recall reopened itself). True = closed.
        private bool ResumedUpdateDone()
        {
            if (resumePath == null || preview) return false;
            // After the install handler's own cleanup has run.
            BeginInvoke(new Action(Close));
            return true;
        }
    }
}
