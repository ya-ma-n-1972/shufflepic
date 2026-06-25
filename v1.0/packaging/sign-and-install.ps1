# B5: ローカル動作確認用に MSIX を自己署名してサイドロード install する。
# 【Store 配布では不要】Store は Microsoft 証明書で再署名する。これはローカルテスト専用。
# 【要・管理者権限】証明書を信頼ストアへ入れるため、管理者の PowerShell で実行する。
# 使い方:
#   pwsh -File sign-and-install.ps1 -Publisher "<CN=...>"   # マニフェストの Publisher と完全一致
param(
    [Parameter(Mandatory)][string]$Publisher,
    [string]$Msix,
    [string]$Password = "shufflepic-test"
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $Msix) { $Msix = Join-Path $here "ShufflePic.msix" }
if (-not (Test-Path $Msix)) { throw "MSIX が見つかりません: $Msix（先に make-msix.ps1）" }

# 1) 自己署名証明書（Subject = マニフェストの Publisher と完全一致が必須）
$cert = New-SelfSignedCertificate -Type Custom -Subject $Publisher `
    -KeyUsage DigitalSignature -FriendlyName "ShufflePic Test (local only)" `
    -CertStoreLocation "Cert:\CurrentUser\My" `
    -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
$thumb = $cert.Thumbprint
$pfx = Join-Path $here "shufflepic-test.pfx"
$cer = Join-Path $here "shufflepic-test.cer"
$pw = ConvertTo-SecureString -String $Password -Force -AsPlainText
Export-PfxCertificate -Cert "Cert:\CurrentUser\My\$thumb" -FilePath $pfx -Password $pw | Out-Null
Export-Certificate -Cert "Cert:\CurrentUser\My\$thumb" -FilePath $cer | Out-Null

# 2) 証明書を信頼（TrustedPeople・要管理者）。Windows がサイドロード署名を信頼するため。
Import-Certificate -FilePath $cer -CertStoreLocation "Cert:\LocalMachine\TrustedPeople" | Out-Null

# 3) 署名は「コピー」に対して行う（アップロード用の未署名 ShufflePic.msix を温存）。
$signed = Join-Path $here "ShufflePic-signed.msix"
Copy-Item $Msix $signed -Force

# signtool（Windows SDK から探す）
$sdk = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Directory -ErrorAction SilentlyContinue |
    Where-Object { Test-Path (Join-Path $_.FullName "x64\signtool.exe") } |
    Sort-Object Name -Descending | Select-Object -First 1
$signtool = if ($sdk) { Join-Path $sdk.FullName "x64\signtool.exe" } else { "signtool.exe" }
& $signtool sign /fd SHA256 /a /f $pfx /p $Password $signed
if ($LASTEXITCODE -ne 0) { throw "signtool に失敗しました（exit $LASTEXITCODE）" }

# 4) インストール（署名コピーを使用）
Add-AppxPackage -Path $signed
"インストール完了。スタートメニューの ShufflePic から起動して動作確認してください。"
"確認ポイント: %APPDATA%\ShufflePic への状態保存・再起動復元 / delete・oversized への移動 / フォルダ選択ダイアログ。"
