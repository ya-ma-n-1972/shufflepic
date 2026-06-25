# B4: ShufflePic の MSIX パッケージを生成する。
# 使い方（B1 で取得した Product Identity を渡す）:
#   pwsh -File make-msix.ps1 -IdentityName "<Package/Identity/Name>" `
#        -Publisher "<CN=...>" -PublisherDisplayName "<発行者表示名>"
# 値を省略するとプレースホルダのまま生成し、警告を出す（ローカル構造確認用）。
param(
    [string]$IdentityName = "__IDENTITY_NAME__",
    [string]$Publisher = "__PUBLISHER__",
    [string]$PublisherDisplayName = "__PUBLISHER_DISPLAY_NAME__",
    [switch]$SkipBuild
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$crate = Resolve-Path (Join-Path $here "..")
$exe = Join-Path $crate "target\release\shufflepic.exe"

if (-not $SkipBuild) {
    Push-Location $crate
    cargo build --release
    Pop-Location
}
if (-not (Test-Path $exe)) { throw "exe が見つかりません: $exe（先に cargo build --release）" }

# マニフェスト（プレースホルダ置換）
$manifest = (Get-Content -Raw (Join-Path $here "AppxManifest.xml")).
    Replace("__IDENTITY_NAME__", $IdentityName).
    Replace("__PUBLISHER__", $Publisher).
    Replace("__PUBLISHER_DISPLAY_NAME__", $PublisherDisplayName)
if ($manifest -match "__IDENTITY_NAME__|__PUBLISHER__|__PUBLISHER_DISPLAY_NAME__") {
    Write-Warning "Identity 値が未設定です。B1（Partner Center の製品 ID）の値を引数で渡してください。"
}

# ステージング（exe + Assets + AppxManifest）
$stage = Join-Path $here "staging"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
$manifest | Set-Content -LiteralPath (Join-Path $stage "AppxManifest.xml") -Encoding utf8
Copy-Item (Join-Path $here "Assets") (Join-Path $stage "Assets") -Recurse
Copy-Item $exe (Join-Path $stage "shufflepic.exe")

# makeappx を Windows SDK から探す
$sdk = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Directory -ErrorAction SilentlyContinue |
    Where-Object { Test-Path (Join-Path $_.FullName "x64\makeappx.exe") } |
    Sort-Object Name -Descending | Select-Object -First 1
$makeappx = if ($sdk) { Join-Path $sdk.FullName "x64\makeappx.exe" } else { "makeappx.exe" }

$out = Join-Path $here "ShufflePic.msix"
& $makeappx pack /d $stage /p $out /o
if ($LASTEXITCODE -ne 0) { throw "makeappx に失敗しました（exit $LASTEXITCODE）" }
"MSIX 生成完了: $out"
