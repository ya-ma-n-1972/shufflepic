# ShufflePic v1.0 — MSIX パッケージング

Microsoft ストア公開用の MSIX を作る一式（公開計画書のフェーズ B）。

## ファイル
- `gen-assets.ps1` … 1024 マスターから MSIX タイル/ロゴ（`Assets\`）を生成（B2）。**生成済み**。
- `AppxManifest.xml` … マニフェスト（Identity はプレースホルダ。B1 後に差し替え）（B3）。
- `make-msix.ps1` … `.msix` を生成（B4）。
- `sign-and-install.ps1` … ローカルテスト用に自己署名→信頼→install（B5、要管理者）。
- `Assets\` … 生成済みロゴ（35 ファイル）。

## 手順

1. **B1（Partner Center）**：アプリ名「ShufflePic」を予約 →「製品管理 → 製品 ID」で次を取得。
   - パッケージ/識別名（Identity Name）
   - パッケージ/発行者（Publisher、`CN=...`）
   - パッケージ/発行者表示名（PublisherDisplayName）

2. **B4 パッケージ生成**（Developer Command Prompt 推奨＝SDK にパスが通る）:
   ```
   pwsh -File make-msix.ps1 -IdentityName "<Identity Name>" -Publisher "CN=..." -PublisherDisplayName "<表示名>"
   ```
   → `ShufflePic.msix` が出力。

3. **B5 ローカル動作確認**（管理者 PowerShell）:
   ```
   pwsh -File sign-and-install.ps1 -Publisher "CN=..."   # マニフェストの Publisher と完全一致
   ```
   → スタートメニューの ShufflePic で起動し、**%APPDATA%\ShufflePic への状態保存・再起動復元 /
   delete・oversized への移動 / フォルダ選択**を確認。

4. **C 提出**：Partner Center で `.msixupload`（または `.msix`）をアップロード。
   - `runFullTrust` の**理由**を記入（例：「ユーザー指定フォルダ内の画像の閲覧・並べ替え、および退避フォルダ
     への移動を行う通常の Win32 デスクトップ機能のため」）。
   - 掲載情報・スクリーンショット・年齢レーティング・価格・市場を設定し提出 → 認定 → 公開。

## Identity

`Name` / `Publisher` / `PublisherDisplayName` は Partner Center の「製品管理 → 製品 ID」から取得し、
`make-msix.ps1` に引数で渡す（リポジトリには値を記録しない）。`AppxManifest.xml` 側はプレースホルダのまま。

## 生成物
- `ShufflePic.msix` … **未署名**。**Partner Center へのアップロード用**（Store が再署名）。
- `ShufflePic-signed.msix` … `sign-and-install.ps1` が作る**署名コピー**。**ローカルテスト専用**。

## メモ
- 署名は **Store が再署名**するため、提出版（`ShufflePic.msix`）は自己署名不要（B5 はローカル確認専用）。
- `ProcessorArchitecture` は x64。ARM64 等も出す場合はビルドとマニフェストを分けて複数パッケージ化。
- `BackgroundColor=#2B2B2B`（白縁取りアイコンが映える中間〜濃色）。好みで変更可。
- タイルサイズの過不足は Partner Center / マニフェスト デザイナーの検証で最終確認する。
