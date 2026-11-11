using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Text;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Windows.Forms;

namespace Recall.Setup
{
    internal static class Program
    {
        [DllImport("user32.dll")]
        private static extern bool SetProcessDPIAware();

        [STAThread]
        private static int Main(string[] args)
        {
            // Before any window: Windows then hands Setup real pixels instead of stretching a 96-DPI
            // bitmap (the blurry text at 125-200 % scaling). The window scales itself (AutoScaleMode.Dpi).
            SetProcessDPIAware();
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            // Leftover %TEMP%\Recall-Uninstall-<hex> copies from an earlier uninstall (best-effort).
            TempCopy.Sweep(Path.GetTempPath(), line => { });
            int handled;
            if (SetupModes.TryRun(args, out handled)) return handled;
            bool repair = args.Length == 1 && args[0].Equals(LaunchChecks.RepairArgument, StringComparison.OrdinalIgnoreCase);
            if (repair) args = new string[0];
            else if (args.Length == 0 && SetupPackage.HasPayload)
            {
                // Opened by hand: the version already installed against the latest published one.
                var targets = SetupTargets.FromEnvironment();
                var installed = InstalledRecall.Find(targets);
                if (installed != null)
                {
                    // The app's "Check for updates automatically" off: no GitHub call (compared as offline).
                    var latest = LatestRelease.ChecksAllowed(UserRoots.For(targets).Local) ? LatestRelease.Fetch() : null;
                    var check = LaunchChecks.Decide(installed.Version, SetupBuildInfo.Version, latest);
                    SetupLog.For(null)("Setup " + SetupBuildInfo.Version + " opened: installed " + (installed.Version ?? "?")
                        + ", latest " + (latest ?? "unknown (offline)") + ": " + check);
                    if (!LaunchChecks.Confirm(check, installed, SetupBuildInfo.Version, latest)) return 0;
                }
            }
            string updatePath = null, resumePath = null;
            bool update = (args.Length == 2 || (args.Length == 4 && args[2].Equals("--resume", StringComparison.OrdinalIgnoreCase)))
                && args[0].Equals("--update", StringComparison.OrdinalIgnoreCase);
            if (update)
            {
                updatePath = SetupPackage.ExistingInstallPath(args[1]);
                if (args.Length == 4) resumePath = args[3];
                // A swap an earlier run could not finish is undone before anything else (all old again).
                if (updatePath != null)
                {
                    var why = InstallSwap.Recover(updatePath);
                    if (why != null) SetupLog.For(null)("WARNING: an earlier update is not undone yet: " + why);
                }
            }
            else if (args.Length != 0
#if PREVIEW
                && !(args.Length == 2 && args[0] == "--check-preview")
#endif
                )
            {
                MessageBox.Show("Recall Setup received unsupported options.", "Recall Setup",
                    MessageBoxButtons.OK, MessageBoxIcon.Error);
                return 2;
            }
            if (update && updatePath == null)
            {
                MessageBox.Show("Recall Setup could not verify the installed Recall folder.", "Recall Setup",
                    MessageBoxButtons.OK, MessageBoxIcon.Error);
                return 2;
            }
            using (var window = new SetupWindow(updatePath, resumePath))
            {
#if PREVIEW
                if (args.Length == 2 && args[0] == "--check-preview")
                {
                    try
                    {
                        window.SavePreview(args[1]);
                        window.CheckPreview();
                        return 0;
                    }
                    catch (Exception error)
                    {
                        File.WriteAllText(args[1] + ".error.txt", error.ToString());
                        return 1;
                    }
                }
#endif
                Application.Run(window);
            }
            return 0;
        }
    }

    internal sealed class SetupFonts : IDisposable
    {
        private readonly List<PrivateFontCollection> collections = new List<PrivateFontCollection>();
        private readonly List<IntPtr> buffers = new List<IntPtr>();
        public readonly Font Title;
        public readonly Font Mark;
        public readonly Font Body;
        public readonly Font Strong;
        public readonly Font SystemText;

        /// Sizes in points at 96 DPI, made in pixels at the window's scale, so text grows and
        /// shrinks with the layout instead of with the display setting alone.
        public SetupFonts(float scale)
        {
            this.scale = scale;
            var display = Load("Recall.Display.ttf");
            var body = Load("Recall.Body.ttf");
            var strong = Load("Recall.Strong.ttf");
            Title = Make(display, 29, FontStyle.Bold);
            Mark = Make(display, 18, FontStyle.Bold);
            Body = Make(body, 10.5f, FontStyle.Regular);
            Strong = Make(strong, 10.5f, FontStyle.Bold);
            SystemText = new Font("Segoe UI", Pixels(10.5f), GraphicsUnit.Pixel);
        }

        private readonly float scale;

        private float Pixels(float points) { return points * 96f / 72f * scale; }

        private FontFamily Load(string name)
        {
            var collection = new PrivateFontCollection();
            collections.Add(collection);
            using (var input = Assembly.GetExecutingAssembly().GetManifestResourceStream(name))
            using (var memory = new MemoryStream())
            {
                if (input == null) return null;
                input.CopyTo(memory);
                var data = memory.ToArray();
                var address = Marshal.AllocHGlobal(data.Length);
                buffers.Add(address);
                Marshal.Copy(data, 0, address, data.Length);
                collection.AddMemoryFont(address, data.Length);
            }
            var families = collection.Families;
            return families.Length == 0 ? null : families[0];
        }

        private Font Make(FontFamily family, float points, FontStyle fallback)
        {
            float size = Pixels(points);
            if (family != null)
            {
                if (family.IsStyleAvailable(FontStyle.Regular)) return new Font(family, size, FontStyle.Regular, GraphicsUnit.Pixel);
                if (family.IsStyleAvailable(FontStyle.Bold)) return new Font(family, size, FontStyle.Bold, GraphicsUnit.Pixel);
            }
            return new Font("Segoe UI", size, fallback, GraphicsUnit.Pixel);
        }

        public void Dispose()
        {
            Title.Dispose();
            Mark.Dispose();
            Body.Dispose();
            Strong.Dispose();
            SystemText.Dispose();
            foreach (var collection in collections) collection.Dispose();
            foreach (var address in buffers) Marshal.FreeHGlobal(address);
        }
    }
}
