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
        private YesNoChoice quickLaunch;
        private YesNoChoice quickTiming;
        private bool advanced;

        private Panel CreateQuickSetup()
        {
            var panel = new Panel { Bounds = options.Bounds, TabIndex = 0 };
            AddLabel(panel, "Install location", 0, 5, 525, 24, fonts.Strong, Ink);
            quickPath = AddLabel(panel, "", 0, 34, 418, 26, fonts.SystemText, Ink);
            quickPath.AutoEllipsis = true;
            quickBrowse = AddButton(panel, "Change…", 430, 28, 95, 32, false, 0);
            quickBrowse.AccessibleName = "Change install location";
            quickBrowse.Click += (sender, args) => BrowseFolder();
            quickShortcut = AddLabel(panel, "", 0, 60, 525, 23, fonts.Body, Muted);

            quickLaunch = AddDecision(panel, 101, "Start recording after guided setup?",
                "Recording stays paused while you set things up.", "Start recorder", value => launch.Checked = value);
            quickTiming = AddDecision(panel, 171, "Help Recall recognize more games and show FPS?",
                "Windows may ask for permission. It takes effect after sign-in.",
                "Set up game detection and FPS", value => frameTiming.Checked = value);
            quickClips = AddLabel(panel, "", 0, 241, 525, 20, fonts.Body, Muted);
            AddLabel(panel, "Game recording requires an NVIDIA GPU with NVENC.", 0, 263, 525, 19, fonts.Body, Muted);
            return panel;
        }

        private YesNoChoice AddDecision(Panel panel, int y, string question, string hint,
            string name, Action<bool> change)
        {
            AddLabel(panel, question, 0, y, 408, 24, fonts.Strong, Ink);
            AddLabel(panel, hint, 0, y + 25, 408, 36, fonts.Body, Muted);
            var choice = new YesNoChoice(name, fonts.Strong, change)
            {
                Bounds = new Rectangle(414, y + 2, 111, 33)
            };
            panel.Controls.Add(choice);
            return choice;
        }

        private void ShowOptions(bool showAdvanced)
        {
            advanced = showAdvanced;
            options.Visible = advanced;
            quick.Visible = !advanced;
            mode.Text = advanced ? "Quick setup" : "Advanced";
            mode.Show();
            quickLaunch.Value = launch.Checked;
            quickTiming.Value = frameTiming.Checked;
            UpdateDestination();
        }

        private void RefreshQuickSummary(string path)
        {
            bool update = path != null && File.Exists(Path.Combine(path, "recall-native.exe"));
            if (!busy && !failed && !complete)
            {
                title.Text = advanced ? "Advanced setup" : update ? "Update Recall" : "Set up Recall";
                description.Text = advanced ? "Choose the install folder, shortcut, and Windows permissions."
                    : "Quick setup installs Recall for your Windows account.";
                primary.Text = preview ? "Preview install" : update ? "Update Recall" : "Install Recall";
            }
            quickPath.Text = path ?? "Choose a valid install location.";
            quickPath.ForeColor = path == null ? Error : Ink;
            quickShortcut.Text = shortcut.Checked ? "Setup adds or refreshes a Start menu shortcut."
                : "Setup keeps your existing Start menu shortcuts.";
            quickClips.Text = update ? "Setup keeps your clip folder and recording settings."
                : @"Clips go to Videos\Recall. Change the folder in Settings → Media.";
        }

        private sealed class YesNoChoice : Panel
        {
            private readonly RadioButton yes;
            private readonly RadioButton no;

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
                var button = new RadioButton
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
