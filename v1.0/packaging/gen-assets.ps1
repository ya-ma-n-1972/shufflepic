# B2: ShufflePic の MSIX タイル/ロゴを 1024 マスターから生成する。
# 使い方: pwsh -File gen-assets.ps1
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$master = Resolve-Path (Join-Path $here "..\..\assets\shufflepic-icon-1024.png")
$outdir = Join-Path $here "Assets"
New-Item -ItemType Directory -Force -Path $outdir | Out-Null
$src = [System.Drawing.Image]::FromFile($master)

function Save-Square([int]$size, [string]$name) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = 'HighQualityBicubic'
    $g.PixelOffsetMode = 'HighQuality'
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.DrawImage($src, 0, 0, $size, $size)
    $g.Dispose()
    $bmp.Save((Join-Path $outdir $name), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}

function Save-Wide([int]$w, [int]$h, [string]$name) {
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = 'HighQualityBicubic'
    $g.PixelOffsetMode = 'HighQuality'
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)
    $icon = [int]($h * 0.9)
    $x = [int](($w - $icon) / 2)
    $y = [int](($h - $icon) / 2)
    $g.DrawImage($src, $x, $y, $icon, $icon)
    $g.Dispose()
    $bmp.Save((Join-Path $outdir $name), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}

# scale 接尾辞と倍率
$scales = [ordered]@{ '' = 1.0; '.scale-125' = 1.25; '.scale-150' = 1.5; '.scale-200' = 2.0; '.scale-400' = 4.0 }
# 正方形ロゴ（基準サイズ）
$squares = [ordered]@{ 'Square44x44Logo' = 44; 'Square71x71Logo' = 71; 'Square150x150Logo' = 150; 'Square310x310Logo' = 310; 'StoreLogo' = 50 }

foreach ($n in $squares.Keys) {
    $base = $squares[$n]
    foreach ($s in $scales.Keys) {
        $px = [int][Math]::Round($base * $scales[$s])
        Save-Square $px "$n$s.png"
    }
}
# ワイドタイル
foreach ($s in $scales.Keys) {
    $w = [int][Math]::Round(310 * $scales[$s])
    $h = [int][Math]::Round(150 * $scales[$s])
    Save-Wide $w $h "Wide310x150Logo$s.png"
}
# アプリ一覧/タスクバー用 targetsize（プレート無し）
foreach ($t in 16, 24, 32, 48, 256) {
    Save-Square $t "Square44x44Logo.targetsize-$t.png"
}

$src.Dispose()
"$((Get-ChildItem $outdir -Filter *.png).Count) files generated in $outdir"
