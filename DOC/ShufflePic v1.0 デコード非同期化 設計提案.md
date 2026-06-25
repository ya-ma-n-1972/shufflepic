# ShufflePic v1.0 設計提案：デコードの別スレッド化（UI スレッド非ブロック化）

| 項目 | 内容 |
| --- | --- |
| 種別 | 設計提案（v1.0 向け） |
| 対象読者 | 開発者 |
| 前提 | 前身 実装（`../v1.0/src/`）と詳細設計書を理解していること |
| 状態 | 採用方針（v1.0 で実装済み）。競合時は v1.0 要求定義書・v1.0 詳細設計書を優先 |

---

> **本書は v1.0 詳細設計書 §4.1 / §4.8 と整合済み。** 採用する機構は、**表示用／先読み用の二段優先キュー**と
> **epoch による世代識別**。RAM は **F-8（巨大画像を `oversized` へ退避して寸法上限以下に限定）**＋先読み枚数上限で
> 有界化し、**CPU RAM バイト予約プロトコルは設けない**。実装が両文書で食い違う場合は詳細設計書を正とする。

---

## 1. 背景と目的

前身 は「複数枚をあらかじめ先読みし、VRAM 上限で管理する」窓型プリロードにより、
pqiv / IrfanView 系の「1 枚先読み＋遷移時デコード」より滑らかな切替を狙った設計である。

しかし **デコードは UI スレッド上で同期実行**される。具体的には `cache::refill_window()`
が `app.rs` の `update()` 内で `image::open()`（`image_loader::load_rgba`）を直接呼ぶ。
そのため次の状況で UI スレッドがブロックし、コマ落ち（ヒッチ）が発生し得る。

- 非常に大きい画像（例: 8000×6000 級）を 1 枚デコードするフレーム。数百 ms 単位で停止し得る。
- `window.len() < 2`（表示枠が空）のとき、VRAM 上限を無視して**即フルデコードを強制**する経路
  （`cache.rs` の「表示用スロット」分岐）。起動直後・削除直後・サイクル境界直後に顕著。
- フレーム予算 `N=2` による分割は「1 フレームあたりの枚数」を絞るだけで、
  **1 枚あたりのデコード時間そのもの**は分割できない。

**目的：デコード（および寸法プローブ・I/O）を UI スレッドから完全に外し、`update()` の
1 フレームが画像デコードでブロックしないようにする。** テクスチャ生成（GPU アップロード）と
状態更新のみを UI スレッドに残す。

非目標（v1.0 では扱わない）：

- デコード結果のディスクキャッシュ。
- 3 枚以上の同時表示。

本書は F-1 のデコード非同期化だけを扱う。v1.0 全体では F-3 により Undo を廃止し、
F-7 により表示枚数を 1 または 2 の可変とする。その他の機能との統合は
「ShufflePic v1.0 詳細設計書」を正とする。

---

## 2. 前身 の制約（現状整理）

| 箇所 | 現状 | 問題 |
| --- | --- | --- |
| `cache::refill_window` | UI スレッドで同期デコード | 大画像で 1 フレームがブロック |
| 表示用スロット（`wlen < 2`） | VRAM 無視で即フルデコード | 起動/削除直後にヒッチ |
| 寸法プローブ `image_loader::dimensions` | UI スレッドで header I/O | I/O 待ちが UI に乗る |
| フレーム予算 `N=2` | 枚数のみ分割 | 1 枚の重さは緩和されない |

維持すべき不変条件（前身 から継承。詳細設計 §8.1 等）：

- 窓（`window`）は `play_order[cursor..]` の**連続ミラー**である。
- `cursor` を進めるのは `advance` のみ（削除・補充失敗・デコード失敗では進めない）。
- VRAM 推定合計 `current_vram` は常に窓内 `vram_size` の合計と一致する。
- 原本は読み取り専用。削除は隔離移動のみ。テクスチャ破棄は次フレーム冒頭で `pending_free` を drop。

---

## 3. 設計方針

1. **デコードはワーカースレッド（プール）で実行**し、結果（CPU 上の `egui::ColorImage` + 寸法）を
   チャネルで UI スレッドへ返す。**`load_texture`（GPU アップロード）だけは UI スレッドで行う**
   （小コスト・egui のフレーム同期に乗る）。
2. UI スレッドは「**要求を出す**」「**到着済み結果を窓へ反映する**」だけを行い、**待たない**。
   到着が間に合わなければ該当スロットは「読み込み中」表示のまま次フレームへ。
3. 先読みの深さは **VRAM バイト上限（窓に置いた分）** と
   **読み先行パイプラインの枚数上限（in-flight + ready buffer）** で制限する。
   デコード対象は F-8 で寸法上限以下に限定されるため、CPU 側 RGBA は「先読み枚数 × 上限サイズ」で有界
   （バイト予約は不要）。寸法プローブは UI から外し、ワーカーが上限判定（`Oversized`）も担う。
4. **epoch による世代無効化とパス基準検証を併用**する。到着結果は
   「現在世代か」「今も `play_order` 上の近傍で必要か」を検証し、不要なら破棄する。

---

## 4. アーキテクチャ

```
            (UI スレッド: eframe update)
  ┌───────────────────────────────────────────────┐
  │ refill_window():                               │
  │   1. ready_buffer から該当パスを取り出し         │
  │      → load_texture → window へ push            │
  │   2. 不足分の次パスを decode 要求（重複/上限制御） │
  │   3. VRAM 上限・パイプライン上限で停止            │
  └───────────────▲───────────────────┬────────────┘
       DecodeResult │                   │ DecodeRequest
   (ColorImage+dims)│                   ▼
  ┌────────────────┴───────────────────────────────┐
  │ priority queue ─┐                                 │
  │ prefetch queue ─┴─(priority first)→ Worker ×K    │
  │  worker: image::open → to_rgba8 → ColorImage    │
  │          + dims を返す。失敗は Err を返す         │
  │  結果送信後に ctx.request_repaint() で UI を起こす │
  └─────────────────────────────────────────────────┘
```

- ワーカー → UI への通知は `ctx.request_repaint()`（`egui::Context` は `Send + Sync`、
  スレッド外から呼べる。egui の標準的な非同期パターン）。
- ワーカー数 `K = clamp(利用可能コア数 - 1, 1..=4)` を初期値とする（UI スレッドを枯渇させない）。

---

## 5. データ構造（提案）

`image_loader.rs`

```rust
/// ワーカーがデコードして返す結果（CPU 上、GPU 未アップロード）。
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub color: egui::ColorImage, // load_texture にそのまま渡せる形
    pub rgba_bytes: usize,       // ready_bytes 集計に使用
}

pub fn decode_color(path: &Path) -> Result<DecodedImage, DecodeError>;
// ワーカーは「寸法取得 → 上限判定（F-8）→ デコード」の順。
// DecodeError は Missing / Failed / Oversized（寸法上限超過）を区別する。
```

新規モジュール `decoder.rs`（ワーカープールとチャネル）

```rust
pub struct FileFingerprint {
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
}

pub struct DecodeRequest {
    pub path: PathBuf,
    pub epoch: u64,   // 必須：投入時点の候補列世代
    pub fingerprint: FileFingerprint, // 要求時の長さ・更新時刻等
}

pub enum DecodeOutcome {
    Ok(DecodedImage),
    Failed,                       // デコード失敗（カウンタ対象）
    Missing,                      // open 時点で存在しない（カウンタ非対象）
    Oversized { w: u32, h: u32 }, // 寸法上限超過。F-8 で oversized フォルダへ退避（カウンタ非対象）
}

pub struct DecodeResult {
    pub path: PathBuf,
    pub epoch: u64,
    pub fingerprint: FileFingerprint,
    pub outcome: DecodeOutcome,
}

pub struct DecoderPool {
    tx_priority: Sender<DecodeRequest>, // 表示枠用 bounded queue
    tx_prefetch: Sender<DecodeRequest>, // 先読み用 bounded queue
    rx_res: Receiver<DecodeResult>,     // workers → UI（bounded）
    current_epoch: Arc<AtomicU64>,      // ワーカーのデコード前失効判定
    // join handles 等
}

impl DecoderPool {
    pub fn new(ctx: egui::Context, workers: usize) -> Self;
    pub fn request_display(&self, req: DecodeRequest) -> Result<(), TrySendError<DecodeRequest>>;
    pub fn request_prefetch(&self, req: DecodeRequest) -> Result<(), TrySendError<DecodeRequest>>;
    pub fn try_recv(&self) -> Option<DecodeResult>; // 非ブロッキング
    pub fn set_epoch(&self, epoch: u64);
}
```

`cache.rs`（`CacheState` 拡張）

```rust
pub struct CacheState {
    pub window: VecDeque<CachedImage>,   // 既存：Ready なテクスチャのみ
    pub current_vram: usize,
    pub pending_free: Vec<TextureHandle>,
    pub preload_blocked_by_vram: bool,

    // ---- v1.0 追加 ----
    /// デコード済みだが未配置の結果（パス基準）。配置時に load_texture する。
    pub ready: HashMap<PathBuf, DecodedImage>,
    /// 要求済み・未到着のパス集合（重複要求防止）。世代更新時に全クリアする。
    pub inflight: HashSet<PathBuf>,
    /// ready 内の RGBA 展開後バイト合計。
    pub ready_bytes: usize,
    /// 現在の候補列世代。
    pub epoch: u64,
}
```

定数（`main.rs`）

```rust
pub const DECODE_WORKERS: usize = 0;     // 0 = 自動（コア数-1 を clamp 1..=4）
pub const MAX_INFLIGHT: usize = 6;       // 同時デコード要求の上限
pub const MAX_READY_BUFFER: usize = 6;   // 未配置の保持上限（F-8 の寸法上限と合わせ RAM を有界化）
pub const READAHEAD_DEPTH: usize = 8;    // cursor から先読み要求を出す最大相対距離
pub const MAX_RESULTS_PER_FRAME: usize = 4;
pub const MAX_UPLOADS_PER_FRAME: usize = 2;
pub const DISPLAY_QUEUE_CAPACITY: usize = 2;
pub const PREFETCH_QUEUE_CAPACITY: usize = 6;
pub const RESULT_QUEUE_CAPACITY: usize = 8;

// F-8 巨大画像の退避（初期値。実測で調整可）
pub const OVERSIZED_MAX_PIXELS: u64 = 32_000_000;
pub const OVERSIZED_MAX_SIDE: u32 = 10_000;
pub const OVERSIZED_DIR_NAME: &str = "oversized";
```

---

## 6. 制御フロー

### 6.1 `refill_window`（UI スレッド・非ブロック化後）

毎フレーム、ブロックせずに以下を行う。

1. **結果回収**：`pool.try_recv()` を `MAX_RESULTS_PER_FRAME` 件まで回す。各 `DecodeResult` について
   - 失効判定（§7。epoch・fingerprint・パス）に合致 → 破棄
     （`ColorImage` を drop、`inflight` から除去し、必要なら現ファイルを新規要求）。
   - `Ok` → `ready` に格納（`ready_bytes += rgba_bytes`）。`Failed`/`Missing`/`Oversized` → §6.3 の処理。
   - いずれも `inflight` から当該パスを除去。
2. **配置**：`idx = cursor + window.len()` から前方へ、`play_order[idx]` が `ready` にあれば
   取り出して `load_texture` → `window.push_back`、`current_vram += vram`。
   1 フレーム最大 `MAX_UPLOADS_PER_FRAME` 件、かつ `window` 連続性を満たす限り続ける。
   先頭 `display_count` 枚は VRAM 上限より表示を優先し、それ以降だけ VRAM 上限で停止する。
3. **要求発行**：まだ窓が埋まらず（表示枠 `display_count` + VRAM/パイプライン余地あり）かつ
   `inflight.len() < MAX_INFLIGHT` かつ `ready.len() < MAX_READY_BUFFER` の範囲で、
   `play_order[cursor + offset]`（`offset < READAHEAD_DEPTH`）のうち
   `ready` にも `inflight` にも無いパスへ `request()` を出す。
   - 先頭 `display_count` 枚（表示枠）は高優先キューへ、それ以降は通常キューへ投入する。
4. **VRAM ゲート**：`window` に置いた分の合計が `VRAM_LIMIT` を超えそうなら配置を止め
   `preload_blocked_by_vram = true`（前身 と同じ意味）。**ただしデコード要求自体は枚数上限で
   既に律速されており、寸法プローブを UI で行う必要はない**（寸法は結果に含まれる）。

> 前身 にあった「表示枠は VRAM 上限より優先する」という規則は維持する。
> 同期フルデコードだけを「表示枠は高優先キューで非同期デコード」に置き換える。
> 配置は到着後で、到着までは「読み込み中」を表示する。

### 6.2 表示（`app.rs`）

- `window.len() >= 1` の Ready 分を 前身 と同じレイアウトで描画。
- 表示枠が未到着（`window.len() < display_count`）の間は、不足スロットに「読み込み中...」を描画
  （前身 の空窓表示を流用）。
- 滑らかさのため、`update()` は結果到着でも再描画されるよう、ワーカーからの
  `request_repaint()` に任せる（ポーリング不要）。

### 6.3 デコード失敗 / 消失（パス基準）

前身 `handle_decode_fail` の意味を踏襲し、UI スレッドで実施する。

- `Failed`：`fail_counts[path] += 1`。`play_order` から当該パスを除去し、
  `< MAX_DECODE_FAILS` なら末尾へ退避、以上なら恒久除外。`preload_blocked_by_vram = false`。
- `Missing`：カウンタに数えず `play_order` から除去（前身 §8.1-6 と同じ）。
- `Oversized`：失敗回数に数えず、対象を `oversized` フォルダへ隔離移動する
  （F-8。詳細設計 §4.8）。`cursor` は進めない。
  - 成功時は `play_order` から除去する。
  - 通常の移動失敗時は通知を出さず、対象を未表示領域の末尾へ戻して `playing` を維持する。
    再び候補に選ばれた時点で再判定・再移動する（対象パス単位で失敗回数を数える）。
  - 同一パスの移動が累計 5 回失敗したら `halted`（補充・送り・退避・境界判定・rescan を停止）に入り、
    ファイル名を含むモーダルを表示して `playing=false` とする。原本と候補は残し、自動再試行を停止する。
  - 移動先の同名衝突は `delete` と共通の番号付与処理で回避し、上書きしない。
- いずれも成功時は `fail_counts.remove(path)`（前身 同様）。
- 処理後に `cursor >= play_order.len()` かつ窓空となった場合は、v1.0 詳細設計 §4.4 の
  `settle_cycle_or_empty()` でサイクル境界または空状態へ移行する。

---

## 7. 無効化（到着結果が古い場合の扱い）

シャッフル・削除・rescan・フォルダ変更で候補列が変わると、先行要求した結果が不要になり得る。
**epoch による世代無効化とパス基準検証を併用**する。

到着した `DecodeResult { path }` は、次のいずれかなら破棄する：

- `result.epoch != cache.epoch`。
- 要求時の `FileFingerprint`（少なくともファイル長・更新時刻。取得可能なら作成時刻も含む）が
  現在の同一パスの metadata と一致しない、または再取得できない。
- `path` が現在の `play_order` に存在しない（削除・恒久除外済み）。
- `path` が `play_order` 上で `cursor + READAHEAD_DEPTH` より後方にしか無い（当面不要）。

破棄時は `ready` に積まず `ColorImage` を即 drop。`ready` は配置時にも
「`play_order[cursor + window.len()]` と一致するか」で検証されるため、
順序入れ替え（`reshuffle_tail`）が起きても**誤った位置への配置は起こらない**
（一致しなければ単に保持され、近傍に来れば配置、遠ければ §7 の枚数/距離掃除で破棄）。

`ready` / `inflight` の肥大防止：各フレーム末に、`cursor + READAHEAD_DEPTH` より後方の
`ready` エントリを掃除する。`inflight` はキャンセル不能なので集合からの除去は到着時に行う
（到着結果は §7 の判定で破棄され得る）。

候補列を変える操作（reshuffle/cycle/delete/rescan/folder-change）で `epoch += 1` し、
`ready` と UI 側の inflight 管理をクリアする。`inflight` は `(path, epoch)` 単位とし、旧世代要求が
同じパスの新しい高優先要求を妨げないようにする。ワーカーもフルデコード開始前に共有 current epoch を確認し、
旧世代要求ならデコードせず破棄する。正しさの最終判定は到着時のパス基準検証で担保する。
候補列を変更する各処理は共通の世代更新ヘルパを経由し、更新漏れを防ぐ。

同じ epoch・同じパスでも外部でファイル内容が差し替えられる可能性があるため、epochだけには依存しない。
`Ok` / `Failed` / `Missing` は fingerprint 不一致なら現在状態へ適用しない。`Oversized` はファイル移動を伴うため、
fingerprint 一致確認に加えて移動直前に `image_dimensions` を再実行し、現在も上限超過の場合だけ移動する。

---

## 8. 並行性とライフサイクル

- **チャネル**：`crossbeam-channel` の bounded channel を採用する。
  表示用高優先キュー、先読み用通常キュー、結果キューの 3 本を持つ。
  ワーカーは `select_biased!` 等で高優先キューを優先して待機し、空の場合だけ通常キューを受信する。
  通常キューで単独ブロックし、高優先要求の到着を取り逃がす実装は禁止する。
  単一 FIFO に `priority` フラグを付けるだけの方式は採用しない。
- **起動**：`DecoderPool::new(ctx.clone(), K)` を `ShufflePicApp::new` または `eframe` 初期化時に生成。
- **終了**：`DecoderPool::Drop` では要求 Sender と結果 Receiver を先に drop して全チャネルを切断し、
  その後ワーカーを join する。結果キューが満杯のまま UI が終了しても、結果送信が切断エラーで解除され、
  join がデッドロックしない順序とする。デコード中の 1 枚は完了を待つ（短時間）。
- **再描画起床**：各ワーカーは結果送信後に `ctx.request_repaint()` を呼ぶ。
- **GPU アップロード**：`load_texture` は UI スレッドのみ（egui のフレームに同期）。
- **CPU RAM（F-8 で有界・予約なし）**：ワーカーは「寸法取得 → 上限判定」の順で進み、上限超過なら `Oversized` を
  返してデコードしない。デコードする画像は寸法上限以下なので、RAM は「(K + 結果キュー + `MAX_READY_BUFFER`)
  × 上限サイズ」で有界。バイト予約（`DecodeBudget`/`DecodeReservation`/`Deferred`）は設けない。

---

## 9. VRAM・予算の考え方（変更点）

| 項目 | 前身 | v1.0 |
| --- | --- | --- |
| VRAM 上限 | 窓配置分を `VRAM_LIMIT` で律速 | 同左（配置時に判定。維持） |
| デコード枚数の律速 | `max_attempts`（フレーム予算 N） | `MAX_INFLIGHT` / `MAX_READY_BUFFER` / `READAHEAD_DEPTH` |
| CPU 側 RGBA | 同期処理中の1枚中心 | F-8 で寸法上限以下に限定。枚数上限（in-flight/ready/結果キュー）で有界 |
| 寸法プローブ | UI で `image_dimensions` | UI 経路から撤去。ワーカー内で上限判定（`Oversized`）に使用 |
| 1 枚の重さ | UI をブロック | ワーカーで吸収（UI 非ブロック） |

`current_vram` と `window` の `vram_size` 合計一致という不変条件は維持する
（配置・evict 時のみ増減）。

---

## 10. エラー処理・堅牢性

- デコード失敗・寸法不正は `Failed`、open 不可は `Missing`、寸法上限超過は `Oversized` として UI で集約。
- ワーカー内では `panic` させない（`image::open` の `Result` を握りつぶさず Outcome へ変換）。
- ワーカーが万一全滅しても UI はブロックしない（bounded キューが満杯になれば `try_send` が失敗するが
  `update()` は進む）。
  健全性のため、`inflight` に積んだまま一定時間（例：数 rescan 周期）結果が来ないパスは
  要求を再投入できるよう、`inflight` にタイムスタンプを持たせる案を検討（任意）。

---

## 11. テスト計画

ロジック層（スレッドに依存しない単体テスト）を厚くする。

- `decode_color`：正常画像で dims と画素サイズが一致／破損ファイルで `Err(Failed)`／
  消失ファイルで `Err(Missing)`。
- `cache` 配置ロジック：`ready` に必要パスがある状態で `refill_window` を呼ぶと
  前身 と同じ「窓は `play_order[cursor..]` の連続ミラー」「`current_vram` 一致」を満たす。
  - テストではワーカーを使わず、`ready` を手動で充填して配置だけを検証する
    （`DecoderPool` を注入可能にするか、配置関数を純粋関数として切り出す）。
- 無効化：epoch 不一致、`play_order` に存在しない、遠方のパスの結果を渡すと破棄され、窓に入らない。
- 同一パス差し替え：要求後に同じパスのファイルを置換すると fingerprint 不一致で旧 `Ok` / `Failed` /
  `Oversized` を適用せず、古い巨大判定で現在ファイルを移動しない。
- 優先制御：通常キューを満杯にした状態でも表示要求を高優先キューへ投入でき、ワーカーが通常待ちより先に取得する。
- F-8 退避：寸法上限超過で `Oversized` を返し、移動成功時は対象が `oversized` フォルダへ移動して
  `play_order` から除かれる（`cursor` 不変・`fail_counts` 非加算・`delete` と共通の衝突回避で原本保全）。
- F-8 移動失敗：通常失敗は未表示末尾への再投入で再生継続、同一パスの累計 5 回失敗で `halted`（ループ停止）＋
  ファイル名付きモーダル＋再生停止となる。
- 世代競合：同じパスを旧epochと現epochで要求し、旧 `Ok` / `Failed` が現状態へ適用されない。
- 失敗/消失：`Failed` 3 回で恒久除外、`Missing` はカウンタ非対象（前身 テストを移植）。
- 既存の `delete_by_path` / `rescan` / `quarantine` テストは挙動不変であることを回帰確認。

スレッド統合テストは、プール往復、優先取得、結果キュー滞留中の安全な Drop を対象とする。

---

## 12. 段階的移行

1. **抽出**：`refill_window` の「配置」と「デコード」を分離。デコードを差し替え可能にする
   （trait or 関数注入）。この時点では同期デコードのままで挙動不変・テスト緑を維持。
2. **ワーカー導入**：`decoder.rs` と `CacheState` 拡張（`ready`/`inflight`）を追加。
   表示枠は高優先キューへ要求し、配置は到着後に変更。
3. **UI 寸法プローブ撤去**：UI 経路から外し、ワーカー内の寸法上限判定（`Oversized`）へ移す。
4. **計測**：大画像混在の数千枚フォルダで、advance 時の UI スレッド最長停止時間を計測
   （目標：1 フレーム内デコード起因の停止をゼロに）。

---

## 13. 影響範囲

| ファイル | 変更 |
| --- | --- |
| `main.rs` | ワーカー・キュー・フレーム処理上限・F-8 閾値の定数追加、プール生成 |
| `decoder.rs`（新規） | ワーカープール・チャネル・要求/結果型 |
| `image_loader.rs` | `decode_color`（`ColorImage`+dims）追加。`dimensions` は UI 経路から外す |
| `cache.rs` | `ready`/`ready_bytes`/`inflight`/`epoch` 追加。`refill_window` を「回収→配置→要求」に再構成 |
| `app.rs` | 結果回収の起点、未到着スロットの「読み込み中」表示、無効化トリガでの掃除 |
| `Cargo.toml` | （採用時）`crossbeam-channel` 追加 |

---

## 14. リスクと留保

- **GPU アップロードの集中**：到着が一度に重なると `load_texture` が同フレームに集中し得る。
  `MAX_UPLOADS_PER_FRAME` で平準化する。
- **読み先行の無駄打ち**：シャッフル直後は先行デコードが捨てられる可能性。`READAHEAD_DEPTH` を
  小さめに始め、計測で調整する。
- **メモリ**：F-8 でデコード対象を寸法上限以下に限定するため、`ready`・結果キュー・ワーカー処理中の
  CPU 側 RGBA は「枚数上限 × 上限サイズ」で有界。巨大画像はそもそもデコードせず退避する。
- **egui のスレッド安全性前提**：`Context::request_repaint` / `load_texture` の呼び出し規約は
  使用する egui バージョン（現状 0.31）で必ず確認すること（CLAUDE.md の方針：自分の記憶を
  過信せず一次情報で裏取りする）。

---

## 15. 未決事項（実装前に決める）

- ワーカー数 K の既定値と上限。
- `READAHEAD_DEPTH` / 各キュー上限 / F-8 寸法閾値の最終値（本書の初期値から計測で詰める）。

---

## 改訂履歴

- 2026-06-24 初版。
- 2026-06-24 v1.0 横断設計へ統合。Undo 廃止・表示枚数可変・F-4 共通遷移、
  二段優先キュー、CPU RAM バイト予算、表示枠の VRAM 優先、フレーム処理上限を反映。
- 2026-06-24 再レビュー反映。単一FIFO・`ready_bytes` のみのゲート・パス単独識別案を撤回し、
  表示用予約キュー、RAII RAM予約、epoch必須を本文全体の採用仕様として統一。
- 2026-06-24 F-8 反映に伴い本文を正常化。CPU RAM バイト予約（`DecodeBudget`/`DecodeReservation`/`Deferred`/
  巨大表示例外）を本文から除去し、寸法上限＋枚数上限で有界化。`Oversized` を追加。表示優先キューと epoch は維持。
- 2026-06-24 `Oversized` 移動失敗時の遷移を追加。通常失敗は非モーダル通知後に未表示末尾へ戻し、
  最終候補時のみ最大 5 回失敗後にダイアログ表示・再生停止とした。衝突回避は `delete` と共通化。
- 2026-06-24 同一パスの外部差し替え対策として `FileFingerprint` 照合と
  `Oversized` 移動直前の寸法再判定を追加。
