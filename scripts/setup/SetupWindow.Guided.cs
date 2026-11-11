using System;
using System.Drawing;
using System.IO;
using System.Windows.Forms;

namespace Recall.Setup
{
    internal sealed partial class SetupWindow
    {
        private Label quickPath;
        private Button quickBrowse;
        private Label quickShortcut;
        private Label quickClips;
        private Label quickPrerequisites;
        private YesNoChoice quickLaunch;
        private YesNoChoice quickTiming;
        private bool advanced;
        /// Bounds are in pixels once ApplyScale ran (before that, in 96-DPI layout units, which
        /// UnitLayout keeps for every rescale).
        private bool layoutScaled;

        private Panel CreateQuickSetup()
        {
            var panel = new Panel { Bounds = options.Bounds, TabIndex = 0 };
            AddLabel(panel, "Install location", 0, 5, 525, 24, fonts.Strong, Ink);
            quickPath = AddLabel(panel, "", 0, 34, 418, 26, fonts.Body, Ink);
            quickPath.AutoEllipsis = true;
            quickBrowse = AddButton(panel, "Change…", 430, 28, 95, 32, false, 0);
            quickBrowse.AccessibleName = "Change install location";
            quickBrowse.Click += (sender, args) => BrowseFolder();
            quickShortcut = AddLabel(panel, "", 0, 60, 525, 23, fonts.Body, Muted);

            quickLaunch = AddDecision(panel, 101, "Start recording after guided setup?",
                "Recording stays paused while you set things up.", "Start recorder", value => launch.Checked = value);
            quickTiming = AddDecision(panel, 171, "Help Recall recognize more games and show FPS?",
                "Joins Performance Log Users for frame timing. Windows asks first.",
                "Set up game detection and FPS", value => frameTiming.Checked = value);
            quickClips = AddLabel(panel, "", 0, 241, 525, 20, fonts.Body, Muted);
            quickPrerequisites = AddLabel(panel, prerequisites.Summary(), 0, 263, 525, 19, fonts.Body, Muted);
            return panel;
        }

        private YesNoChoice AddDecision(Panel panel, int y, string question, string hint,
            string name, Action<bool> change)
        {
            var choice = new YesNoChoice(name, fonts.Strong, change)
            {
                Bounds = new Rectangle(414, y + 2, 111, 33),
                Question = AddLabel(panel, question, 0, y, 408, 24, fonts.Strong, Ink),
                Hint = AddLabel(panel, hint, 0, y + 25, 408, 36, fonts.Body, Muted)
            };
            panel.Controls.Add(choice);
            return choice;
        }

        private void ShowOptions(bool showAdvanced)
        {
            if (updateMode) showAdvanced = false;
            advanced = showAdvanced;
            options.Visible = advanced;
            quick.Visible = !advanced;
            mode.Text = advanced ? "Quick setup" : "Advanced";
            mode.Show();
            if (updateMode) mode.Hide();
            quickLaunch.Value = launch.Checked;
            quickTiming.Value = frameTiming.Checked;
            UpdateDestination();
        }

        private void RefreshQuickSummary(string path)
        {
            bool update = updateMode || path != null && File.Exists(Path.Combine(path, "recall-native.exe"));
            if (!busy && !failed && !complete)
            {
                title.Text = advanced ? "Advanced setup" : update ? "Update Recall" : "Set up Recall";
                description.Text = advanced ? "Choose the install folder, shortcut, and Windows permissions."
                    : update ? "Review the installed folder, then update when you are ready."
                    : "Quick setup installs Recall for your Windows account.";
                primary.Text = preview ? "Preview install" : update ? "Update Recall" : "Install Recall";
            }
            quickPath.Text = path ?? "Choose a valid install location.";
            quickPath.ForeColor = path == null ? Error : Ink;
            quickShortcut.Text = shortcut.Checked && !update ? "Setup adds or refreshes a Start menu shortcut."
                : "Setup keeps your existing Start menu shortcuts.";
            // An update opens Recall as it was, never guided setup: nothing to start after it.
            if (!updateMode)
            {
                quickLaunch.Shown = !update;
                PlaceQuickRows(!update);
            }
            quickClips.Text = update ? "Setup keeps your clip folder and recording settings."
                : @"Clips go to Videos\Recall. Change the folder in Settings → Storage.";
        }

        /// The rows under the start-recording question move up into its place while it is hidden,
        /// so an update shows no gap. Positions in 96-DPI layout units, scaled like the window.
        private void PlaceQuickRows(bool launchShown)
        {
            int top = layout.UnitsOf(quickLaunch.Question).Top + (launchShown ? 70 : 0);
            layout.SetTop(quickTiming.Question, top);
            layout.SetTop(quickTiming.Hint, top + 25);
            layout.SetTop(quickTiming, top + 2);
            layout.SetTop(quickClips, top + 70);
            layout.SetTop(quickPrerequisites, top + 92);
            if (layoutScaled) layout.Apply(this, UiScale);
        }

        private sealed class YesNoChoice : Panel
        {
            private readonly RadioButton yes;
            private readonly RadioButton no;
            public Label Question;
            public Label Hint;

            /// The question, its hint and the buttons together.
            public bool Shown
            {
                set { Visible = value; if (Question != null) Question.Visible = value; if (Hint != null) Hint.Visible = value; }
            }

            public YesNoChoice(string name, Font font, Action<bool> change)
            {
                yes = MakeButton("Yes", name, font, 0);
                no = MakeButton("No", name, font, 57);
                Controls.Add(yes);
                Controls.Add(no);
                no.Checked = true;
                yes.CheckedChanged += (sender, args) =>
                {
                    if (yes.Checked) change(true);
                };
                no.CheckedChanged += (sender, args) =>
                {
                    if (no.Checked) change(false);
                };
            }

            public bool Value
            {
                get { return yes.Checked; }
                set { if (value) yes.Checked = true; else no.Checked = true; }
            }

            private static RadioButton MakeButton(string text, string name, Font font, int x)
            {
                var button = new SmoothRadioButton
                {
                    Text = text, AccessibleName = name + " " + text, Appearance = Appearance.Button,
                    Bounds = new Rectangle(x, 0, 54, 33), FlatStyle = FlatStyle.Flat,
                    BackColor = Raised, ForeColor = Ink, Font = font, TextAlign = ContentAlignment.MiddleCenter,
                    UseCompatibleTextRendering = true, UseVisualStyleBackColor = false
                };
                button.FlatAppearance.BorderColor = Border;
                button.FlatAppearance.CheckedBackColor = Border;
                button.FlatAppearance.MouseOverBackColor = Color.FromArgb(34, 34, 34);
                return button;
            }
        }
    }
}
