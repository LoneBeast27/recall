using System;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Text;

namespace Recall.Setup.Tests
{
    /// An outdated Setup opened by hand updates in place: it fetches the Setup the signed update
    /// file names, keeps it only on the signed size and SHA-256, and never opens a web page.
    internal static partial class TestRunner
    {
        private static void NewerSetupInPlace()
        {
            // The real 0.1.11 file carries what the fetch needs, from the signature-checked JSON.
            var shipped = UpdateSources.Keys(UpdateSources.Embedded(UpdateSources.KeysResource));
            var real = UpdateSources.Verify(Fixture("v0.1.11.json"), Fixture("v0.1.11.json.minisig"), shipped, new string[0]);
            Check(real != null && real.Setup != null && real.Setup.Size == 15102464L
                && real.Setup.Sha256 == "9bcccb6b54b7eae74770015629eb7e64e1bda73440a598af161507cba55817fe"
                && real.Setup.Urls.Count == 3 && real.Setup.Urls[0].StartsWith("https://gitlab.com/", StringComparison.Ordinal), "the signed file names Setup's size, SHA-256 and 3 addresses");
            var fetched = LatestRelease.Fetch((url, limit) => url.EndsWith(".minisig", StringComparison.Ordinal) ? Fixture("v0.1.11.json.minisig") : Fixture("v0.1.11.json"),
                "62f1bb6c928315aaacce881abeaf9e60dee48092", new List<string> { "https://a.example/recall-update.json" }, shipped, new string[0], 3000);
            Check(fetched != null && fetched.NewerBuild && fetched.Setup != null && fetched.Setup.Size == 15102464L, "the first 0.1.11 Setup sees the newer build and its download");

            var good = Encoding.ASCII.GetBytes("a newer RecallSetup.exe");
            var file = new NewerSetup.File("0.1.11", good.Length, NewerSetup.Sha256Of(good), new List<string> {
                "http://plain.example/RecallSetup.exe", "https://down.example/RecallSetup.exe", "https://bad.example/RecallSetup.exe", "https://good.example/RecallSetup.exe" });
            var asked = new List<string>();
            Func<string, long, Action<long>, byte[]> serve = (url, limit, progress) =>
            {
                asked.Add(url);
                if (url.Contains("down.")) throw new WebException("no answer", WebExceptionStatus.ConnectFailure);
                if (url.Contains("bad.")) return Encoding.ASCII.GetBytes("a changed RecallSetup.ex");
                progress(good.Length);
                return url.Contains("good.") ? good : null;
            };
            string why;
            long seen = 0;
            var body = NewerSetup.Fetch(file, serve, n => seen = n, out why);
            Check(body != null && Encoding.ASCII.GetString(body) == "a newer RecallSetup.exe" && seen == good.Length, "the first address with the signed bytes wins");
            Check(asked.Count == 3 && !asked.Contains("http://plain.example/RecallSetup.exe"), "plain http is never asked; a failing address moves on");
            file = new NewerSetup.File("0.1.11", good.Length, NewerSetup.Sha256Of(Encoding.ASCII.GetBytes("other")), new List<string> { "https://good.example/RecallSetup.exe" });
            Check(NewerSetup.Fetch(file, serve, n => { }, out why) == null && why.Contains("did not match"), "a file that differs from the signed SHA-256 is refused");
            Check(NewerSetup.Fetch(null, serve, n => { }, out why) == null && why.Length > 0, "no signed download: a reason, no fetch");

            var root = Path.Combine(Path.GetTempPath(), "recall-setup-newer-" + Guid.NewGuid().ToString("N"));
            try
            {
                var path = NewerSetup.Save(good, root);
                Check(File.ReadAllText(path) == "a newer RecallSetup.exe" && Path.GetFileName(path) == "RecallSetup.exe"
                    && Path.GetFileName(Path.GetDirectoryName(path)).StartsWith(NewerSetup.FolderPrefix, StringComparison.Ordinal), "saved as RecallSetup.exe in its own folder");
                var start = NewerSetup.StartInfo(path);
                Check(start.Arguments == "" && start.EnvironmentVariables[NewerSetup.HandedOffVariable] == "1", "the newer Setup opens as by hand, marked so it never offers again");
            }
            finally
            {
                try { Directory.Delete(root, true); } catch (IOException) { } catch (UnauthorizedAccessException) { }
            }

            // No path in Setup sends the user to a web page for an update.
            foreach (var source in Directory.GetFiles(sourceDir, "*.cs"))
            {
                var text = File.ReadAllText(source);
                Check(text.IndexOf("OpenReleases", StringComparison.Ordinal) < 0 && text.IndexOf("Process.Start(new ProcessStartInfo(UpdateFlow.ReleasesUrl", StringComparison.Ordinal) < 0
                    && text.IndexOf("Open the download page", StringComparison.Ordinal) < 0, Path.GetFileName(source) + " opens the download page");
            }
        }
    }
}
