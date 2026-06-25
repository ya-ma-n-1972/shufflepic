# ShufflePic v1.0 詳細設計書

| 項目 | 内容 |
| --- | --- |
| 種別 | 詳細設計（v1.0 向け） |
| 対象読者 | 開発者 |
| 前提 | 前身（基盤）の実装・設計文書（特に「再設計 詳細設計書」）を理解していること |
| 関連 | 要求は [v1.0 要求定義書](./ShufflePic%20v1.0%20要求定義書.md)、F-1 は [v1.0 デコード非同期化 設計提案](./ShufflePic%20v1.0%20デコード非同期化%20設計提案.md) |
| 状態 | 詳細設計（スコープ確定 F-1〜F-8） |

---

## 0. 目的と範囲

本書は v1.0 要求定義書（F-1〜F-8）を実装直前の粒度へ落とし込む。前身 詳細設計書（以下「基盤詳細」）を
基盤とし、**v2 からの差分のみ**を定義する。基盤詳細で定義済みかつ変更のない事項（隔離移動の衝突回避、
`pending_free` 遅延 drop、rescan の昇格判定、VRAM 窓の連続ミラー不変条件など）は本書では再掲せず、
該当箇所を「基盤詳細 §X」として参照する。

スコープは F-1〜F-8 に限る。表示レイアウトの抜本変更、ディスクデコードキャッシュ、3 枚以上の同時表示は
扱わない。

### 0.1 v1.0 で変わる前提（重要）

- 前身 の「**内部状態はメモリのみで永続化しない**」という前提は、F-7 により**部分的に撤回**する。
  設定値とサイクル状態をディスクへ保存し、再起動後に同一フォルダなら続きから再開する（§4.7）。
- 前身 の「**表示は常に 2 枚**」という前提は、F-7 により**表示枚数 1 または 2（可変）**へ一般化する（§3.4 / §4.7）。
- 前身 の「削除取り消し（Undo）」は F-3 により**廃止**する（§4.3）。

---

## 1. 機能一覧と影響モジュール

| # | 機能 | 主な変更モジュール | 新規モジュール |
| --- | --- | --- | --- |
| F-1 | デコード非同期化 | `cache.rs`, `app.rs`, `image_loader.rs`, `main.rs` | `decoder.rs` |
| F-2 | メニュー中の自動送り一時停止（残り時間から再開） | `app.rs` | — |
| F-3 | Undo 廃止 | `app.rs`, `quarantine.rs`, `main.rs` | — |
| F-4 | 末尾到達でのサイクル停止修復 | `app.rs`, `playback.rs`, `cache.rs` | — |
| F-5 | 「表示済み」枚数の表示 | `app.rs` | — |
| F-6 | 右クリックに「再生／一時停止」追加 | `app.rs` | — |
| F-7 | 管理画面＋設定＋永続化（表示枚数/間隔） | `app.rs`, `main.rs`, `playback.rs`, `cache.rs` | `settings.rs`, `persist.rs` |
| F-8 | 巨大画像の自動退避 | `image_loader.rs`, `cache.rs`/`decoder.rs`, `app.rs`, `quarantine.rs`（移動を再利用） | — |

F-1 は実装段階を分離しやすいが、表示枚数、F-4、フォルダ変更、永続化復元後の候補列と統合する必要がある。
実装順は §9 を参照（F-3/F-4/F-5/F-6 → F-2 → F-7 → F-1 を推奨）。

---

## 2. モジュール構成（v1.0）

```text
src/
├── main.rs          # 起動：設定/状態ロード → 入力フォルダ確定 → App 生成
├── app.rs           # eframe::App。update ループ、入力、描画、各機能の適用
├── playback.rs      # play_order/cursor/シャッフル/サイクル境界（F-4 で共通遷移を追加）
├── cache.rs         # VRAM 窓と補充（F-1 で ready/inflight、F-7 で display_count 対応）
├── quarantine.rs    # 削除（隔離移動）のみ。Undo 関連を削除（F-3）
├── scanner.rs       # 起動時/定期 rescan（変更なし。基盤詳細 §11 を踏襲）
├── image_loader.rs  # 拡張子判定・デコード（F-1 で ColorImage 返却を追加）
├── decoder.rs       # 【新規】デコードワーカープールとチャネル（F-1）
├── settings.rs      # 【新規】設定値（フォルダ/間隔/枚数）と管理画面 UI（F-7）
└── persist.rs       # 【新規】設定＋サイクル状態のディスク保存/復元（F-7）
```

---

## 3. 主要データ構造（v1.0 の追加・変更）

### 3.1 SettingsState（新規・F-7）

```rust
pub struct SettingsState {
    pub folder: Option<PathBuf>, // 現在の入力フォルダ。未選択起動を表現可能にする
    pub interval_secs: u64,     // 5..=60。advance 間隔
    pub display_count: usize,   // 1 または 2
    pub show_panel: bool,       // 管理画面（設定パネル）を開いているか
}
```

- `interval_secs` は **5〜60 にクランプ**して保持する。`advance_interval = Duration::from_secs(interval_secs)`。
- `display_count` は `{1, 2}` のみ。範囲外はロード時・設定時に最近傍へ丸める。
- F-7 の「フォルダ未選択状態」を表すため、空の `PathBuf` ではなく `Option<PathBuf>` を使う。

### 3.2 PersistState（新規・F-7）

ディスクに保存する**唯一の権威データ**。serde でシリアライズする（§7・JSON）。

```rust
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PersistState {
    pub schema_version: u32,        // 互換判定。初版は 1
    pub folder: PathBuf,            // 絶対パス
    pub interval_secs: u64,
    pub display_count: usize,
    pub play_order: Vec<PathBuf>,   // シャッフル順（続きを再現するため必須）
    pub cursor: usize,
    pub cycle_count: u32,
    pub last_shown: Vec<PathBuf>,   // 境界直後の連続再表示回避。最大2件
}
```

- `pending` / `fail_counts` / キャッシュ窓 / `menu` 等の**揮発状態は保存しない**（再起動後に rescan・補充で再構築）。
- `play_order` を保存しないと「続き」を再現できないため**必須**（要求 §7.4）。
- `last_shown` は復元直後にサイクル境界へ到達した場合も、直前表示画像を新サイクル先頭から避けるため保存する。

### 3.3 QuarantineState（変更・F-3）

Undo 廃止により履歴を持たない。

```rust
pub struct QuarantineState {
    pub dir: PathBuf,   // <入力>/delete
}
// DeleteRecord 構造体・last フィールド・restore() は削除する。
```

### 3.3.1 PlaybackState（変更・F-4 / F-7）

```rust
pub struct PlaybackState {
    // v2 既存フィールド...
    pub last_shown: Vec<PathBuf>, // 直近 advance で実際に表示していた最大2件
    pub empty_state_active: bool, // 空フォルダ状態への初期化済みフラグ（永続化しない）
}
```

- `advance()` は消費前の表示パスを `last_shown` へ保存する。
- サイクル境界では `last_shown` を `reshuffle_all_avoiding` へ渡す。
- F-7 の状態保存・復元にも含め、再起動直後の境界でも連続表示回避を維持する。
- `empty_state_active` は空状態への遷移処理を1回だけ実行するための揮発状態で、保存対象にしない。
  `play_order` に候補が入った時点で `false` に戻す。

### 3.4 CacheState（変更・F-1 / F-7）

```rust
pub struct CacheState {
    pub window: VecDeque<CachedImage>,
    pub current_vram: usize,
    pub pending_free: Vec<TextureHandle>,
    pub preload_blocked_by_vram: bool,

    // ---- F-1 追加（非同期デコード） ----
    pub ready: HashMap<PathBuf, DecodedImage>, // デコード済み・未配置（CPU 上の ColorImage+dims）
    pub inflight: HashSet<PathBuf>,            // 要求済み・未到着（重複要求防止）。世代更新時に全クリア
    pub ready_bytes: usize,                    // ready が保持する RGBA バイト合計
    pub epoch: u64,                            // 候補列の現在世代
}
```

- 表示枚数は `CacheState` に持たせず、`SettingsState.display_count` を補充・advance・描画へ渡す（出所一本化）。
- `DecodedImage` は `image_loader` 側に定義（§4.1）。F-1 未導入の段階では `ready`/`inflight` は追加しない
  （段階移行 §9）。
- `ready_bytes` は `ready` への追加・取り出し・破棄と同時に増減し、常に
  `ready.values().map(|img| img.rgba_bytes).sum()` と一致させる。
- 候補列の所属または順序が変わる操作（サイクル境界、フォルダ変更、削除、rescan、失敗除外）では
  `bump_epoch(decoder)` を呼んで `epoch += 1` し、`ready`/`inflight`/`ready_bytes` をクリアして現世代の要求を
  作り直す（`DecoderPool::set_epoch()` へ即時通知）。
- **`inflight` はパス単位（`HashSet<PathBuf>`）**とし、`(path, epoch)` の複合キーにはしない。世代更新時の
  全クリアで旧世代要求の追跡を捨てるため、旧世代の到着結果が現世代の同一パス要求を誤って解除することはない。
  旧世代の到着結果（epoch 不一致）は破棄し、その際 `inflight` は触らない（現世代のみ解除する）。

> **CPU RAM 予約（`DecodeBudget`/`DecodeReservation`）は F-8 により撤回した。** 巨大画像を退避して
> デコード対象を寸法上限以下に限定するため、RAM は「先読み枚数 × 上限サイズ」で有界になり、
> バイト予約プロトコルは不要（§4.1 / §4.8 / 要求 §8）。

### 3.5 ShufflePicApp（変更）

基盤詳細 §3.5 から以下を**追加／削除**する。

```rust
pub struct ShufflePicApp {
    pub playback: PlaybackState,
    pub cache: CacheState,
    pub quarantine: QuarantineState,   // F-3 で dir のみ
    pub scanner: ScannerState,
    pub settings: SettingsState,       // F-7 追加
    pub decoder: DecoderPool,          // F-1 追加（導入後）

    pub playing: bool,
    pub last_advance: Instant,
    // advance_interval は settings.interval_secs から都度算出（重複保持しない）

    pub fullscreen: bool,
    pub menu_open: bool,
    pub menu_target: Option<PathBuf>,
    pub error: Option<ErrorMessage>,

    // ---- F-2 追加：メニュー表示中の advance 凍結 ----
    pub advance_frozen_remaining: Option<Duration>, // メニューを開いた時点の「次の送りまでの残り時間」
    pub advance_freeze_active: bool,                // 前フレームの menu || settings 状態

    // ---- F-7 追加：永続化の節流 ----
    pub state_dirty: bool,          // 保存すべき変更があるか
    pub last_saved: Instant,        // 最終保存時刻（節流用）

    // 削除：last_undo 等は持たない（F-3）
}
```

- `advance_interval` を独立フィールドで持たず `settings.interval_secs` を唯一の出所とする（間隔変更の即時反映を
  単純化。v2 の「出所一本化」方針を踏襲）。

---

## 4. 機能別詳細設計

### 4.1 F-1：デコード非同期化

本機能の設計は [v1.0 デコード非同期化 設計提案](./ShufflePic%20v1.0%20デコード非同期化%20設計提案.md)（以下「F-1 提案」）に
準拠する。本書では確定事項と他機能との整合を記す。将来の編集で両文書が競合した場合は本書を優先する。

確定事項:

- 新規 `decoder.rs` にワーカープール（`DecoderPool`）を置く。ワーカーは `image::open → to_rgba8 → ColorImage`
  と寸法を返し、送信後に `ctx.request_repaint()` で UI を起こす。チャネルは **`crossbeam-channel`** を採用する。
- 要求キューは**表示用の高優先 bounded キュー**と**先読み用の通常 bounded キュー**に分ける。
  UI は `try_send` で投入し、高優先キューには `display_count` 件以上の予約容量を持たせる。
  ワーカーは `crossbeam_channel::select_biased!` 等で高優先キューを優先して待機し、空の場合だけ通常キューを受信する。
  これにより通常キューが満杯でも表示要求を受け付け、既存の先読み待ちより先に実行できる。
- `DecoderPool::Drop` は要求 Sender と結果 Receiver を先に切断してからワーカーを join する
  （結果送信待ちのワーカーを解放し、終了時デッドロックを防ぐ）。終了時の確実性のため、これは残す。
- `image_loader` に `decode_color(path) -> Result<DecodedImage, DecodeError>` を追加する。
  `DecodedImage` は `width` / `height` / `ColorImage` / `rgba_bytes`（RAM 集計用）を保持する。
  `DecodeError` は `Missing`（消失・失敗カウンタ非対象）・`Failed`（デコード失敗・カウンタ対象）に加え、
  **`Oversized`（寸法上限超過。F-8 で退避対象、失敗カウンタ非対象）**を区別する。寸法はフルデコード前の
  ヘッダ読みで判定する（§4.8）。
- `refill_window` は「**回収 → 配置 → 要求**」の非ブロック処理へ再構成する（F-1 提案 §6.1）。
  `load_texture`（GPU アップロード）のみ UI スレッドで行う。
- **epoch とパス基準検証を併用する**（F-1 提案 §7）。要求・結果は `epoch` を保持し、結果 epoch が現在値と
  異なれば成功／失敗を適用せず破棄する（その際 `inflight` は触らない）。`inflight` はパス単位で持ち、
  世代更新時の全クリアで旧世代要求を捨てる。
  `DecoderPool` は current epoch を `AtomicU64` で共有し、ワーカーは寸法取得・フルデコードの前に
  要求 epoch を確認して、古ければ処理を開始せず破棄する。
  現世代の結果だけを配置時に「`play_order[cursor + window.len()]` と一致するか／近傍に必要か」で再検証する。
  epoch は同一パスの旧成功・旧失敗を現候補へ適用しないための正しさの機構であり、任意最適化ではない。
- `DecodeRequest` / `DecodeResult` は要求時の `FileFingerprint`（少なくともファイル長と更新時刻。
  取得可能なら作成時刻も含む）を保持する。結果適用直前に現在の metadata から再取得して一致を確認し、
  不一致または取得不能なら結果を破棄して現ファイルを新規要求する。これにより同じパスの外部差し替えに
  古い `Ok` / `Failed` / `Missing` を適用しない。
- `Oversized` はファイル移動を伴うため、fingerprint 一致に加えて**移動直前に寸法を再取得**する。
  現在の画像が上限以下なら移動せず、古い `Oversized` 結果を破棄して通常デコードを新規要求する。
- ワーカー数・キュー上限・`READAHEAD_DEPTH` 等は §6 の定数を初期値とし、実測で調整する。

他機能との整合（本書で固定する）:

- **表示枠の優先**: 表示用スロット（`window.len() < display_count`）は高優先キューへ投入し、
  先読み要求とは容量・受信優先度を分離する。配置は到着後で、到着までは「読み込み中...」表示する。
- **VRAM**: 先頭 `display_count` 枚は v2 と同じく VRAM 上限より表示を優先する。
  `current_vram + candidate > VRAM_LIMIT` による配置停止は、表示枠を満たした後の先読みにだけ適用する。
- **CPU RAM（F-8 によりバイト予約は不要）**: F-8 で巨大画像は退避され、デコード対象は寸法上限以下に限定される。
  よってデコード後 RGBA は 1 枚あたり上限以下、同時デコードはワーカー数 K（≤4）、未配置保持は
  `MAX_READY_BUFFER` 枚、結果キューは `RESULT_QUEUE_CAPACITY` で頭打ちとなり、**RAM は
  「(K + 結果キュー + MAX_READY_BUFFER) × 上限サイズ」で有界**。`DecodeBudget`/`DecodeReservation`/
  `Deferred`/巨大表示例外は**設けない**。ワーカーはフルデコード前に寸法を確認し、上限超過なら `Oversized` を
  返して UI が退避する（§4.8）。
- **フレーム予算**: 結果回収（`MAX_RESULTS_PER_FRAME`）と GPU アップロード（`MAX_UPLOADS_PER_FRAME`）に
  1 フレーム上限を設け、`try_recv()` を無制限に回して UI を再占有しない。
- **F-4 との整合**: 非同期化後、表示枠が「未到着」のまま window が空になり得る。**`cursor >= play_order.len()`
  かつ window 空**の判定は、同期版と同じく §4.4 の共通遷移 `settle_cycle_or_empty()` に集約する
  （回収・配置・要求のいずれの後でも、フレーム終端で必ず評価する）。
- **egui 0.31 のスレッド規約**: `Context::request_repaint` / `load_texture` をワーカー外/UI スレッドで
  使い分ける規約を、実装時に egui 0.31 のドキュメントで再確認する（F-1 提案 §14 / CLAUDE.md 方針）。

判断基準（レビューで再三挙がった論点の確定事項。再検討の起点とする）:

- **表示優先に「専用ワーカー」や「先読み K-1 制限」は設けない。** F-8 で 1 枚のデコード時間が有界（寸法上限以下）に
  なり、`select_biased!` で空きワーカーが表示要求を先取りし、旧世代の表示要求はデコード前 epoch チェックで即破棄
  される。最悪待ちは「進行中の有界デコード 1 枚＋最大 1 フレーム」で、5〜60 秒間隔に対して無視できるため。
  実測で表示待ちが問題化した場合に限り、専用ワーカー等を後付けする。
- **CPU RAM のバイト予約方式（`DecodeBudget` 等）へは戻さない。** F-8 の寸法上限により RAM が
  「枚数 × 上限サイズ」で有界化されるため（上記「CPU RAM」項）。上限近傍の画像が複数重なる最悪値だけ実測し、
  必要なら `MAX_INFLIGHT` / `MAX_READY_BUFFER` を下げて調整する（予約プロトコルは追加しない）。

### 4.2 F-2：メニュー表示中の自動送り一時停止（残り時間から再開）

データ: `ShufflePicApp.advance_frozen_remaining: Option<Duration>` と、前フレームの
`advance_freeze_active: bool`。

手順（`update()` 内、UI 描画で当該フレームのメニュー／設定画面状態を確定した後、
advance タイマー判定の前段で評価する）:

1. `freeze_active = menu_open || settings.show_panel` を算出する。
2. **凍結開始（`advance_freeze_active == false && freeze_active == true`）**：
   `remaining = advance_interval.saturating_sub(last_advance.elapsed())` を計算し、
   `advance_frozen_remaining = Some(remaining)` とする。
3. **凍結中（`freeze_active == true`）**：advance タイマー判定を**スキップ**する（送らない）。
   再描画スケジュール（§5 step 10）も advance 起因の `request_repaint_after` を出さない。
4. **凍結終了（`advance_freeze_active == true && freeze_active == false`）**：
   `last_advance = Instant::now() - (advance_interval - remaining)` と再設定し、
   凍結していた残り時間が経過した時点で送られるようにする。`advance_frozen_remaining = None`。
   - `advance_interval`（＝設定間隔）がメニュー表示中に変更された場合に備え、再開時は
     `remaining` を上限 `advance_interval` でクランプする。
5. フレーム終端で `advance_freeze_active = freeze_active` を保存する。

凍結開始・終了の遷移が発生したフレームは advance 判定を行わない。これにより、右クリックでメニューを
開いた同じフレームにタイマー期限が重なっても画像を送らず、閉じた同じフレームにも即送らない。

整合:

- 再生停止中（`playing == false`）はもともと送らないため影響なし（要求 §2.3）。
- rescan はメニュー表示中も継続（v2 と同じ。表示中ペアを除去しないため妨げない）。
- 既存の誤削除防止（`menu_target` パス基準・キャンセル挙動）は維持（基盤詳細 §9）。
- **F-6 で追加する「再生／一時停止」メニュー項目**を押した場合も、メニューが閉じた後に上記 step 4 が
  働く。ただし停止状態から「再生」を選んだ場合は F-6 の通常再生規則を優先し、
  `last_advance = Instant::now()`、`advance_frozen_remaining = None` として設定間隔を全て計り直す。

### 4.3 F-3：Undo 廃止

削除内容:

- `app.rs`：`Ctrl+Z` 入力ハンドリング、上部バー「↶ Undo」ボタン、`do_undo()` を削除する。
- `quarantine.rs`：`DeleteRecord`、`QuarantineState.last`、`restore()` を削除する（§3.3）。
- `do_delete` は `quarantine.last` への記録（基盤詳細 §9.2-13）を行わない。それ以外（隔離移動・窓除去・
  `play_order` 除去・`cursor` 不変・`refill`）は維持する。

整合:

- **削除（隔離移動）は維持**。原本は `<入力>/delete` へ移動し保全される（要求 §3.3）。取り消しは利用者が
  `delete` フォルダから手動で戻す運用とする。
- `delete` と `oversized` の移動は、共通の衝突回避ヘルパを使う。移動先に同名ファイルがある場合は
  `name (1).ext`、`name (2).ext` …と空いている番号を探索し、既存ファイルを上書きしない。
- 入力抑制（`menu_open` 時の `Space`/`F11` 抑制）から `Ctrl+Z` を除く（もはや存在しないため）。
- F-1 提案も Undo 廃止へ更新済みであり、F-1 実装は Undo を前提にしない。

### 4.4 F-4：末尾到達でのサイクル停止修復（共通遷移へ集約）

問題（要求 §4）: サイクル境界（再シャッフル＋`cursor=0`）は v2 では `advance()` が `cursor` を末尾まで
進めた場合のみ発火する。しかし**アプリ内削除・デコード恒久失敗・`refill_window` の外部消失除去・
`rescan` の外部消失除去**など、候補列を短縮する経路でも `cursor >= play_order.len()` に到達し得る。
このとき window 空・`advance()` は `shown==0` で早期 return し、補充も `idx>=len` で停止 → フリーズ。

設計: **境界/空状態の判定を `advance()` から切り出し、単一の共通関数に集約する。**

```rust
// playback.rs（または app 層）。候補列を短縮し得る処理の後、毎フレーム必ず評価する。
// 戻り値で「境界が発火したか」を返してもよい。
pub fn settle_cycle_or_empty(
    pb: &mut PlaybackState,
    cache: &mut CacheState,
    decoder: &DecoderPool,
) -> Settle {
    if pb.play_order.is_empty() {
        if pb.empty_state_active {
            return Settle::None; // 空状態継続。epoch 更新・再初期化を繰り返さない。
        }
        // 空フォルダ状態（基盤詳細 §14）。window/ready/inflight/vram をクリア。
        while cache.evict_front().is_some() {}
        cache.current_vram = 0;
        cache.preload_blocked_by_vram = false;
        cache.ready.clear();
        cache.inflight.clear();
        cache.ready_bytes = 0;
        pb.cursor = 0;
        pb.empty_state_active = true;
        cache.bump_epoch(decoder);
        return Settle::Empty;
    }
    pb.empty_state_active = false;
    if cache.window.is_empty() && pb.cursor >= pb.play_order.len() {
        // サイクル境界（基盤詳細 §7.3 と同手順。window は既に空）。
        cache.current_vram = 0;
        cache.preload_blocked_by_vram = false;
        cache.ready.clear();      // 旧サイクルの先読みは破棄し、新シャッフルで作り直す
        cache.inflight.clear();
        cache.ready_bytes = 0;
        pb.cursor = 0;
        let last_shown = pb.last_shown.clone();
        pb.reshuffle_all_avoiding(&last_shown); // 直近表示があれば回避。無ければ空 Vec
        pb.cycle_count += 1;
        cache.bump_epoch(decoder);
        return Settle::CycleBoundary;
    }
    Settle::None
}
```

呼び出し規約:

- **`update()` のフレーム終端（補充の後、§5 step 8.5）で必ず 1 回評価する。** これにより、advance / delete /
  rescan / refill のどれが候補列を短縮しても、同一フレーム内で同一の遷移に収束する（要求 §4.3：
  「`rescan` と `refill_window` のどちらが先でも最終状態が同一」）。
- `advance()` 内の従来のサイクル境界ブロック（基盤詳細 §7.3）は、この共通関数の呼び出しに**置き換える**
  （二重発火を避けるため、境界処理の実体は一箇所に集約）。`advance()` は「window 先頭を消費し `cursor +=
  shown`」までを行い、境界判定は共通関数へ委ねる。
- `cycle_count` の二重加算を避けるため、共通関数は 1 フレームに必要な遷移を 1 回だけ行う
  （境界 → `cursor=0` 後は条件が偽になるので多重実行されない）。
- 境界・空状態では `ready`/`inflight`/`ready_bytes` をクリアし、`epoch += 1` を
  `DecoderPool` の共有 current epoch へ通知する。進行中の旧世代結果は到着時に epoch 不一致で破棄する。
- 空状態の初期化と epoch 更新は `empty_state_active == false` から空になった遷移時だけ実行する。
  空状態が継続するフレームは `Settle::None` とし、同じ処理を繰り返さない。

`last_shown` の扱い: v2 では advance 内ローカルで保持していた直近表示パスを、境界回避（`reshuffle_all_avoiding`）に
使う。共通関数化に伴い、直近 advance の `last_shown`（最大 2 件）を `PlaybackState` に**スナップショットとして
退避**しておく（`pb.last_shown: Vec<PathBuf>`）。削除/失敗起因の境界では直近表示が無い場合があり、その場合は
空スライスで回避なし（許容。要求 §4.4）。

受け入れ（要求 §4.5）に対応するテストは §8 / §9 で定義する。

### 4.5 F-5：上部バーへ「表示済み」枚数

- 上部バーに `表示済み: {cursor}` を追加する（基盤詳細 §13.1 の表示項目に追記）。
- 値は当該サイクルで advance により表示済みとして消費確定した枚数＝`cursor`。
  window に描画済みでも未消費の画像は含めない。
- 既存「未表示: `play_order.len() - cursor`」と合わせ、`表示済み + 未表示 == play_order.len()` が常に成立する。
- サイクル境界（§4.4）で `cursor=0` に戻るため自動的に 0 リセットされる。

### 4.6 F-6：右クリックメニューへ「再生／一時停止」

- 削除メニュー（基盤詳細 §13.3）の項目に、再生状態に応じた 1 項目を追加する。
  - `playing == true` のとき「一時停止」、`false` のとき「再生」を表示（上部バーのトグルと同じラベル規則）。
- クリック時は `play_toggle_request = true` を立て、`ui.close_menu()` でメニューを閉じる。実処理は描画後
  （§5 step 8）に `playing` をトグルし、再生再開時は `last_advance` を更新する（`Space`/上部ボタンと同一動作）。
- **入力抑制との整合**: `menu_open` による外部入力抑制は「メニュー外の `Space`/ボタン」を抑制するもので、
  **メニュー項目自身のクリックは抑制対象外**。本項目はメニュー内なので抑制されずに要求を立てられる。
- F-2 との整合: メニューを閉じた後、§4.2 step 4 により残り時間から再開する（停止を選べば停止のまま）。
- 停止状態からメニュー項目で再生した場合は `last_advance = Instant::now()` とし、F-2 の保存済み残り時間を
  破棄する。再生中のままメニューを閉じた場合だけ残り時間から再開する。

### 4.7 F-7：管理画面＋設定＋永続化

#### 4.7.1 管理画面（`settings.rs`）

- 上部バーに「⚙ 設定」ボタンを追加し、`settings.show_panel` をトグルする。
- 設定パネルは `egui::Window`（または中央オーバーレイ）で表示し、以下を提供する。
  1. **画像フォルダ**: 現在パスのラベル＋「参照...」ボタン。ボタンで
     `rfd::FileDialog::new().set_directory(現在パス).pick_folder() -> Option<PathBuf>` を呼ぶ（同期・ブロッキング）。
     選択されたら §4.7.3 のフォルダ変更を実行する。
  2. **間隔**: `egui::Slider::new(&mut secs, 5..=60)`（または `DragValue` を 5..=60 にクランプ）。
     変更は即時反映（§4.7.2）。
  3. **表示枚数**: 1/2 のラジオまたは `ComboBox`。変更は即時反映（§4.7.2）。
- **確認ダイアログは設けない**（要求 §7.3）。間隔・枚数の変更は即適用。
- パネル表示中の advance: 予測可能性のため **F-2 と同じ凍結**を適用する（`show_panel == true` を `menu_open` と
  同様に扱い、残り時間から再開）。なお `pick_folder()` 実行中は UI スレッドがブロックするため、その間は
  いずれにせよ送られない。

#### 4.7.2 設定の即時反映（カウントを消さない）

- **間隔変更**: `settings.interval_secs` を 5..=60 にクランプして更新するだけ。`advance_interval` は都度
  `settings.interval_secs` から算出するため次フレームから反映。`last_advance` は維持（サイクル状態不変）。
- **表示枚数変更**: `settings.display_count` を更新するだけ。`play_order`/`cursor`/`cycle_count`/`window` は
  クリアしない。以降、advance の消費枚数・補充の表示スロット数・描画枚数が新しい `display_count` に従う
  （§4.7.5）。2→1 では変更直前の右側画像を描画・消費せず、未表示候補として window に保持する。
  次の advance は左側 1 枚だけを消費するため、その右側画像は後で先頭へ繰り上がり単独表示される。
  1→2 では window に既にある次画像が右に出る（無ければ補充で到着後に出る）。
- いずれの変更も `state_dirty = true` とし、§4.7.4 の節流保存に乗せる。

#### 4.7.3 フォルダ変更（全リセット）

新フォルダ `new_dir` と現在フォルダを可能な限り `canonicalize` して比較し、実体が異なる場合のみ実行する。
同一フォルダの表記差（末尾区切り、相対／絶対、Windows の大文字小文字差）だけではリセットしない。

1. `new_dir` の存在・ディレクトリ性を検証（失敗は UI エラー、変更しない）。
2. `<new_dir>/delete` を準備（ファイルとして存在すれば UI エラーで中止。無ければ作成。基盤詳細 §6）。
3. **サイクル状態を全クリア**: `playback`（`play_order`/`cursor`/`cycle_count`/`pending`/`fail_counts`/
   `last_shown`）を初期化。`cache`（`window` の全テクスチャを `pending_free` へ退避し、`current_vram=0`、
   `ready`/`inflight`/`ready_bytes` をクリア、`preload_blocked_by_vram=false`、`epoch += 1`）。
   新 epoch を `DecoderPool` へ通知し、旧フォルダの結果を成功・失敗とも破棄する（§4.1）。
4. `settings.folder = Some(new_dir)`、`quarantine.dir = <new_dir>/delete`。
5. 新フォルダ直下をスキャン → シャッフルして `play_order` を作り直す（基盤詳細 §6・§11.1）。
6. `state_dirty = true` とし、**即時保存**（§4.7.4。フォルダ確定は重要イベントのため節流せず即書き込み）。

#### 4.7.4 永続化（`persist.rs`・ディスク保存／復元）

保存先:

- 既定は**実行ファイルと同じディレクトリ**の `shufflepic_state.json`。
  候補は「実行ファイル隣 → Windows の `%APPDATA%\ShufflePic` → カレントディレクトリ」の順とする。
  起動時は候補内の有効な状態ファイルを調べ、複数ある場合は更新時刻が最も新しいものを採用する
  （同時刻なら候補順）。既存ファイルが無ければ、候補順で書き込み可能な場所を選ぶ。
  採用したパスは起動中の唯一の保存先として保持し、
  読み書きで混在させない。

保存形式: `PersistState`（§3.2）を `serde_json` で**整形 JSON**として書き出す。書き込みは
同じディレクトリの**一時ファイルへ全量書き込み → `flush` / `sync_all` → `rename` で置換**する。
書き込み・同期・置換のいずれかが失敗した場合は既存の状態ファイルを変更せず、一時ファイルを後始末して
UI エラーを表示する。この方式は破損リスクを低減するが、OS・媒体障害を含む完全なクラッシュ耐性は保証しない。

> **判断基準（Windows の `rename`）**: `std::fs::rename` は**宛先ファイルが既存でも置換する**
> （Windows 10 1607 以降／本環境 Windows 11。公式ドキュメント記載。`to` がディレクトリでない限り成立し、
> 状態 JSON はファイルなので該当）。よって状態ファイルの差し替えに **Windows 専用 API（`ReplaceFile` 等）は不要**。
> 原子性は保証されない（宛先がロック中だと失敗し得る）が、上記の temp→`sync_all`→`rename`＋「失敗時は既存を保持」で
> 破損を避ける。出典: <https://doc.rust-lang.org/std/fs/fn.rename.html>。

保存契機:

- **重要イベントで即時保存**: フォルダ変更（§4.7.3）。
- **節流保存**: 設定変更（間隔/枚数）・サイクル状態変化（advance による `cursor` 更新、境界の `cycle_count`、
  rescan/delete による `play_order` 変化）では `state_dirty=true` を立て、`update()` 終端で
  「`state_dirty && last_saved.elapsed() >= SAVE_THROTTLE`」のとき保存する（§6 の `SAVE_THROTTLE`）。
- **終了時保存**: `on_exit` で保存する。`eframe::App::save()` は eframe の Storage が有効な場合の補助契機に
  留め、終了保存の唯一の保証にはしない。通常稼働中の節流保存が主経路であり、強制終了時に最後の数秒分が
  保存されない可能性は許容する。

復元（起動時・`main.rs`）:

1. `persist::load()` で状態ファイルを読む（無ければ `None`）。
2. **`schema_version` 不一致や JSON 破損は復元失敗**として扱い、`None` と同等にフォールバック（クラッシュしない）。
3. 復元成功かつ `folder` が**現在も存在するディレクトリ**なら、フォルダを正規化して直下を再スキャンし、
   保存状態と突き合わせる。
   - `safe_cursor = min(saved_cursor, saved_play_order.len())` とし、配列分割の安全確保にだけ使用する。
     保存 `play_order[..safe_cursor]` を「表示済み」、残りを「未表示」として分ける。
   - 各保存パスを正規化し、指定フォルダの**直下**にある対応画像だけを採用する。フォルダ外、サブフォルダ、
     `delete` 配下、非対応拡張子、重複、現在消失しているパスは除外する。
   - 実スキャンにのみ存在する新規パスは未表示側へ追加し、その追加部分をシャッフルする。
   - 復元後の `play_order = 有効な表示済み + 有効な未表示 + 新規`、
     `cursor = 有効な表示済み.len()` とする。単純な `min(saved_cursor, len)` は使用しない。
   - `last_shown` は保存値から、現在の指定フォルダ直下に属する最大2件だけを復元する。
   - 突き合わせ後 `cursor >= play_order.len()` なら、起動後の §4.4 共通遷移で境界または空状態へ移行する。
4. 復元失敗、または保存 `folder` が存在しない場合は、**管理画面でフォルダ未選択の初期状態**で起動する
   （§4.7.6）。

#### 4.7.5 表示枚数（display_count）の一般化

v2 でハードコードされた `2` を `settings.display_count`（1 か 2）に置き換える。

- **advance**（基盤詳細 §7.1）: `shown = min(window.len(), display_count)`。
- **補充の表示スロット**（基盤詳細 §8.1-2 / §8.2）: 「`window.len() < 2`」を「`window.len() < display_count`」へ。
  表示スロットは VRAM 上限に関係なく確保する点は不変。
- **描画**（基盤詳細 §13.2）: 先頭 `min(window.len(), display_count)` 枚を並べる。`display_count==1` は 1 枚表示。
- **F-1 の表示枠優先**（§4.1）: 先頭 `display_count` 枚の要求を、先読み要求より先に投入する。
- 不変条件「window は `play_order[cursor..]` の連続ミラー」は枚数に依らず維持。`display_count` は
  「何枚を表示・消費するか」のみを決め、window の作り方自体は変えない。
- 2→1 変更時、変更直前に右側へ描画されていた `window[1]` は `cursor` に加算しない。
  v3 のカウント上は未表示であり、後で `window[0]` として単独表示する。利用者には同じ画像が再度見えるが、
  設定変更で表示枠から外れた画像を未消費として扱う確定仕様であり、重複データ登録ではない。

#### 4.7.6 起動フロー変更（CLI 入力の廃止）

- v2 の標準入力によるパス入力（基盤詳細 §6-1）を**廃止**する。
- 起動時に復元（§4.7.4）を試み、有効な `folder` があればそのフォルダで通常起動。
- 無ければ**フォルダ未選択状態**で起動し、中央に「設定からフォルダを選択してください」を表示。利用者が
  管理画面（§4.7.1）でフォルダを選ぶと §4.7.3 が走り、通常動作へ入る。
- 日本語フォント設定・eframe 起動は v2 を踏襲（基盤詳細 §2.1）。

### 4.8 F-8：巨大画像の自動退避

巨大画像（寸法上限超過）はスライドショーの対象にせず、入力フォルダ直下の専用フォルダ
`<入力>/oversized`（`OVERSIZED_DIR_NAME`）へ隔離移動する。`delete` と同じ移動方式（`quarantine` の
`rename`＋衝突回避を再利用）で行い、原本は保全する（移動であり消去ではない）。

判定:

- 上限は `幅 × 高さ > OVERSIZED_MAX_PIXELS` または `max(幅, 高さ) > OVERSIZED_MAX_SIDE`。
- 寸法は `image::image_dimensions`（ヘッダ読み・高速）で取得し、**フルデコードしない**。

タイミング（遅延判定。起動時の全件 probe はしない）:

- F-1（非同期）導入後：ワーカーがフルデコード前に寸法を確認し、上限超過なら `DecodeError::Oversized` を返す
  （デコードしない）。UI が下記の退避を行う。
- F-1 導入前（同期段階）：`refill_window` がデコード直前に `image_dimensions` で判定する。
- rescan で後から入った巨大画像も、デコード候補になった時点で同様に退避される。

退避処理（`Oversized` を受けた／検出した時。UI スレッド）:

1. 退避フォルダ `<入力>/oversized` を準備する。無ければ作成し、同名の通常ファイルが存在するなど
   準備できない場合は、下記の「移動失敗時」処理へ進む。
2. 対象を `quarantine::move_with_collision_avoidance(対象, oversized_dir)` 相当の共通ヘルパで移動する。
   このヘルパは `delete` 移動にも使用し、同名時は `name (1).ext`、`name (2).ext` …を選んで上書きしない。
3. **移動成功時**は対象パスを `play_order`（および `ready`/`inflight`）から除去する。
   **`cursor` は進めない**（補充失敗と同じ扱い・基盤詳細 §12.2）。`fail_counts` には数えない。
4. フレーム終端の `settle_cycle_or_empty()`（§4.4）で、退避により末尾到達しても停止しない。

移動失敗時（実装＝対象パス単位の累計試行で停止）:

1. `oversized` フォルダ準備失敗、衝突回避先の確保失敗、移動（`hard_link`／コピー）失敗等を移動失敗として扱う。
2. **対象パスごとに失敗回数を数える**（`oversized_attempts`）。失敗のたびに対象を**未表示領域の末尾へ戻す**。
   `cursor` は進めず、`fail_counts` にも加算しない。候補列変更として epoch を更新し、失敗直後の同一フレームで
   再要求せず他の未表示画像を先に処理する。対象が再び候補位置へ来た時点で寸法判定と移動を再試行する
   （通常失敗時は画面通知を出さず、静かに再キューする）。
3. 失敗回数が**累計で `MAX_OVERSIZED_MOVE_ATTEMPTS`（5）回**に達したら、**スライドショーのループ処理ごと
   停止する**（`halted` 状態へ）。「最後の未表示候補を検出してその場で連続 5 回」ではなく、対象が候補に選ばれる
   たびに 1 回試行し累計 5 回で停止する方式とする。
   この時点で未表示ストックの残りも表示不能な例外ファイルである可能性が高いため、止めて利用者の手動処理に
   委ねる。具体的には次を行う。
   - `playing = false` かつ `halted = true` とし、**以後フレームでは補充（refill）・送り（advance）・退避処理・
     サイクル境界判定（settle）をすべて停止**する。これにより対象を `play_order` に残しても再要求・再試行が
     起きず（ループしない）、窓も前進しないため§4.4 のストールも発生しない。
   - **モーダルダイアログ**を表示する（背後の操作を抑止）。メッセージには**具体的なファイル名**（および場所・原因）を
     含める。OK でダイアログは閉じるが `halted` は維持する。
   - 対象は `play_order` に残し、原本も入力フォルダに残す（データは失わない）。
   - 復帰は明示的な利用者操作のみ：手動でファイルを処理した後、「⚙ 設定」でフォルダを選び直すか、アプリを
     再起動する（`halted` は揮発・永続化しない）。**`halted` 中は同一フォルダの再選択でも全リセットして復帰する**
     （`change_folder` は `halted` 中は同一フォルダ判定でスキップしない）。

整合:

- 退避フォルダは `delete` と同様スキャン対象外（直下ファイルのみ走査）。
- これにより F-1 の CPU RAM バイト予約・巨大表示例外は不要（§4.1 / 要求 §8.3）。
- 通常の移動失敗は F-4 の空状態ではなく「未表示末尾への再投入」であり、サイクルを継続する。
- 最終 5 回失敗による `halted` 停止は、処理不能を利用者へ通知する明示的な全停止であり、F-4 の不具合停止
  （表示すべき画像が残るのに固まる）とは区別する。`halted` 中はループ処理を動かさないため、対象を `play_order`
  に残してもストール・再要求ループは起きない。

---

## 5. update() フロー（v1.0 改訂）

F-2 の「メニューを開いた同一フレームから凍結」を保証するため、advance 判定を UI 状態検出後へ移す。

1. 前フレームで `pending_free` に入った `TextureHandle` を drop。
2. **F-1: `decoder` からの結果回収**（最大 `MAX_RESULTS_PER_FRAME` 件）。まず epoch を照合し、
   旧世代なら成功／失敗を適用せず破棄する。現世代だけ fingerprint とパス基準で再検証し、`ready` 格納または
   失敗／消失処理を行い、対応する `(path, epoch)` を `inflight` から除去する。
3. 前フレームから継続中のメニュー／設定画面状態を基準にキーボード入力を読む。
   表示中は `Space` / `F11` を抑制する。
4. rescan タイマー期限切れなら `scanner::rescan`。
5. `cache::refill_window` の結果配置を行い、現在表示可能な窓を準備する。
6. UI を描画し、当該フレームの `menu_open_now` / `settings.show_panel` と各操作要求を確定する。
7. **F-2/F-7: `freeze_active_now = menu_open_now || settings.show_panel` として凍結開始／終了を処理する。**
   開始または終了したフレームは `advance_suppressed_this_frame = true` とする。
8. 描画後の操作要求を適用する。全画面、削除、再生／一時停止、フォルダ変更、設定変更を処理する。
   停止状態から再生した場合は `last_advance = now` とし、保存済み残り時間を破棄する。
9. **advance 判定**: `playing`、`!freeze_active_now`、`!advance_suppressed_this_frame`、
   `play_order` 非空、タイマー期限切れの全条件を満たす場合だけ `advance()` を実行する。
10. advance／削除／設定変更後の状態に対して再度 `cache::refill_window` を行う。
    F-1 後の GPU 配置は全呼び出し合計で 1 フレーム最大 `MAX_UPLOADS_PER_FRAME` 件とする。
11. **`settle_cycle_or_empty()` を 1 回評価**し、advance/delete/rescan/refill のどの経路でも
    サイクル境界または空状態へ収束させる。
12. エラー期限処理、永続化の節流保存を行う。
13. 再描画をスケジュールする。`freeze_active_now` 中は advance 起因の
    `request_repaint_after` を出さず、F-1 の結果到着はワーカーの `request_repaint()` で起床する。
    advance、削除、設定変更、境界遷移が発生した場合は `request_repaint()` で次フレームを即要求する。
14. `menu_open = menu_open_now`、`advance_freeze_active = freeze_active_now` を次フレーム用に保存する。

初回フレームの全プリロード回避（基盤詳細 §5「初回フレーム」）は踏襲。F-1 導入後は「初回は高優先要求を
出すだけ」に変わる（到着まで読み込み中表示）。

---

## 6. 定数（v1.0 の追加・変更）

```rust
// 変更：間隔は設定値（5..=60）。定数は既定値として残す。
pub const DEFAULT_INTERVAL_SECS: u64 = 15;     // 復元値が無いときの既定
pub const MIN_INTERVAL_SECS: u64 = 5;
pub const MAX_INTERVAL_SECS: u64 = 60;
pub const DEFAULT_DISPLAY_COUNT: usize = 2;    // 既定枚数

// F-7 永続化
pub const STATE_FILE_NAME: &str = "shufflepic_state.json";
pub const SAVE_THROTTLE: Duration = Duration::from_secs(5); // 節流保存の最短間隔
pub const PERSIST_SCHEMA_VERSION: u32 = 1;

// F-1（初期値。実測で調整）
pub const DECODE_WORKERS: usize = 0;   // 0=自動（コア数-1 を 1..=4 にクランプ）
pub const MAX_INFLIGHT: usize = 6;
pub const MAX_READY_BUFFER: usize = 6;             // 未配置保持の枚数上限（F-8 の寸法上限と合わせ RAM を有界化）
pub const READAHEAD_DEPTH: usize = 8;
pub const MAX_RESULTS_PER_FRAME: usize = 4;
pub const MAX_UPLOADS_PER_FRAME: usize = 2;
pub const DISPLAY_QUEUE_CAPACITY: usize = 2;       // 表示用高優先キュー
pub const PREFETCH_QUEUE_CAPACITY: usize = 6;
pub const RESULT_QUEUE_CAPACITY: usize = 8;

// F-8 巨大画像の退避（初期値。実測で調整可。利用者は閾値に関与しない）
pub const OVERSIZED_MAX_PIXELS: u64 = 32_000_000;  // 幅×高さ がこれ超で退避（≒ デコード後 128MB）
pub const OVERSIZED_MAX_SIDE: u32 = 10_000;        // 長辺がこれ超でも退避
pub const OVERSIZED_DIR_NAME: &str = "oversized";  // <入力>/oversized（delete と並ぶ兄弟サブフォルダ）
pub const MAX_OVERSIZED_MOVE_ATTEMPTS: usize = 5;  // 対象パス単位の累計移動失敗回数の上限（超過で halted）

// 据え置き（基盤詳細 §3.6）
pub const VRAM_LIMIT: usize = 2 * 1024 * 1024 * 1024;
pub const REFILL_BUDGET_N: usize = 2;          // F-1 後は枚数ゲートへ役割移行（GPU アップロード平準化用に残置可）
pub const INITIAL_FILL_ATTEMPT_LIMIT: usize = 8;
pub const RESCAN_INTERVAL_SECS: u64 = 10;
pub const MAX_DECODE_FAILS: u32 = 3;
pub const ERROR_MSG_SECS: u64 = 4;
```

---

## 7. 依存クレート（Cargo.toml の変更）

| クレート | 用途 | 備考 |
| --- | --- | --- |
| `serde`（derive） | `PersistState` のシリアライズ | F-7 |
| `serde_json` | 状態ファイル（JSON）入出力 | F-7。RON 等でも可だが可読性で JSON を既定 |
| `rfd` | ネイティブフォルダ選択ダイアログ | F-7。`pick_folder()` 同期版 |
| `crossbeam-channel` | デコード mpmc チャネル | F-1（F-1 提案 §8 第1案） |

- バージョンは実装時に `cargo add` で最新安定を採用し、`cargo build --release` で確認する（CLAUDE.md：
  記憶に頼らず一次情報で確認）。`eframe` 既定 `Storage` は使わないため `persistence` フィーチャは必須ではない
  （`save()` フック内で独自ファイルへ書く。§4.7.4）。

---

## 8. テスト計画（v1.0 追加分）

ロジック層（スレッド非依存）を厚くする方針は v2 を踏襲。

- **F-4（最重要・回帰必須）**
  - サイクル末尾の最後の 1 枚をアプリ内削除 → `settle_cycle_or_empty` で境界へ移行し、停止しない。
  - 最後の 1 枚がデコード連続失敗で恒久除外 → 同上。
  - 未キャッシュの最終候補を外部削除し、`refill_window` の `path.exists()` 除去で `cursor==len` になる経路 →
    フレーム終端の共通遷移で境界へ移行（rescan 先行・refill 先行の両順序で最終状態が同一）。
  - 全消失で `play_order` 空 → 空フォルダ状態。
- **F-7（永続化）**
  - `PersistState` の round-trip（保存→読込で同値）。`schema_version` 不一致・破損 JSON は `None` 相当へ
    フォールバックし panic しない。
  - 復元時 `cursor > saved_play_order.len()` でも、分割範囲だけを `safe_cursor` で安全化し、
    最終 `cursor` は検証後の有効な表示済み件数から再計算する。
  - フォルダ変更でサイクル状態が全クリアされる（`cursor=0`/`cycle_count=0`/新 `play_order`）。
  - 表示枚数 2→1／1→2 でカウント（`cursor`/`cycle_count`/`play_order`）が不変。
    2→1 では変更直前の右側画像が未表示として残り、後で単独表示される。
  - 間隔変更が 5..=60 にクランプされる。
  - 復元 JSON にフォルダ外パス、サブフォルダ、`delete` 配下、重複、非対応拡張子を混入しても除外される。
  - 保存失敗時に既存の正常ファイルが保持され、アプリが継続する。
- **F-2**: メニュー開で残り時間が凍結し、閉じてから残り時間後に advanceすることに加え、
  開閉遷移と送り期限が同一フレームに重なっても advance しないことを検証する。
- **F-2/F-6**: メニューと設定パネルの重複表示、一方だけ閉じるケース、停止中からメニュー内再生するケースを検証。
- **F-3**: `restore`/`DeleteRecord` 不在のコンパイル確認、削除は隔離移動のみで原本保全。
- **F-5**: `表示済み == cursor` かつ `表示済み + 未表示 == play_order.len()`。
- **F-1**: `decode_color` の正常/破損（`Failed`）/消失（`Missing`）、配置ロジック（`ready` 充填→`refill_window`
  で連続ミラー・`current_vram` 一致）、無効化（epoch 不一致および遠方/不在パスの破棄）、
  通常キュー満杯でも表示要求を受け付けて先に取得すること、表示枠だけが VRAM 上限を超えて配置可能なことを検証。
  同一パスを旧世代と新世代で要求し、旧成功／旧失敗が新世代へ適用されないことも回帰確認する。
  同一 epoch 中に同じパスのファイルを差し替えた場合も fingerprint 不一致で旧結果を破棄し、
  古い `Oversized` 結果では移動しないことを確認する。
  スレッド統合では往復 1 枚に加え、結果キュー滞留中に `DecoderPool` を drop しても join が停止しないことを確認する。
- **F-8**: 寸法上限超過の判定（`OVERSIZED_MAX_PIXELS` / `OVERSIZED_MAX_SIDE` の境界値）、`Oversized` 検出で
  対象が `oversized` フォルダへ移動し `play_order` から除かれること（`cursor` 不変・`fail_counts` 非加算）、
  移動が `delete` と共通の衝突回避命名で原本を保全すること、退避で末尾到達しても F-4 で停止しないこと、
  `oversized`・`delete` がスキャン対象外であることを検証。
  - 通常の移動失敗では通知を出さず未表示末尾へ戻し、`playing` を維持する。
  - 再投入後は他の未表示画像が先に処理され、同一フレームで失敗画像を再要求しない。
  - 同一パスの移動が累計 5 回失敗したら `halted` 状態に入り、ファイル名を含むモーダルを表示して
    `playing=false`・補充/送り/退避/境界判定/rescan を停止する。対象ファイルは消去しない。
  - `halted` 中に同一フォルダを再選択しても全リセットして復帰する。
  - `delete` / `oversized` の移動先に同名ファイルが複数ある場合、空いている番号を選び上書きしない。

UI/ネイティブダイアログ（`rfd`）に依存する部分は自動テスト対象外とし、ロジック関数を純粋に切り出して検証する。

---

## 9. 段階的移行（実装順）

各段階で `cargo build` / `cargo test` を緑に保つ。

1. **F-3（Undo 廃止）**: 不要コード除去。最小リスク。テスト調整。
2. **F-4（共通遷移）**: `settle_cycle_or_empty` を導入し、`advance` の境界を委譲。境界テストを追加。
3. **F-5 / F-6**: 上部バー「表示済み」、右クリック「再生／一時停止」。
4. **F-2**: メニュー凍結・残り時間再開。
5. **F-7**: `settings.rs`（管理画面・display_count 一般化・間隔）、`persist.rs`（保存/復元）、起動フロー変更。
   - 先に display_count 一般化と間隔設定（メモリ内）を入れて動作確認 → 次に永続化を追加。
6. **F-8**: 巨大画像の寸法判定と退避（`oversized` フォルダ）。F-1 導入前は `refill_window` のデコード直前に
   同期 `image_dimensions` で判定し、`quarantine` の移動を再利用。
7. **F-1**: `decoder.rs` 導入、`refill_window` を回収→配置→要求へ再構成、UI スレッド上の寸法プローブを撤去
   （寸法はデコード結果に同梱。寸法上限判定はワーカーへ移し `Oversized` を返す）。
   最後に回す（最大の構造変更で、F-4/F-7/F-8 との統合点を先に安定させる）。

---

## 10. 未決事項（実装前に確定する）

- `rfd` / `serde_json` / `crossbeam-channel` の採用バージョン（`cargo add` 時に確定）。
- 設定パネルの体裁（`egui::Window` か中央オーバーレイか）と開閉キー割当の要否。
- F-1 の各上限値とワーカー数の最終値（§6 の初期値から実測で調整）。

---

## 改訂履歴

- 2026-06-24 初版作成。スコープ F-1〜F-7 の詳細設計を定義（前身 詳細設計書からの差分構成）。
- 2026-06-24 横断レビュー反映。優先キュー、CPU RAM 予算、表示枠 VRAM 例外、複合凍結、
  永続化パス検証・保存先フォールバック・安全な置換手順を確定。
- 2026-06-24 F-1 再レビュー反映。単一FIFO・`ready_bytes` のみのゲート・パス単独識別では
  表示優先、RAM上限、同一パス旧結果排除を保証できないため、二段キュー、RAII RAM予約、epoch必須へ統一。
- 2026-06-24 F-8（巨大画像の自動退避）を追加。デコード対象を寸法上限以下に限定するため、F-1 の CPU RAM
  バイト予約（`DecodeBudget`/`DecodeReservation`/`Deferred`/巨大表示例外）を撤回し、RAM は
  「先読み枚数 × 上限サイズ」で有界化。表示優先キューと epoch（世代識別）は維持。
- 2026-06-24 F-8 の移動失敗遷移を追加。通常失敗は非モーダル通知後に未表示末尾へ戻して継続し、
  同一パスの累計 5 回失敗で halted（ループ停止＋ファイル名付きモーダル）とした。
  `delete` / `oversized` の衝突回避移動を共通処理として定義。
- 2026-06-24 空状態の初期化を遷移時1回へ限定し、同一パス差し替えを fingerprint と
  `Oversized` 移動前再判定で検出する設計を追加。復元 cursor の安全化手順を統一し、
  表示枚数 2→1 時の右側画像を未表示として後で単独表示する仕様を確定。
- 2026-06-24 再三の論点へ判断基準を明記。(1) `std::fs::rename` は Windows 10 1607+/Win11 で既存ファイルを
  置換するため状態ファイル差し替えに専用 API 不要（§4.7.4・一次資料）。(2) 表示優先に専用ワーカー／先読み
  K-1 制限は設けない、(3) CPU RAM バイト予約方式へは戻さない、を確定事項として §4.1 に記録。
- 2026-06-24 実装レビュー反映（`../v1.0/`）。隔離移動を `hard_link`＋原本削除（非対応FSは予約+rename）へ変更し
  上書き競合を解消。F-8 退避失敗のモーダルを擬似モーダル（全操作抑止）化。候補列変更を永続化 dirty 化。
  F-8 最終失敗は**ループ処理ごと停止（`halted`）＋ファイル名付きモーダル**へ確定（§4.8）。
- 2026-06-25 実装再レビュー反映（ShufflePic v1.0）。(#1) `halted` 中は同一フォルダ再選択でも全リセットして復帰。
  (#3) リンク非対応FS のフォールバックを **rename ではなくコピー＋原本削除**へ変更（上書き競合を解消）。
  (#4) 退避モーダル中は設定パネルを描画せず操作を遮断。(#5) `halted` 中は rescan も停止。
  §4.8 を実装どおり「対象パス単位の累計 5 回失敗で halted」へ統一。残る `v3`／`未実装` 表記を v1.0 へ更新。
