# Test fixture for ocr_pipeline.rs: renders a PNG with a sentinel word using
# System.Drawing, same technique as the M4 technical-spike script
# (scratchpad/ocr-spike/make_images.ps1), parameterized for reuse from Rust tests.
#
# ASCII-only comments on purpose: Windows PowerShell 5.1 (powershell.exe, as
# opposed to pwsh) guesses a script's encoding from the system ANSI codepage
# when the file has no UTF-8 BOM, and will mis-tokenize non-ASCII comment text
# badly enough to break parsing of unrelated code below it.
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Sentinel,
    [int]$Width = 900,
    [int]$Height = 220,
    [string]$FontFamily = "Consolas",
    [int]$FontSize = 26
)

Add-Type -AssemblyName System.Drawing

$bmp = New-Object System.Drawing.Bitmap $Width, $Height
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
$g.Clear([System.Drawing.Color]::White)

$font = New-Object System.Drawing.Font($FontFamily, $FontSize, [System.Drawing.FontStyle]::Regular)
$brush = [System.Drawing.Brushes]::Black
$g.DrawString($Sentinel, $font, $brush, 20, 20)

$parent = Split-Path -Parent $Path
if ($parent -and -not (Test-Path $parent)) {
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
}

$bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose()
$bmp.Dispose()
$font.Dispose()
