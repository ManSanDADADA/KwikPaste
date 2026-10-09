# Real Windows image paste through the platform panel's Enter -> paste::paste path.
#
#   powershell -STA -NoProfile -ExecutionPolicy Bypass -File tools\platform-probes\image-paste.ps1 [-Exe <KwikPaste.exe>] [-IdleSeconds 30]
#
# Runs only the native-dev selftest-platform identity; refuses any already-running KwikPaste.
# Creates its own WinForms target, checks one real Ctrl+V / WM_PASTE, every pixel, foreground,
# hidden panel and unchanged history. A separate copy-back exercises pixel-based suppression
# after Clipboard.GetImage/GDI+ reencoding, followed by genuine copies while its TTL is still live.
# The clipboard is materialized before the test and restored after the probe watcher exits.
param(
    [string]$Exe = '',
    [ValidateRange(0, 300)][int]$IdleSeconds = 30
)

if ($env:OS -ne 'Windows_NT') { throw 'image-paste.ps1 requires Windows.' }
if ([Threading.Thread]::CurrentThread.GetApartmentState() -ne 'STA') { throw 'Run powershell with -STA.' }
. "$PSScriptRoot\common.ps1"

Add-Type -ReferencedAssemblies System.Windows.Forms, System.Drawing @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Text;
using System.Windows.Forms;

/// Own-window target: records only real paste commands, never a scripted clipboard read as a paste.
public sealed class ImagePasteTarget : Form {
    public Control Box;
    public Bitmap PastedImage;
    public int Pastes;
    public string PasteError;

    sealed class Surface : Control {
        readonly ImagePasteTarget owner;
        public Surface(ImagePasteTarget owner) {
            this.owner = owner;
            SetStyle(ControlStyles.Selectable, true);
            TabStop = true;
            Dock = DockStyle.Fill;
            BackColor = Color.White;
        }
        protected override void WndProc(ref Message message) {
            if (message.Msg == 0x0302) { owner.ReceivePaste(); return; }
            base.WndProc(ref message);
        }
    }

    public ImagePasteTarget() {
        Text = "kwikpaste-probe-image-paste-target";
        StartPosition = FormStartPosition.Manual;
        Box = new Surface(this);
        Controls.Add(Box);
    }

    protected override bool ProcessCmdKey(ref Message message, Keys keyData) {
        if (keyData == (Keys.Control | Keys.V)) { ReceivePaste(); return true; }
        return base.ProcessCmdKey(ref message, keyData);
    }

    protected override void WndProc(ref Message message) {
        if (message.Msg == 0x0302) { ReceivePaste(); return; }
        base.WndProc(ref message);
    }

    void ReceivePaste() {
        Pastes++;
        try {
            using (var image = Clipboard.GetImage()) {
                if (image == null) { PasteError = "Clipboard.GetImage returned null"; return; }
                if (PastedImage != null) PastedImage.Dispose();
                PastedImage = new Bitmap(image);
            }
        } catch (Exception error) { PasteError = error.ToString(); }
    }

    /// Exact RGBA and left/right position comparison catches swapped halves and lost color/alpha.
    public int WrongPixels(int width, int height, Color left, Color right) {
        if (PastedImage == null || PastedImage.Width != width || PastedImage.Height != height) return -1;
        int wrong = 0;
        for (int y = 0; y < height; y++) for (int x = 0; x < width; x++) {
            if (PastedImage.GetPixel(x, y).ToArgb() != (x < width / 2 ? left : right).ToArgb()) wrong++;
        }
        return wrong;
    }

    public byte[] ReencodedPng() {
        if (PastedImage == null) throw new InvalidOperationException("no pasted image to reencode");
        using (var stream = new MemoryStream()) {
            PastedImage.Save(stream, ImageFormat.Png);
            return stream.ToArray();
        }
    }

    protected override void Dispose(bool disposing) {
        if (disposing && PastedImage != null) { PastedImage.Dispose(); PastedImage = null; }
        base.Dispose(disposing);
    }
}

public static class ImagePasteFixture {
    /// Add valid non-pixel PNG metadata so an OS-decoded/GDI+-encoded copy must have different bytes.
    public static byte[] WithMarker(byte[] png, string marker) {
        byte[] data = Encoding.ASCII.GetBytes("kwikpaste-probe\0" + marker);
        byte[] chunk = new byte[data.Length + 12];
        int length = data.Length;
        for (int i = 0; i < 4; i++) chunk[i] = (byte)(length >> (24 - i * 8));
        Encoding.ASCII.GetBytes("tEXt").CopyTo(chunk, 4);
        data.CopyTo(chunk, 8);
        uint crc = 0xffffffff;
        for (int i = 4; i < chunk.Length - 4; i++) {
            crc ^= chunk[i];
            for (int bit = 0; bit < 8; bit++) crc = (crc >> 1) ^ ((crc & 1) != 0 ? 0xedb88320u : 0);
        }
        crc ^= 0xffffffff;
        for (int i = 0; i < 4; i++) chunk[chunk.Length - 4 + i] = (byte)(crc >> (24 - i * 8));
        byte[] result = new byte[png.Length + chunk.Length];
        Array.Copy(png, 0, result, 0, png.Length - 12);
        Array.Copy(chunk, 0, result, png.Length - 12, chunk.Length);
        Array.Copy(png, png.Length - 12, result, png.Length - 12 + chunk.Length, 12);
        return result;
    }

    /// Eagerly copy formats: the IDataObject returned by GetDataObject can otherwise refer to a
    /// clipboard owner that disappears when the fixture is written. Abort if a format cannot be read.
    public static DataObject Snapshot() {
        var snapshot = new DataObject();
        var source = Clipboard.GetDataObject();
        if (source == null) return snapshot;
        foreach (string format in source.GetFormats(false)) {
            object data = source.GetData(format, false);
            if (data == null) throw new InvalidOperationException("cannot snapshot clipboard format " + format);
            var image = data as Image;
            var stream = data as MemoryStream;
            var bytes = data as byte[];
            if (image != null) data = new Bitmap(image);
            else if (stream != null) data = new MemoryStream(stream.ToArray());
            else if (bytes != null) data = bytes.Clone();
            snapshot.SetData(format, false, data);
        }
        return snapshot;
    }
}
'@

$Exe = Resolve-AppExe $Exe
$running = @(Get-Process KwikPaste -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) { throw 'KwikPaste is already running; this probe will not stop it or use its clipboard watcher. Run on an isolated Windows runner.' }
Assert-Desktop -IdleSeconds $IdleSeconds -NeedsInput

$results = New-ResultDir 'image-paste'
$report = New-Object System.Collections.Generic.List[string]
$failures = New-Object System.Collections.Generic.List[string]
function Note([string]$Line) { $report.Add($Line); Write-Host $Line }
function Check([string]$Name, [bool]$Passed, [string]$Detail = '') {
    Note ("  {0}: {1}{2}" -f $Name, $(if ($Passed) { 'ok' } else { 'FAIL' }), $(if ($Detail) { " ($Detail)" } else { '' }))
    if (-not $Passed) { $failures.Add($Name) }
}

function Get-HistoryCount {
    [void](Get-ProbeEvents 'count')
    Send-ProbeCommand $Exe '--selftest-count'
    $event = Wait-ProbeEvent 'count' 3000
    if ($null -eq $event) { throw 'The app did not report its history count.' }
    return [long]$event.total
}

function Wait-ImageCapture([int]$Width, [int]$Height, [int]$TimeoutMs = 3000) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    while ($watch.ElapsedMilliseconds -lt $TimeoutMs) {
        foreach ($event in @(Get-ProbeEvents 'clipboard')) {
            if ($event.item.kind -eq 'image' -and $event.item.width -eq $Width -and $event.item.height -eq $Height) { return $event }
            throw "Unexpected watcher capture: $($event | ConvertTo-Json -Compress -Depth 8)"
        }
        [Probe]::Pump(20)
    }
    return $null
}

$userForeground = [Probe]::GetForegroundWindow()
$userCursor = New-Object Probe+POINT
[void][Probe]::GetCursorPos([ref]$userCursor)
$userClipboard = [ImagePasteFixture]::Snapshot()
$oldSelftest = $env:KWIKPASTE_SELFTEST
$oldProbeLog = $env:KWIKPASTE_PROBE_LOG
$app = $null
$target = $null
$identityConfirmed = $false
$startedAt = Get-Date
try {
    $app = Start-ProbeApp $Exe $results -Arguments @('--selftest-real-clipboard')
    $startup = (Get-Content $app.Stderr -Encoding UTF8) -join "`n"
    if ($startup -notmatch 'core started: com\.fastthree\.kwikpaste\.native-dev\.selftest-platform Dev,') {
        throw 'The executable did not confirm native-dev selftest-platform identity; refusing clipboard writes.'
    }
    if ([int]$app.Ready.pid -ne $app.Process.Id) { throw 'Ready event does not belong to the process this probe launched.' }
    $identityConfirmed = $true
    Note "probe app pid $($app.Process.Id), native-dev selftest-platform; generated fixture <= 96x48 pixels"
    Send-ProbeCommand $Exe '--selftest-settings={"clipboard":{"capture":{"image":true},"feedback":{"copySound":false},"filters":{"excludedAppIds":[]}},"shortcuts":{"pauseAppIds":[],"pauseInFullscreen":false}}'
    [Probe]::Pump(300)

    $target = New-Object ImagePasteTarget
    $work = [Probe]::PrimaryWorkArea()
    $target.Bounds = New-Object System.Drawing.Rectangle(($work[0] + 60), ($work[1] + 60), 480, 240)
    $target.TopMost = $true
    $target.Show()
    [Probe]::Pump(300)
    $center = $target.Box.PointToScreen((New-Object System.Drawing.Point(150, 80)))
    if (-not [Probe]::ClickIntoForegroundAt($target.Handle, $center.X, $center.Y)) { throw 'The image target could not take the foreground.' }
    $target.TopMost = $false
    [void]$target.Box.Focus()

    [void](Get-ProbeEvents 'clipboard')
    $before = Get-HistoryCount
    $width = Get-Random -Minimum 64 -Maximum 97
    $height = 48
    # Reencoding removes the metadata nonce; randomized pixels keep repeat runs distinct in the DB.
    $leftRgb = Get-Random -Minimum 0 -Maximum 16777216
    do { $rightRgb = Get-Random -Minimum 0 -Maximum 16777216 } while ($rightRgb -eq $leftRgb)
    $left = [System.Drawing.Color]::FromArgb(255, ($leftRgb -shr 16), (($leftRgb -shr 8) -band 255), ($leftRgb -band 255))
    $right = [System.Drawing.Color]::FromArgb(255, ($rightRgb -shr 16), (($rightRgb -shr 8) -band 255), ($rightRgb -band 255))
    $png = [ImagePasteFixture]::WithMarker([Clip]::Png($width, $height, $left, $right), [guid]::NewGuid().ToString())
    $dib = [Clip]::Dib($width, $height, $left, $right)
    [IO.File]::WriteAllBytes((Join-Path $results 'original.png'), $png)
    $formats = New-Object 'System.Collections.Generic.Dictionary[string,byte[]]'
    $formats['PNG'] = $png
    $formats['#8'] = $dib
    [Probe]::RequireForegroundOf($target.Handle, [IntPtr]::Zero, 'the generated image copy')
    [Clip]::Set($formats)
    $captured = Wait-ImageCapture $width $height
    if ($null -eq $captured) { throw 'The real clipboard watcher did not store the generated PNG + CF_DIB image.' }
    $sourceId = $captured.item.sourceAppId
    if (-not $sourceId) { throw 'The captured image has no sourceAppId; cannot isolate the reencoding guard test.' }
    $stored = Get-HistoryCount
    Check 'watcher stored one new image' ($stored -eq $before + 1 -and -not $captured.deduplicated) "$before -> $stored; id $($captured.item.id)"
    [void](Get-ProbeEvents 'pasted')
    [void](Get-ProbeEvents 'shown')
    [void](Get-ProbeEvents 'clipboard')
    Send-ProbeCommand $Exe '--selftest-show'
    if ($null -eq (Wait-ProbeEvent 'shown' 3000)) { throw 'The platform panel did not show.' }
    [Probe]::Pump(150)
    [Probe]::TapFor($target.Handle, $app.Hwnd, [uint16]0x0D)
    $pasted = Wait-ProbeEvent 'pasted' 3000
    [Probe]::Pump(700)
    $extraPastes = @(Get-ProbeEvents 'pasted')
    $writebackCaptures = @(Get-ProbeEvents 'clipboard')
    Check 'Enter injected paste for the captured image once' ($null -ne $pasted -and $pasted.id -eq $captured.item.id -and $pasted.kind -eq 'item' -and $extraPastes.Count -eq 0)
    Check 'target received exactly one real paste command' ($target.Pastes -eq 1 -and -not $target.PasteError) "count $($target.Pastes); $($target.PasteError)"
    Check 'Clipboard.GetImage delivered exact dimensions and all two-color pixels' ($target.WrongPixels($width, $height, $left, $right) -eq 0) "expected ${width}x${height}"
    Check 'image target kept the foreground' ([Probe]::GetForegroundWindow() -eq $target.Handle)
    Check 'panel hidden after image paste' (-not [Probe]::IsWindowVisible($app.Hwnd))
    $writtenFormats = [Clip]::Formats()
    Check 'image writeback offers PNG and CF_DIB' ($writtenFormats -contains 'PNG' -and $writtenFormats -contains '#8') ($writtenFormats -join ',')
    Check 'image paste writeback added no history or watcher capture' ((Get-HistoryCount) -eq $stored -and $writebackCaptures.Count -eq 0)
    if ($null -eq $target.PastedImage) { throw 'No received image for the OS reencoding check.' }

    $rewritten = $target.ReencodedPng()
    [IO.File]::WriteAllBytes((Join-Path $results 'reencoded.png'), $rewritten)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $originalHash = [BitConverter]::ToString($sha.ComputeHash($png))
        $rewrittenHash = [BitConverter]::ToString($sha.ComputeHash($rewritten))
    } finally { $sha.Dispose() }
    if ($originalHash -eq $rewrittenHash) { throw 'GDI+ did not change the PNG encoding; pixel fallback was not exercised.' }
    Note "OS image decode/GDI+ reencode changed PNG bytes: $($png.Length) -> $($rewritten.Length) bytes"

    # The ordinary writeback above already consumed its one-shot guard. Exclude this target's source
    # for a fresh copy-back so that notification cannot consume the raw-hash registration first.
    $excluded = @{ clipboard = @{ filters = @{ excludedAppIds = @($sourceId) } } } | ConvertTo-Json -Compress -Depth 6
    Send-ProbeCommand $Exe "--selftest-settings=$excluded"
    [Probe]::Pump(300)
    [void](Get-ProbeEvents 'copied')
    [void](Get-ProbeEvents 'clipboard')
    [Probe]::RequireForegroundOf($target.Handle, [IntPtr]::Zero, 'the image guard check')
    $guardClock = [Diagnostics.Stopwatch]::StartNew()
    Send-ProbeCommand $Exe "--selftest-copy-item=$($captured.item.id)"
    $copied = Wait-ProbeEvent 'copied' 1000
    if ($null -eq $copied -or $copied.id -ne $captured.item.id) { throw 'Copy-back did not confirm the expected image.' }
    [Probe]::Pump(150)
    Send-ProbeCommand $Exe '--selftest-settings={"clipboard":{"filters":{"excludedAppIds":[]}}}'
    if ($guardClock.ElapsedMilliseconds -ge 1500) { throw "Copy-back and exclusion reset took $($guardClock.ElapsedMilliseconds) ms; cannot reliably test the 2 s guard TTL." }
    $formats['PNG'] = $rewritten
    [Probe]::RequireForegroundOf($target.Handle, [IntPtr]::Zero, 'the reencoded image copy')
    [Clip]::Set($formats)
    Note "reencoded clipboard published within $($guardClock.ElapsedMilliseconds) ms of copy-back start"
    [Probe]::Pump(300)
    $suppressedCaptures = @(Get-ProbeEvents 'clipboard')
    Check 'same pixels with different PNG bytes produced no watcher capture' ($suppressedCaptures.Count -eq 0) "captures $($suppressedCaptures.Count)"

    # Switching back to the original bytes bypasses RepeatFilter's 1 s same-hash window. The pixel
    # match must have consumed the raw registration too, so this genuine copy must reach persistence
    # BEFORE the 2 s TTL expires (deduplicated into the original row). A reset left excluded fails here.
    if ($guardClock.ElapsedMilliseconds -ge 1700) { throw 'Not enough guard TTL remains to verify one-shot consumption.' }
    $formats['PNG'] = $png
    [Probe]::RequireForegroundOf($target.Handle, [IntPtr]::Zero, 'the original image recopy')
    [Clip]::Set($formats)
    $remainingMs = 2000 - $guardClock.ElapsedMilliseconds
    if ($remainingMs -le 0) { throw 'Original image recopy exceeded the guard TTL; consumption was not verified.' }
    $originalAgain = Wait-ImageCapture $width $height ([int]$remainingMs)
    Check 'pixel match consumed the original raw-hash guard before TTL expired' ($null -ne $originalAgain -and $originalAgain.item.id -eq $captured.item.id -and $originalAgain.deduplicated -and $guardClock.ElapsedMilliseconds -lt 2000) "elapsed $($guardClock.ElapsedMilliseconds) ms"

    # The intervening original hash also bypasses RepeatFilter for the rewritten PNG. A second pixel
    # registration left behind would suppress it; a consumed guard permits one new encoded history row.
    # Keep BOTH recopy captures inside the same TTL. Count's second-process RPC happens only afterward.
    if ($guardClock.ElapsedMilliseconds -ge 1800) { throw 'Not enough guard TTL remains to verify the final pixel registration was consumed.' }
    $formats['PNG'] = $rewritten
    [Probe]::RequireForegroundOf($target.Handle, [IntPtr]::Zero, 'the genuine reencoded image copy')
    [Clip]::Set($formats)
    $remainingMs = 2000 - $guardClock.ElapsedMilliseconds
    if ($remainingMs -le 0) { throw 'Final reencoded image recopy exceeded the guard TTL; consumption was not verified.' }
    $genuine = Wait-ImageCapture $width $height ([int]$remainingMs)
    $finalCaptureMs = $guardClock.ElapsedMilliseconds
    if ($finalCaptureMs -ge 2000) { throw "Final reencoded capture reached $finalCaptureMs ms; cannot distinguish consumption from TTL expiry." }
    $after = Get-HistoryCount
    Check 'later real copy of the reencoded image is captured before guard TTL expires' ($null -ne $genuine -and -not $genuine.deduplicated -and $genuine.item.id -ne $captured.item.id -and $finalCaptureMs -lt 2000 -and $after -eq $stored + 1) "$stored -> $after; capture at $finalCaptureMs ms"
    Check 'guard sequence adds only the final genuine reencoded history row' ($after -eq $stored + 1 -and $null -ne $genuine -and -not $genuine.deduplicated)
    $guardPastes = @(Get-ProbeEvents 'pasted')
    Check 'guard checks did not send another paste' ($target.Pastes -eq 1 -and $guardPastes.Count -eq 0)
} catch {
    $failures.Add("ERROR: $($_.Exception.Message)")
    Note "ERROR: $($_.Exception.Message)"
} finally {
    $exitConfirmed = $false
    try {
        if ($null -ne $app) {
            if (-not $app.Process.HasExited) {
                if (-not $identityConfirmed) { throw 'Cannot stop an unconfirmed app identity; clipboard restoration is blocked.' }
                try { Send-ProbeCommand $Exe '--selftest-settings={"clipboard":{"filters":{"excludedAppIds":[]}}}' } catch {
                    Note "reset probe exclusions: $($_.Exception.Message)"
                }
                try { Stop-ProbeApp $app $Exe } catch {
                    $failures.Add("cleanup: $($_.Exception.Message)")
                    Note "cleanup: $($_.Exception.Message)"
                    if (-not $app.Process.HasExited) { $app.Process.Kill() }
                }
            }
            # common.ps1's timeout path calls Kill without waiting. Never restore while its watcher
            # can still be alive, even when Stop-ProbeApp returned or the process already looked exited.
            if (-not $app.Process.WaitForExit(3000) -or -not $app.Process.HasExited) {
                throw "Own probe PID $($app.Process.Id) did not fully exit; clipboard restoration is blocked."
            }
        } else {
            # Ready can time out before Start-ProbeApp returns its process. Match this invocation's
            # exact exe, both test flags and creation time; require its own log to confirm dev identity.
            $owners = @(Get-CimInstance Win32_Process -Filter "Name = 'KwikPaste.exe'" | Where-Object {
                $_.ExecutablePath -eq $Exe -and $_.CommandLine -match '--selftest-platform' -and
                $_.CommandLine -match '--selftest-real-clipboard' -and $_.CreationDate -ge $startedAt
            })
            if ($owners.Count -gt 1) { throw 'Multiple possible probe owners; refusing to kill an ambiguous PID or restore the clipboard.' }
            if ($owners.Count -eq 1) {
                $startup = (Get-Content (Join-Path $results 'app-stderr.txt') -Encoding UTF8 -ErrorAction SilentlyContinue) -join "`n"
                if ($startup -notmatch 'core started: com\.fastthree\.kwikpaste\.native-dev\.selftest-platform Dev,') {
                    throw 'Timed-out probe identity is unconfirmed; refusing to kill its PID or restore the clipboard.'
                }
                $process = Get-Process -Id $owners[0].ProcessId -ErrorAction SilentlyContinue
                if ($null -ne $process) {
                    $creationDeltaMs = [Math]::Abs(($process.StartTime.ToUniversalTime() - $owners[0].CreationDate.ToUniversalTime()).TotalMilliseconds)
                    if ($process.Path -ne $Exe -or $creationDeltaMs -gt 1) { throw 'Timed-out probe PID ownership changed; refusing to kill it or restore the clipboard.' }
                    if (-not $process.HasExited) { $process.Kill() }
                    if (-not $process.WaitForExit(3000) -or -not $process.HasExited) {
                        throw "Own timed-out probe PID $($process.Id) did not fully exit; clipboard restoration is blocked."
                    }
                }
            }
        }
        $exitConfirmed = $true
    } catch {
        $failures.Add("process cleanup: $($_.Exception.Message)")
        Note "process cleanup: $($_.Exception.Message)"
    } finally {
        try {
            if ($null -ne $target) {
                try { $target.Close() } finally { $target.Dispose(); [Probe]::Pump(100) }
            }
        } catch {
            $failures.Add("target cleanup: $($_.Exception.Message)")
            Note "target cleanup: $($_.Exception.Message)"
        } finally {
            try {
                if ($exitConfirmed) { [System.Windows.Forms.Clipboard]::SetDataObject($userClipboard, $true, 10, 100) }
                else { Note 'Clipboard not restored: the own probe process exit could not be confirmed.' }
            } catch {
                $failures.Add("clipboard restore: $($_.Exception.Message)")
                Note "clipboard restore: $($_.Exception.Message)"
            } finally {
                try {
                    $env:KWIKPASTE_SELFTEST = $oldSelftest
                    $env:KWIKPASTE_PROBE_LOG = $oldProbeLog
                } finally {
                    try {
                        [void][Probe]::SetCursorPos($userCursor.X, $userCursor.Y)
                        if ($userForeground -ne [IntPtr]::Zero) { [void][Probe]::SetForegroundWindow($userForeground) }
                    } finally {
                        $report | Set-Content (Join-Path $results 'summary.txt') -Encoding UTF8
                        Write-Host "results: $results"
                    }
                }
            }
        }
    }
}

if ($failures.Count -gt 0) { Write-Host "FAILED: $($failures -join '; ')"; exit 1 }
Write-Host 'PASSED'
