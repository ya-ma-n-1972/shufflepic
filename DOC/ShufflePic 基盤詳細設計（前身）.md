# ShufflePic 基盤詳細設計（前身）

> 本書は **ShufflePic v1.0 の前身**となる詳細設計（社内反復の旧版＝当時の v2.0）です。
> ShufflePic v1.0（公開版）の詳細設計は「ShufflePic v1.0 詳細設計書」が正で、本書を変更のない事項の
> **基盤**として「基盤詳細 §X」と参照します。本書中の Undo・2枚固定・CLI フォルダ入力・コード参照
> （`../v2.0/src/` 等の旧パス）は**当時の仕様**であり、ShufflePic v1.0 では変更・廃止されています。

## 0. 目的と範囲

本書は「高速画像ビューワー 再設計 要求定義書」を実装直前の粒度へ落とし込む詳細設計書である。実装対象は Rust + eframe/egui 0.31 の画像スライドショーアプリであり、旧実装の「ファイル移動で内部状態を表現する」構造を廃止し、メモリ状態を中心に再設計する。

本書では、モジュール分割、主要データ構造、状態不変条件、画面更新フロー、削除・Undo・再スキャン・キャッシュ補充の手順を定義する。コードそのもの、UI の細かな見た目、実測後の性能チューニング値は本書の範囲外とする。

## 1. 全体方針

- 入力フォルダ内の原本ファイルは、ユーザーが削除操作を明示した場合を除き、移動・改名・削除しない。
- 既出、未表示、削除済み、デコード失敗などの状態はメモリ内で管理する。
- ファイルシステム操作は、隔離フォルダ作成、削除隔離、Undo のみで行う。
- ファイル操作は成功確認後にだけメモリ状態へ反映する。
- `TextureHandle` の破棄は描画と同一フレームで行わず、`pending_free` に退避して次フレーム冒頭で drop する。
- `play_order[cursor..]` と `window` の対応関係を壊さない。`window` に対応する範囲は rescan や Undo のシャッフル対象にしない。

## 2. モジュール構成

```text
src/
├── main.rs
├── app.rs
├── playback.rs
├── cache.rs
├── quarantine.rs
├── scanner.rs
└── image_loader.rs
```

### 2.1 main.rs

責務:

- 起動引数または既存方式の対話入力から入力フォルダを取得する。
- 入力フォルダの存在、ディレクトリ性を検証する。
- `<入力フォルダ>/delete` がファイルとして存在しないことを検証する。
- 隔離フォルダが存在しなければ作成する。
- 日本語フォント設定を行う。
- 初期スキャン、`PicViewerApp` の生成、`eframe::run_native` の起動を行う。

主な失敗時動作:

- 入力フォルダが存在しない、またはディレクトリでない場合は起動前にエラー終了。
- `<入力>/delete` がファイルの場合は起動前にエラー終了。
- 隔離フォルダ作成失敗時は起動前にエラー終了。

### 2.2 app.rs

責務:

- `eframe::App` 実装。
- 毎フレームの `update()` で、入力、描画、タイマー、キャッシュ補充、rescan 呼び出しを制御する。
- `pending_free` の毎フレーム冒頭 drop を実行する。
- 再生/停止、削除メニュー、Undo、全画面切替、エラー表示を扱う。

主な保持状態（型定義は §3.5 `PicViewerApp` を参照。出所は一本化する）:

- `PlaybackState` / `CacheState` / `QuarantineState` / `ScannerState`
- 再生中フラグ（`playing`）
- 最終 advance 時刻（`last_advance`）
- 最終 rescan 時刻は `ScannerState.last_rescan` を唯一の出所とし、App には重複して持たない
- 一時エラー（`error`: メッセージ＋期限）
- 削除確認メニュー状態（`menu`）
- 全画面状態（`fullscreen`）

### 2.3 playback.rs

責務:

- `play_order` と `cursor` の管理。
- シャッフル、サイクル境界処理、表示機会の消費、失敗パスの末尾退避または恒久除外を扱う。
- `window` と `play_order` の対応を保つためのインデックス操作を提供する。

### 2.4 cache.rs

責務:

- `VecDeque<CachedImage>` による VRAM 窓管理。
- 画像デコード要求を `image_loader` へ委譲し、成功時に egui テクスチャを生成する。
- 1フレームあたり最大 N 枚の補充予算を守る。
- `current_vram` と `window` の整合性を維持する。
- 破棄対象テクスチャを `pending_free` に退避する。

### 2.5 quarantine.rs

責務:

- 削除対象画像を `<入力>/delete` へ `std::fs::rename` で移動する。
- 衝突回避ファイル名を生成する。
- Undo 用の直前履歴を保持する。
- Undo 時に隔離先から元パスへ `rename` する。

### 2.6 scanner.rs

責務:

- 起動時スキャン。
- 10秒ごとの再スキャン。
- 追加ファイルの pending 管理。
- 書き込み完了判定。
- 外部消失の検出。

rescan は UI スレッドで実行する。巨大フォルダでは一時的に 100ms を超える可能性があるが、本仕様では許容する。

### 2.7 image_loader.rs

責務:

- 対応拡張子判定。
- **寸法のみのプローブ**: `image::image_dimensions(path) -> ImageResult<(u32, u32)>` でフルデコードせずに `(width, height)` を取得する（ヘッダ読みのみで高速。追加フィーチャ不要）。先読み候補の VRAM 見積もりに用いる。
- `image::open` による画像読み込み（フルデコード）。
- RGBA8 への変換。
- egui `ColorImage` 用データ生成。
- VRAM 見積もり値 `width * height * 4` の算出（プローブ結果・デコード結果のいずれからも算出可能）。

## 3. 主要データ構造

### 3.1 PlaybackState

```rust
pub struct PlaybackState {
    pub play_order: Vec<PathBuf>,
    pub cursor: usize,
    pub pending: Vec<PendingFile>,
    pub fail_counts: HashMap<PathBuf, u32>,
    pub cycle_count: u32,
}

pub struct PendingFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}
```

`cursor` は、窓が空でなければ `window[0]` に対応する `play_order` インデックスを表す。`cursor` を進めるのは advance のみ（削除・補充失敗では進めない）であり、値は「このサイクルで実際に表示した枚数」と一致する。

### 3.2 CacheState

```rust
pub struct CacheState {
    pub window: VecDeque<CachedImage>,
    pub current_vram: usize,
    pub pending_free: Vec<TextureHandle>,
    pub preload_blocked_by_vram: bool,
}

pub struct CachedImage {
    pub path: PathBuf,
    pub texture: TextureHandle,
    pub width: f32,
    pub height: f32,
    pub vram_size: usize,
}
```

`current_vram` は常に `window.iter().map(|img| img.vram_size).sum()` と一致させる。`pending_free` へ退避した時点で `current_vram` から差し引く。`preload_blocked_by_vram` は、3枚目以降の先読み候補が VRAM 上限を超えるためキャッシュ投入を見送った状態を表す。advance、delete、外部消失などで `window` が減った場合、または Undo/rescan/失敗退避などで `play_order[cursor + window.len()..]` の候補列が変わった場合は `false` に戻す。

### 3.3 QuarantineState

```rust
pub struct QuarantineState {
    pub dir: PathBuf,
    pub last: Option<DeleteRecord>,
}

pub struct DeleteRecord {
    pub original: PathBuf,
    pub quarantined: PathBuf,
}
```

Undo は直前1件のみ対応する。新しい削除が成功したら古い `last` は上書きする。

### 3.4 ScannerState

```rust
pub struct ScannerState {
    pub last_rescan: Instant,   // rescan タイマーの唯一の基準。App には別途持たない。
    pub interval: Duration,     // 既定 10 秒
}
```

pending ファイル群は `PlaybackState.pending` に保持する。scanner はそれを更新する。rescan の最終実行時刻は本構造体 `last_rescan` を**唯一の出所**とし、`PicViewerApp` 側に重複して持たない。

### 3.5 PicViewerApp（最上位状態）

`eframe::App` を実装する最上位構造体。タイマー・UI 状態を集約して所有する。

```rust
pub struct PicViewerApp {
    pub playback: PlaybackState,
    pub cache: CacheState,
    pub quarantine: QuarantineState,
    pub scanner: ScannerState,

    pub playing: bool,            // 再生中フラグ
    pub last_advance: Instant,    // advance タイマーの基準
    pub advance_interval: Duration, // 既定 15 秒

    pub fullscreen: bool,         // 全画面状態
    pub menu_open: bool,          // 前フレームで削除メニューが開いていたか（入力抑制用）
    pub menu_target: Option<PathBuf>, // 右クリック時点の削除対象パス
    pub error: Option<ErrorMessage>, // 一時エラー（メッセージ＋期限）
}

pub struct ErrorMessage {
    pub text: String,
    pub expires_at: Instant,      // 3〜5 秒後
}
```

rescan の時刻は `scanner.last_rescan`、advance の時刻は `last_advance` を用いる（出所を一本化）。削除メニューの対象は `window_index` ではなく、右クリック時点の `PathBuf` として `menu_target` に保持する。表示更新や rescan により左右の位置が入れ替わっても、別画像を誤削除しないためである。

### 3.6 主要定数

実装はハードコードでよい（設定ファイル化はスコープ外）。実測後に調整し得る値。

```rust
const VRAM_LIMIT: usize = 2 * 1024 * 1024 * 1024; // 2 GiB（窓の推定 VRAM 上限）
const REFILL_BUDGET_N: usize = 2;                  // 1フレームあたり最大デコード枚数（初回フレームは例外、§5）
const INITIAL_FILL_ATTEMPT_LIMIT: usize = 8;        // 初回フレームで2枚揃えるために試す最大候補数
const ADVANCE_INTERVAL_SECS: u64 = 15;             // スライド切替間隔
const RESCAN_INTERVAL_SECS: u64 = 10;              // 再スキャン間隔
const MAX_DECODE_FAILS: u32 = 3;                   // 連続デコード失敗の恒久除外しきい値
const ERROR_MSG_SECS: u64 = 4;                     // 一時エラー表示時間（3〜5秒の範囲）
```

`VRAM_LIMIT`・`REFILL_BUDGET_N`・`INITIAL_FILL_ATTEMPT_LIMIT` は §8、間隔は §3.5 のタイマー、`MAX_DECODE_FAILS` は §12 から参照する。

## 4. 不変条件

### 4.1 play_order と window

- `play_order.is_empty()` の場合、`cursor == 0`、`window.is_empty()` とする。
- `window` が空でなければ `window[0].path == play_order[cursor]`。
- `window[i].path == play_order[cursor + i]` が成り立つ範囲だけを `window` に保持する。
- 補充元インデックスは常に `cursor + window.len()`。
- rescan や Undo で並び替えてよい範囲は `play_order[cursor + window.len()..]` のみ。
- 表示済み領域 `play_order[..cursor]` は rescan では触らない。

### 4.2 cursor

- `cursor` は「現在表示中の左画像の位置」である。
- **`cursor` を進めるのは advance のみ**で、実際に表示していた枚数（最大2）だけ進める。
- 削除操作では `cursor` を進めない。
- 補充失敗（表示用スロット・先読み用とも）では `cursor` を進めない（§12.2）。
- 上記により `cursor` は「このサイクルで実際に表示した枚数」と一致する。

### 4.3 TextureHandle

- 描画に使ったフレーム内で `TextureHandle` を直接 drop しない。
- 削除、advance、サイクル境界、外部消失で窓から除去したテクスチャは `pending_free` に移す。
- `pending_free` は毎フレームの `update()` 冒頭で drop する。

## 5. update() フロー

`app.rs` の `update()` は次の順序で処理する。ポインタ由来の操作（右クリックメニュー、背景ダブルクリック、各ボタン）は egui のイミディエイトモード特性上 UI 描画中に検出されるため、**検出は描画中に行い、状態を変える実処理は描画後にまとめて適用**する（特に削除はテクスチャを `pending_free` に積むため、同一フレームで drop しない順序を守る）。

1. **前フレームで `pending_free` に入った `TextureHandle` を drop する。**（毎フレーム冒頭・例外なし）
2. キーボード入力を読む。**削除メニュー表示中は、キャンセル以外のキーボードショートカット（`Space`/`Ctrl+Z`/`F11`）を抑制する。**
3. 抑制されていなければ: `Ctrl+Z` → Undo（§10）、`Space` → 再生/停止トグル、`F11` → 全画面トグル要求を立てる。
4. rescan タイマーが期限切れなら `scanner::rescan` を実行する（§11）。
5. 再生中かつ advance タイマーが期限切れなら `advance` を実行し（§7）、最終 advance 時刻を更新する。
6. `cache::refill_window` を当該フレームの予算内で実行する（§8）。
7. UI を描画する（§13）。この中で次を**検出のみ**行い、実処理は描画後へ繰り延べる。削除メニュー表示中は、削除/キャンセル以外の UI 操作要求（再生/停止ボタン、Undo ボタン、背景ダブルクリックによる全画面切替）も生成しない:
   - 画像の右クリック → 削除メニューを開く（メニュー状態を更新。右クリック時点の対象 `PathBuf` を `menu_target` に記録）。
   - メニューの「削除」クリック → `delete_request = Some(menu_target_path)`。「キャンセル」/メニュー外クリック → メニューを閉じる。
   - 背景のダブルクリック（`double_clicked()`）→ 全画面トグル要求を立てる。
   - 再生/停止ボタン・Undo ボタンのクリック → それぞれの要求を立てる。
8. **描画後に繰り延べた処理を適用する**:
   - 全画面トグル要求があれば `ViewportCommand::Fullscreen(新状態)` を送る。
   - `delete_request` があれば §9 の削除手順を実行する（成功時、除去テクスチャは `pending_free` へ。実 drop は次フレーム step 1）。
   - Undo ボタン要求があれば §10 を実行する。
9. エラーメッセージの期限切れ（3〜5秒）を処理する。
10. 再生中・補充が必要・タイマー待ちのいずれかなら `ctx.request_repaint_after` を設定する。

**初回フレームの扱い**: 初回 `update()` では `window[0]`/`window[1]` を満たすデコードを優先する。起動「1秒以内」目標のため、**初回フレームに限り `refill_window` の N 枚/フレーム予算を超えてよい**が、壊れた画像が大量にある場合に初回 update が長時間止まらないよう、試行数は `INITIAL_FILL_ATTEMPT_LIMIT` までとする。条件は「2枚揃う」「候補が尽きる」「試行数が `INITIAL_FILL_ATTEMPT_LIMIT` に達する」のいずれかで打ち切る（VRAM 上限は §8.2 のとおり表示用2枚を常に優先）。ただし全画像プリロードは行わない。2回目以降のフレームは通常どおり N 予算に従う。

## 6. 起動フロー

1. 入力フォルダを取得する。
2. 入力フォルダの存在とディレクトリ性を検証する。
3. 隔離フォルダパス `<入力>/delete` を作る。
4. 隔離フォルダパスが通常ファイルならエラー終了する。
5. 隔離フォルダがなければ作成する。
6. 入力フォルダ直下をスキャンし、対応拡張子のファイルだけを取得する。
7. サブフォルダはすべて無視する。
8. `play_order` をシャッフルする。
9. `cursor = 0`、`cycle_count = 0`、`fail_counts = {}` で初期化する。
10. `PicViewerApp` を生成して eframe を起動する。

## 7. advance 設計

### 7.1 入力

- `PlaybackState`
- `CacheState`
- 現在表示していた枚数

現在表示していた枚数は `min(window.len(), 2)` とする。

### 7.2 手順

1. `play_order` が空なら何もしない。
2. `window` の先頭から現在表示枚数分を取り除く。
3. 取り除いた画像のパスを `last_shown: Vec<PathBuf>` として保持する。
4. 取り除いた画像の `TextureHandle` を `pending_free` へ退避する。
5. `current_vram` を減算する。
6. `preload_blocked_by_vram = false` に戻す（窓が進み、先読み可能な余地が変わったため）。
7. `cursor += 表示枚数`。
8. `cursor >= play_order.len()` ならサイクル境界処理へ進む。
9. そうでなければ `refill_window` を呼び出す。

### 7.3 サイクル境界

1. `last_shown` を保持したまま、残っている `window` をすべて `pending_free` へ退避する。
2. `window.clear()`、`current_vram = 0`、`preload_blocked_by_vram = false` とする。
3. `cursor = 0` に戻す。
4. `play_order` を再シャッフルする。
5. `last_shown` に含まれるパスが新サイクル先頭に来た場合、可能なら後方の要素と入れ替える。
6. `play_order.len() <= 2` などで回避不能な場合は、そのまま許容する。
7. `cycle_count += 1`。
8. **このフレーム内では補充しない**（refill は次フレームの `update()` step 6 に委ねる）。そのため境界の1フレームだけ `window` が空になり「補充中」表示になり得るが、15秒間隔に対し1フレームの空表示は無視できるため許容する。次フレームで `play_order[0..]` から通常補充される。

## 8. キャッシュ補充設計

### 8.1 refill_window

目的:

- `window` を `play_order[cursor..]` の連続ミラーとして維持する。
- 表示用2枚を最優先で確保する。
- それ以上の先読みは VRAM 上限と N 枚/フレーム予算に従う。

手順:

1. `play_order` が空なら終了する。
2. `window.len() < 2` の間は、VRAM 上限に関係なく表示用候補を補充する。
3. `window.len() >= 2` の場合は、`current_vram >= VRAM_LIMIT` または `preload_blocked_by_vram == true` なら終了する。この早期終了は N 予算を消費しない。N 予算を消費するのは、候補を実際にデコード／スキップ処理した場合である（VRAM 超過の検出による即時終了は step 12 のとおり消費しない）。
4. 補充候補インデックス `idx = cursor + window.len()` を求める。
5. `idx >= play_order.len()` なら終了する。
6. `play_order[idx]` が存在しなければ、該当パスを `play_order` から除去して次候補へ進む。
7. **表示用スロットの補充（`window.len() < 2`）**: VRAM 上限に関係なく `image_loader::load_rgba` でフルデコードする。成功したら `CachedImage` を `window.push_back` する。失敗時は step 9 以降。
8. **先読みの補充（`window.len() >= 2`）**: まず `image_loader::image_dimensions` で**寸法のみ取得**し、`vram_size = width * height * 4` を見積もる。
   - `current_vram + vram_size > VRAM_LIMIT` なら、**フルデコードせず**に `preload_blocked_by_vram = true` として補充を終了する。該当パスは `play_order` に残す（表示用スロットへ来たときは step 7 / §8.2 により上限より表示を優先する）。無駄なフルデコードを発生させない。
   - 上限内なら `load_rgba` でフルデコードし、`CachedImage` を `window.push_back` する。
   - 寸法取得自体が失敗した場合（ヘッダ破損等）はデコード失敗として扱い、step 9 以降へ進む。
9. デコード（または寸法取得）に失敗したら `fail_counts[path] += 1` とする。
10. 失敗回数が3未満なら、該当パスを `play_order[idx]` から取り除き、`play_order[cursor + window.len()..]` の末尾へ退避する。
11. 失敗回数が3以上なら、該当パスを `play_order` から除去する。
12. 成功・失敗・外部消失は1試行として N 予算を消費する。VRAM 超過の検出（寸法プローブで上限超過と判明）は、その時点で即 `break` して同フレームの補充を終えるため、予算加算は省略する（加算してもしなくても同フレームの補充は終了するので挙動差は無い。実装もこれに合わせて加算しない）。
13. step 10 の末尾退避や step 11 の除去によって `play_order[cursor + window.len()..]` の候補列が変わった場合は、`preload_blocked_by_vram = false` に戻す。

### 8.2 表示用2枚と VRAM 上限

巨大画像2枚で 2GB を超える場合でも、`window[0]` と `window[1]` は保持する（表示用スロットの補充はフルデコードを直接行う）。3枚目以降の先読みは、**フルデコード前に `image_dimensions` で寸法だけ読んで `vram_size` を見積もり**、追加後に `current_vram + vram_size > VRAM_LIMIT` となるなら先読みキャッシュへ入れない（フルデコードもしない）。候補パスは `play_order` に残し、`preload_blocked_by_vram = true` として以後の先読みを止める。advance、delete、外部消失で `window` が減った場合、または Undo/rescan/失敗退避などで窓より後ろの候補列が変わった場合は `preload_blocked_by_vram = false` に戻し、再び補充を試みる。

## 9. 削除設計

### 9.1 操作対象

右クリックされた画像の `PathBuf` を、メニューを開いた時点で `menu_target` として保持する。削除実行時は `window_index` ではなく、このパスを対象とする。

`window_index` だけを保持すると、メニュー表示中に advance/rescan が走った場合、同じ左右位置に別画像が入り、ユーザーが右クリックした画像ではないファイルを削除し得る。削除は実ファイル移動を伴うため、対象はパスで固定する。

### 9.2 手順

1. `menu_target` に保持している対象パスを取得する。
2. 対象パスが現在の `window` 内に存在するか検索する。存在しない場合は「対象画像が切り替わったため削除できない」旨を表示し、ファイル操作もメモリ状態変更も行わない。
3. 隔離先候補名を生成する。
4. 候補先が存在しなければ `std::fs::rename(src, dest)` を試す。
5. AlreadyExists 相当の失敗なら次の候補名で再試行する。
6. その他の失敗なら UI にエラー表示し、メモリ状態は変更しない。
7. 成功したら `window` から対象を除去する。
8. 対象 `TextureHandle` を `pending_free` へ退避する。
9. `current_vram` を減算する。
10. `preload_blocked_by_vram = false` に戻す（窓が減り、先読み可能な余地が変わったため）。
11. 対象パスを `play_order` から除去する。
12. `cursor` は変更しない。
13. `quarantine.last` を更新する。
14. `menu_target = None` にする。
15. `refill_window` を呼ぶ。

注意:

`std::fs::rename` は既存ターゲットを置換し得る。存在チェックと rename の間に他プロセスが同名ファイルを作る競合は完全には防げない。今回の単一ユーザー用途ではリトライループによるリスク縮小までとし、完全な排他作成は行わない。

## 10. Undo 設計

### 10.1 手順

1. `quarantine.last` が `None` なら何もしない。
2. 元パスが既に存在する場合はエラー表示し、状態変更しない。
3. `std::fs::rename(quarantined, original)` を実行する。
4. 失敗したらエラー表示し、状態変更しない。
5. 成功したら `quarantine.last = None` にする。
6. `original` を `play_order[cursor + window.len()..]` の範囲へ追加する。
7. `play_order[cursor + window.len()..]` のみを再シャッフルする。
8. `preload_blocked_by_vram = false` に戻す（窓より後ろの候補列が変わったため）。
9. 次回以降の補充で自然に取り込む。

Undo は現在表示中・先読み済みの窓を壊さない。

`play_order` への追加（step 6）を完了してから制御を返すこと。これにより直後の rescan が元パスを「新規」と誤認して `pending` へ二重登録することを防ぐ（rescan の既知集合 `known = play_order ∪ pending` に既に含まれるため。§11.2-2,3）。

## 11. rescan 設計

### 11.1 起動時スキャン

- `read_dir(input_dir)` を使う。
- 直下の通常ファイルだけを対象とする。
- サブフォルダはすべて無視する。
- 拡張子は小文字化して `jpg, jpeg, png, gif, bmp, webp` のみ対象。

### 11.2 定期 rescan

10秒ごとに UI スレッドで実行する。

手順:

1. 入力フォルダ直下の対象画像集合 `scanned` を作る。
2. `known = play_order ∪ pending` を作る（この時点の既存集合。以降の昇格・追加判定の基準）。
3. **既存 `pending`** の各要素について metadata を再取得する。**新規検出（手順6）より前に行う**こと。
4. サイズと更新時刻が前回値と同じなら書き込み完了とみなし、`play_order[cursor + window.len()..]` へ追加する。追加が発生した場合は `preload_blocked_by_vram = false` に戻す（窓より後ろの候補列が変わったため）。
5. サイズまたは更新時刻が変わっていれば pending の観測値を更新する。昇格前に消えていれば pending から落とす。
6. `scanned - known` を新規候補として `pending` へ追加する（**この rescan では昇格させず、次回 rescan の昇格判定に回す**）。手順3を先に済ませてあるため、今回追加した新規は同一呼び出しで昇格しない。これが書き込み途中ファイル保護（要求 §2.7／1スキャン分の遅延）の要点である。
7. `known - scanned` を外部消失として扱う。
8. 外部消失が `play_order[cursor + window.len()..]` にあれば除去する。除去が発生した場合は `preload_blocked_by_vram = false` に戻す（窓より後ろの候補列が変わったため）。
9. 外部消失が窓内未表示キャッシュにあれば、対応する `window` と `play_order` の要素を除去して `pending_free` へ退避し、`preload_blocked_by_vram = false` に戻す。
10. 外部消失が現在表示中の `window[0]/[1]` なら、テクスチャが残っている限り表示継続してよい。
11. 新規追加があった場合は `play_order[cursor + window.len()..]` のみ再シャッフルし、`preload_blocked_by_vram = false` に戻す。

表示済み領域 `play_order[..cursor]` は触らない。表示済み領域にある外部消失パスは、次サイクル以降に自然に清算する。

## 12. デコード失敗設計

### 12.1 fail_counts

- キーは `PathBuf`。
- 成功したら 0 にリセットまたは削除する。
- 失敗したら +1。
- 3回に達したら当該セッションでは `play_order` から恒久除外する。
- 次回起動では再スキャンにより再試行される。

### 12.2 補充失敗の統一処理（表示用スロット・先読み用とも同一）

補充は連続ミラー方式により、対象が `window[0]`/`window[1]`（表示用）か先読み用かに関わらず、常に `idx = cursor + window.len()` での同一操作である。したがってデコード失敗・外部消失の処理も両者で同一とし、**`cursor` は進めない**。

> 重要: 「除去」と「`cursor++`」を両方行ってはならない。除去により次の画像が同じスロットへ繰り上がるため `cursor` を進める必要はなく、進めると次の画像を1枚飛ばし、`window[0].path == play_order[cursor]` の不変条件（§4.1）が破れる。初回同期デコードや `window[0]`/`window[1]` を満たす段階の失敗も、この同一処理に従う。

手順:

1. 補充位置 `idx = cursor + window.len()` を求める。
2. 該当パスの失敗回数を増やす（`fail_counts[path] += 1`）。
3. `play_order[idx]` を除去する。
4. 失敗回数が3未満なら `play_order[cursor + window.len()..]` の末尾へ退避する（このサイクルの後半で再試行）。
5. 失敗回数が3以上なら退避せず恒久除外する。
6. `cursor` は進めない。
7. 外部消失（デコード以前に存在しない）は失敗回数に数えず、`play_order[idx]` を即除去する。
8. 成功・失敗・外部消失のいずれも1試行として N 予算を消費する。
9. step 4 の末尾退避・step 5 の恒久除外・step 7 の即除去のいずれかで `play_order[cursor + window.len()..]` の候補列が変わった場合は、`preload_blocked_by_vram = false` に戻す（§8.1-13 と同じ。以前「大きすぎる」と判定した候補がもう先頭でない可能性があるため）。

## 13. UI 設計

### 13.1 上部バー

表示項目:

- 再生/停止ボタン
- 残り未表示枚数
- キャッシュ枚数
- サイクル回数
- Undo ボタン
- 一時エラーメッセージ

残り未表示枚数:

```text
remaining = play_order.len().saturating_sub(cursor)
```

空状態では 0 と表示する。

### 13.2 画像領域

- `window[0]` を左、`window[1]` を右に表示する。
- `window.len() == 1` の場合は1枚のみ中央寄せまたは左寄せで表示する。
- 画像は表示領域の高さを揃え、合計幅が領域を超えないよう縮小する。
- アスペクト比は維持する。

### 13.3 削除メニュー

- 画像右クリックで対象画像に紐づくメニューを表示する。
- メニュー対象は左右位置ではなく、右クリック時点の `PathBuf` として固定する。削除実行時にそのパスが現在の窓に存在しなければ削除しない。
- メニュー表示中は削除/キャンセル以外の入力を抑制する。
- `Space`、`Ctrl+Z`、`F11` も抑制対象とする。

### 13.4 全画面

- 背景ダブルクリックまたは `F11` で切り替える。
- `ViewportCommand::Fullscreen(bool)` を使う。
- マルチモニタ環境では、カーソルのあるモニタで全画面化される winit 既定挙動を前提とする。要求定義 §2.9 の「マルチモニタで正しく動く」を満たすか実機で確認し、満たさない場合のモニタ指定方法は実装時に eframe/winit のドキュメントで調べる（本書では既定挙動を採用）。

### 13.5 エラー表示

- 画面上部に 3〜5 秒表示する。
- モーダルにはしない。
- 削除失敗、Undo 失敗、隔離フォルダ作成失敗、継続的な rescan 失敗を表示対象とする。
- 単発 metadata 失敗は UI に出さなくてよい。

## 14. 空状態設計

`play_order` が空になった場合:

- `cursor = 0`
- `window.clear()`
- `current_vram = 0`
- `preload_blocked_by_vram = false`
- 空フォルダ状態を表示する。
- advance、補充、サイクル境界処理は行わない。
- rescan は継続する。
- 新規画像が pending 昇格したら通常動作へ復帰する。

## 15. エラー処理方針

| 場面 | 処理 |
| --- | --- |
| 入力フォルダ不正 | 起動前エラー |
| `<入力>/delete` がファイル | 起動前エラー |
| 隔離フォルダ作成失敗 | 起動前エラー |
| 削除 rename 失敗 | UI エラー、状態変更なし |
| Undo rename 失敗 | UI エラー、状態変更なし |
| デコード失敗 | スキップ、fail_counts 更新 |
| 外部消失 | play_order/window から除去 |
| metadata 取得失敗 | 原則 UI 表示なし、次回 rescan に持ち越し |

## 16. 受け入れ確認観点

- 起動直後に最初の1〜2枚が表示される。
- 全プリロード待ちが発生しない。
- 削除しない限り入力フォルダ直下の原本ファイル名・件数が変わらない。
- 削除成功時のみ `delete` フォルダへ移動し、表示順から消える。
- Undo 成功時のみ元パスへ戻り、以後の表示候補へ復帰する。
- 同名衝突時に既存ファイルを上書きしない。
- `pending_free` 遅延により wgpu の texture destroyed パニックが出ない。
- rescan で追加画像が取り込まれる。
- 壊れた画像でクラッシュしない。
- 空フォルダ状態でサイクルカウントが空回りしない。

## 17. 実装時の注意

- 実装は §2 のモジュール単位で段階的に進め、各段階で `cargo build` を通す。
- `cargo build --release` を確認に使う。
- デバッグビルドでのデコード速度は性能判断に使わない。
- eframe/egui 0.31 の API 差異は公式ドキュメントで確認する。
- `std::fs::rename` の Windows 挙動は上書き可能性を前提に扱う。
- `image` クレートはデフォルトフィーチャを使う。
