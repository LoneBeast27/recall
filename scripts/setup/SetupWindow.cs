using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using System.Windows.Forms;

namespace Recall.Setup
{
    internal sealed partial class SetupWindow : Form
    {
        private static readonly Color Canvas = Color.FromArgb(10, 10, 10);
        private static readonly Color Raised = Color.FromArgb(18, 18, 18);
        private static readonly Color Border = Color.FromArgb(40, 40, 40);
        private static readonly Color Ink = Color.FromArgb(237, 237, 237);
        private static readonly Color Muted = Color.FromArgb(163, 163, 163);
        private static readonly Color Signal = Color.FromArgb(255, 69, 58);
        private static readonly Color Mint = Color.FromArgb(129, 201, 149);
        private static readonly Color Error = Color.FromArgb(242, 139, 130);

        private readonly SetupFonts fonts = new SetupFonts();
        private readonly bool preview = !SetupPackage.HasPayload;
        private readonly bool updateMode;
        private readonly string updatePath;
        private readonly SetupPrerequisites prerequisites = SetupPrerequisites.Detect();
        private readonly Panel options;
        private readonly Panel quick;
        private readonly Button mode;
        private readonly Panel result;
        private readonly Label title;
        private readonly Label description;
        private readonly Label destinationNote;
        private readonly Label resultTitle;
        private readonly Label resultDetail;
        private readonly TextBox destination;
        private readonly CheckBox shortcut;
        private readonly CheckBox launch;
        private readonly CheckBox frameTiming;
        private readonly Button cancel;
        private readonly Button primary;
        private readonly Label[] steps = new Label[3];
        private readonly Rail rail;
        private bool busy;
        private bool complete;
        private bool restartRequired;
        private string installedPath;
        private int step;
        private bool failed;

        public SetupWindow() : this(null) { }

        public SetupWindow(string updatePath)
        {
            this.updatePath = updatePath;
            updateMode = updatePath != null;
            Text = "Recall Setup";
            Icon = Icon.ExtractAssociatedIcon(Assembly.GetExecutingAssembly().Location);
            StartPosition = FormStartPosition.CenterScreen;
            FormBorderStyle = FormBorderStyle.FixedDialog;
            MaximizeBox = false;
            MinimizeBox = false;
            AutoScaleMode = AutoScaleMode.Dpi;
            AutoScaleDimensions = new SizeF(96, 96);
            ClientSize = new Size(764, 490);
            BackColor = Canvas;
            ForeColor = Ink;
            Font = fonts.Body;
            DoubleBuffered = true;

            rail = new Rail { Bounds = new Rectangle(0, 0, 181, 490) };
            Controls.Add(rail);
            AddLabel(rail, "Recall", 49, 26, 106, 35, fonts.Mark, Ink);
            for (int i = 0; i < steps.Length; i++)
            {
                steps[i] = AddLabel(rail, new[] { "Setup", "Install", "Finish" }[i],
                    50, 127 + i * 43, 112, 24, fonts.Body, Muted);
            }
            AddLabel(rail, "Game clips,\non your PC.", 24, 416, 140, 48, fonts.Body, Muted);

            title = AddLabel(this, "Install Recall", 210, 34, 388, 54, fonts.Title, Ink);
            description = AddLabel(this, "Set up your game recorder, clip library, and editor.",
                211, 92, 525, 29, fonts.Body, Muted);
            var status = new StatusPill(preview ? "Setup preview" : "Windows setup", fonts.Strong)
                { Bounds = new Rectangle(589, 40, 146, 28) };
            Controls.Add(status);

            options = new Panel { Bounds = new Rectangle(210, 138, 525, 282) };
            Controls.Add(options);
            AddLabel(options, "Install location", 0, 5, 520, 24, fonts.Strong, Ink);
            var field = new Panel { Bounds = new Rectangle(0, 34, 420, 37), BackColor = Raised };

            options.Controls.Add(field);
            destination = new TextBox {
                Bounds = new Rectangle(10, 9, 400, 23), BorderStyle = BorderStyle.None,
                BackColor = Raised, ForeColor = Ink, Font = fonts.SystemText,
                AccessibleName = "Install location", TabIndex = 0,
                Text = updatePath ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Programs", "Recall")
            };
            field.Paint += (s, e) =>
            {
                using (var edge = new Pen(destination.Focused ? Muted : Border))
                    e.Graphics.DrawRectangle(edge, 0, 0, field.Width - 1, field.Height - 1);
            };
            field.Controls.Add(destination);
            destination.GotFocus += (s, e) => field.Invalidate();
            destination.LostFocus += (s, e) => field.Invalidate();
            var browse = AddButton(options, "Browse…", 430, 34, 95, 37, false, 1);
            browse.Click += (sender, args) => BrowseFolder();
            destinationNote = AddLabel(options, "Choose an SSD for Recall and its preview cache.",
                0, 75, 525, 27, fonts.Body, Muted);
            bool existing = File.Exists(Path.Combine(destination.Text, "recall-native.exe"));
            string startMenu = Environment.GetFolderPath(Environment.SpecialFolder.Programs);
            shortcut = AddOption("Add or refresh a Start menu shortcut", 113,
                !existing || File.Exists(Path.Combine(startMenu, "Recall.lnk")), 2);
            launch = AddOption("Start recording after guided setup", 149, false, 3);
            frameTiming = AddOption("Set up game detection and FPS", 185, false, 4);
            AddLabel(options, "Frame-timing stats: joins Performance Log Users. Windows asks first.",
                26, 216, 499, 22, fonts.Body, Muted);
            AddLabel(options, prerequisites.Summary(),
                0, 254, 525, 24, fonts.Body, Muted);

            quick = CreateQuickSetup();
            Controls.Add(quick);

            result = new Panel { Bounds = options.Bounds, Visible = false };
            Controls.Add(result);
            resultTitle = AddLabel(result, "", 0, 39, 525, 38, fonts.Mark, Ink);
            resultDetail = AddLabel(result, "", 0, 88, 525, 106, fonts.Body, Muted);
            var progress = new ProgressBar {
                Name = "progress", Style = ProgressBarStyle.Marquee, MarqueeAnimationSpeed = 32,
                Bounds = new Rectangle(0, 12, 525, 4), Visible = false
            };
            result.Controls.Add(progress);

            mode = AddButton(this, "Advanced", 210, 441, 124, 34, false, 1);
            cancel = AddButton(this, "Cancel", 490, 441, 90, 34, false, 2);
            primary = AddButton(this, preview ? "Preview install" : "Install Recall", 592, 441, 143, 34, true, 3);
            options.TabIndex = 0;
            result.TabStop = false;
            mode.Click += (sender, args) => ShowOptions(!advanced);
            cancel.Click += (sender, args) => Close();
            primary.Click += async (sender, args) => await Install();
            CancelButton = cancel;
            AcceptButton = primary;
            destination.TextChanged += (sender, args) => UpdateDestination();
            FormClosing += (sender, args) => { if (busy) args.Cancel = true; };
            ShowOptions(false);
            if (updateMode) ConfigureUpdate();
            SetStep(0);
        }

        private void ConfigureUpdate()
        {
            destination.ReadOnly = true;
            quickBrowse.Hide();
            quickLaunch.Hide();
            quickTiming.Hide();
            quickClips.Bounds = new Rectangle(0, 105, 525, 48);
            shortcut.Checked = File.Exists(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Programs), "Recall.lnk"));
            launch.Checked = false;
            frameTiming.Checked = false;
            mode.Hide();
            title.Text = "Update Recall";
            description.Text = "Review the installed folder, then update when you are ready.";
            primary.Text = "Update Recall";
            RefreshQuickSummary(updatePath);
        }

        [DllImport("dwmapi.dll")]
        private static extern int DwmSetWindowAttribute(IntPtr window, int attribute, ref int value, int size);

        protected override void OnHandleCreated(EventArgs args)
        {
            base.OnHandleCreated(args);
            int dark = 1;
            DwmSetWindowAttribute(Handle, 20, ref dark, sizeof(int));
        }

        protected override void OnPaint(PaintEventArgs args)
        {
            base.OnPaint(args);
            using (var line = new Pen(Border))
            {
                args.Graphics.DrawLine(line, 210, 129, 734, 129);
                args.Graphics.DrawLine(line, 181, 425, ClientSize.Width, 425);
            }
        }

        protected override void Dispose(bool disposing)
        {
            bool releaseFonts = disposing && !IsDisposed;
            base.Dispose(disposing);
            if (releaseFonts) fonts.Dispose();
        }

        private CheckBox AddOption(string text, int y, bool initial, int tab)
        {
            var check = new CheckBox {
                Text = text, Checked = initial, Bounds = new Rectangle(0, y, 525, 28),
                FlatStyle = FlatStyle.Flat, ForeColor = Ink, BackColor = Canvas,
                Font = fonts.Strong, UseCompatibleTextRendering = true, TabIndex = tab
            };
            check.FlatAppearance.BorderColor = Color.FromArgb(94, 94, 94);
            check.FlatAppearance.CheckedBackColor = Raised;
            options.Controls.Add(check);
            return check;
        }

        private static Label AddLabel(Control parent, string text, int x, int y, int w, int h, Font font, Color ink)
        {
            var label = new Label {
                Text = text, Bounds = new Rectangle(x, y, w, h), Font = font, ForeColor = ink,
                UseCompatibleTextRendering = true, UseMnemonic = false
            };
            parent.Controls.Add(label);
            return label;
        }

        private Button AddButton(Control parent, string text, int x, int y, int w, int h, bool accent, int tab)
        {
            var button = new Button {
                Text = text, Bounds = new Rectangle(x, y, w, h), FlatStyle = FlatStyle.Flat,
                BackColor = accent ? Signal : Raised, ForeColor = accent ? Canvas : Ink, Font = fonts.Strong,
                UseCompatibleTextRendering = true, UseVisualStyleBackColor = false, TabIndex = tab
            };
            button.FlatAppearance.BorderColor = accent ? Signal : Border;
            button.FlatAppearance.MouseOverBackColor = accent ? Color.FromArgb(255, 104, 95) : Border;
            parent.Controls.Add(button);
            return button;
        }

        private void UpdateDestination()
        {
            string path = SetupPackage.InstallPath(destination.Text);
            bool valid = path != null && (!updateMode
                || string.Equals(SetupPackage.ExistingInstallPath(path), updatePath, StringComparison.OrdinalIgnoreCase));
            primary.Enabled = valid && !busy;
            destinationNote.Text = valid ? (updateMode ? "This verified Recall installation will be updated in place."
                : "Choose an SSD for Recall and its preview cache.") : "Enter a folder on a local drive, such as C:\\Apps\\Recall.";
            destinationNote.ForeColor = valid ? Muted : Error;
            RefreshQuickSummary(path);
        }

        private void BrowseFolder()
        {
            using (var dialog = new FolderBrowserDialog { Description = "Choose where to install Recall" })
            {
                var folder = SetupPackage.InstallPath(destination.Text);
                if (folder != null && Directory.Exists(folder)) dialog.SelectedPath = folder;
                if (dialog.ShowDialog(this) == DialogResult.OK) destination.Text = dialog.SelectedPath;
            }
        }

        private void SetStep(int current)
        {
            step = current;
            rail.Step = step;
            for (int i = 0; i < steps.Length; i++)
            {
                steps[i].Font = i == step ? fonts.Strong : fonts.Body;
                steps[i].ForeColor = i == step ? Ink : Muted;
            }
            rail.Invalidate();
        }

        private async Task Install()
        {
            if (restartRequired)
            {
                Close();
                return;
            }
            if (failed)
            {
                Retry();
                return;
            }
            if (complete)
            {
                try
                {
                    if (!preview)
                    {
                        if (updateMode) SetupPackage.OpenRecall(installedPath);
                        else SetupPackage.OpenGuidedSetup(installedPath, launch.Checked && SetupPrerequisites.Detect().NvidiaDriver);
                    }
                }
                catch (Exception error)
                {
                    resultTitle.Text = "Recall setup could not open";
                    resultTitle.ForeColor = Error;
                    resultDetail.Text = error.GetBaseException().Message;
                    return;
                }
                Close();
                return;
            }
            var path = SetupPackage.InstallPath(destination.Text);
            if (path == null || busy) return;
            var selection = new InstallOptions(path, shortcut.Checked, false, frameTiming.Checked, updateMode);
            busy = true;
            options.Hide();
            quick.Hide();
            mode.Hide();
            result.Show();
            var progress = result.Controls["progress"];
            progress.Show();
            primary.Enabled = cancel.Enabled = false;
            SetStep(1);
            title.Text = preview ? "Previewing setup" : updateMode ? "Updating Recall" : "Installing Recall";
            description.Text = preview ? "See how setup looks while it runs." : "Setup waits for Recall to finish saving before replacing files.";
            resultTitle.Text = preview ? "Preparing the preview…" : "Checking Windows components…";
            resultTitle.ForeColor = Ink;
            resultDetail.Text = selection.Path;

            try
            {
                string warning = null;
                if (preview) await Task.Delay(450);
                else
                {
                    var updates = new Progress<string>(text => { if (busy) resultTitle.Text = text; });
                    warning = await Task.Run(() => SetupPackage.Install(selection, updates));
                    // Opt-in only, never on an update: Setup itself, elevated through the visible prompt.
                    if (selection.FrameTiming && !updateMode)
                    {
                        resultTitle.Text = "Setting up frame timing…";
                        var timing = await FrameTiming.Request(Handle);
                        if (timing != null && !timing.StartsWith("Frame timing: you are", StringComparison.Ordinal)
                            && !timing.StartsWith("Frame timing: added", StringComparison.Ordinal))
                            warning = warning == null ? timing : warning + "\n" + timing;
                    }
                }
                SetStep(2);
                title.Text = preview ? "Preview complete" : updateMode ? "Recall is updated" : "Recall is ready";
                description.Text = preview ? "Your selections were rehearsed." : updateMode
                    ? "The update completed successfully." : "Installation completed successfully.";
                resultTitle.Text = preview ? "Selected install location" : updateMode ? "Updated in place" : "Installed to";
                resultTitle.ForeColor = Mint;
                resultDetail.Text = selection.Path + (updateMode
                    ? "\n\nYour clips, cache, recording settings, and linked folders were kept."
                    : "\n\nContinue to set up storage, audio, and recording quality.");
                if (!preview && !SetupPrerequisites.Detect().NvidiaDriver)
                    resultDetail.Text += "\nAdd the NVIDIA recording driver before starting the recorder.";
                if (warning != null)
                {
                    description.Text = "Recall was installed. Review the setup note below.";
                    resultTitle.Text = "Setup note";
                    resultTitle.ForeColor = Ink;
                    resultDetail.Text = warning;
                }
                installedPath = selection.Path;
                primary.Text = preview ? "Done" : updateMode ? "Open Recall" : "Continue setup";
                cancel.Hide();
                complete = true;
            }
            catch (SetupRestartRequired error)
            {
                title.Text = "Restart Windows";
                description.Text = "Windows needs a restart before Recall can finish installing.";
                resultTitle.Text = "Restart required";
                resultTitle.ForeColor = Ink;
                resultDetail.Text = error.Message;
                primary.Text = "Close setup";
                cancel.Hide();
                restartRequired = true;
            }
            catch (Exception error)
            {
                title.Text = "Setup stopped";
                description.Text = "Review the message below, then return to your options.";
                resultTitle.Text = "Recall could not install";
                resultTitle.ForeColor = Error;
                resultDetail.Text = error.GetBaseException().Message;
                primary.Text = "Back to options";
                failed = true;
            }
            finally
            {
                busy = false;
                progress.Hide();
                primary.Enabled = cancel.Enabled = true;
                primary.Focus();
            }
        }

        private void Retry()
        {
            failed = false;
            result.Hide();
            SetStep(0);
            ShowOptions(updateMode ? false : advanced);
            if (updateMode) mode.Hide();
        }
    }
}
