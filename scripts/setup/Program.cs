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
        [STAThread]
        private static int Main(string[] args)
        {
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            string updatePath = null;
            if (args.Length == 2 && args[0].Equals("--update", StringComparison.OrdinalIgnoreCase))
                updatePath = SetupPackage.ExistingInstallPath(args[1]);
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
            if (args.Length == 2 && args[0].Equals("--update", StringComparison.OrdinalIgnoreCase) && updatePath == null)
            {
                MessageBox.Show("Recall Setup could not verify the installed Recall folder.", "Recall Setup",
                    MessageBoxButtons.OK, MessageBoxIcon.Error);
                return 2;
            }
            using (var window = new SetupWindow(updatePath))
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

        public SetupFonts()
        {
            var display = Load("Recall.Display.ttf");
            var body = Load("Recall.Body.ttf");
            var strong = Load("Recall.Strong.ttf");
            Title = Make(display, 29, FontStyle.Bold);
            Mark = Make(display, 18, FontStyle.Bold);
            Body = Make(body, 10.5f, FontStyle.Regular);
            Strong = Make(strong, 10.5f, FontStyle.Bold);
            SystemText = new Font("Segoe UI", 10.5f);
        }

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

        private static Font Make(FontFamily family, float size, FontStyle fallback)
        {
            if (family != null)
            {
                if (family.IsStyleAvailable(FontStyle.Regular)) return new Font(family, size);
                if (family.IsStyleAvailable(FontStyle.Bold)) return new Font(family, size, FontStyle.Bold);
            }
            return new Font("Segoe UI", size, fallback);
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
