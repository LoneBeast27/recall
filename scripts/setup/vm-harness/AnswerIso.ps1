# Dot-sourced by Start-CleanGuest.ps1: writes a folder into a small ISO (IMAPI2) that the guest's
# Windows Setup reads its Autounattend.xml from.
function Save-AnswerIso([string]$Folder, [string]$Path) {
    if (-not ('RecallAnswerIso' -as [type])) {
        Add-Type -TypeDefinition @"
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
public static class RecallAnswerIso {
    public static void Save(string path, object imageStream) {
        var src = (IStream)imageStream;
        var length = Marshal.AllocHGlobal(4);
        try {
            var buffer = new byte[65536];
            using (var file = new FileStream(path, FileMode.CreateNew)) {
                while (true) {
                    src.Read(buffer, buffer.Length, length);
                    var count = Marshal.ReadInt32(length);
                    if (count <= 0) break;
                    file.Write(buffer, 0, count);
                }
            }
        } finally { Marshal.FreeHGlobal(length); }
    }
}
"@
    }
    $filesystem = New-Object -ComObject IMAPI2FS.MsftFileSystemImage
    $filesystem.FileSystemsToCreate = 3
    $filesystem.VolumeName = 'RECALLANSWER'
    $filesystem.Root.AddTree($Folder, $false)
    $disc = $filesystem.CreateResultImage()
    [RecallAnswerIso]::Save($Path, $disc.ImageStream)
    if ((Get-Item -LiteralPath $Path).Length -lt 32KB) { throw 'The answer ISO was not written correctly' }
}
