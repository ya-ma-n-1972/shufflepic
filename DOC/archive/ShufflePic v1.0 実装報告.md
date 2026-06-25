# ShufflePic v1.0 実装報告

| 項目 | 内容 |
| --- | --- |
| 種別 | 実装報告（v1.0） |
| 対象読者 | 開発者 / レビュア |
| 実装先 | `D:\Library\programming\Rust\ShufflePic\v1.0`（前身 を雛形に作成） |
| 状態 | 実装・自動テスト完了。**GUI 実機起動による目視確認は未実施**（§6） |

---

## 0. 概要

- v1.0 要求定義書（F-1〜F-8）・同 詳細設計書に基づき、前身 を基盤として再実装した。
- 結果: **debug / release 両ビルド成功、`cargo test` 13 件成功、`cargo clippy --all-targets -- -D warnings` 警告ゼロ**。
- 主な変更: デコード非同期化（F-1）／メニュー中の自動送り凍結（F-2）／Undo 廃止（F-3）／末尾到達のサイクル
  停止修復（F-4）／「表示済み」表示（F-5）／右クリック再生・一時停止（F-6）／管理画面＋永続化（F-7）／
  巨大画像の自動退避（F-8）。

---

## 1. 成果物

```
v1.0/
├── Cargo.toml
└── src/
    ├── main.rs        # 定数、起動（CLI 廃止・永続化から復元）、フォント、プール生成
    ├── app.rs         # eframe::App、update ループ、描画、各機能の適用
    ├── playback.rs    # PlaybackState（last_shown / empty_state_active 追加）、シャッフル
    ├── cache.rs       # CacheState（ready/inflight/ready_bytes/epoch）、refill_window、settle
    ├── decoder.rs     # 【新規】ワーカープール（二段優先キュー＋epoch）
    ├── image_loader.rs# decode_color/ColorImage、寸法判定、巨大画像判定、fingerprint
    ├── quarantine.rs  # delete/oversized 共通の衝突回避移動（hard_link 優先）
    ├── scanner.rs     # 起動時/定期 rescan（display_count 連動）
    ├── settings.rs    # 【新規】設定値＋管理画面 UI（rfd フォルダ選択）
    └── persist.rs     # 【新規】設定＋サイクル状態のディスク保存/復元・突き合わせ
```

### 依存（Cargo.toml）

```toml
eframe = "0.31"
egui = "0.31"
image = "0.25"
rand = "0.8"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rfd = "0.15"
crossbeam-channel = "0.5"
```

---

## 2. ビルド・静的検証結果

| 項目 | 結果 |
| --- | --- |
| `cargo build`（debug） | ✅ 成功 |
| `cargo build --release` | ✅ 成功 |
| `cargo clippy --all-targets -- -D warnings` | ✅ 警告ゼロ |
| `cargo test` | ✅ 13 件成功 |

---

## 3. 設計との対応（機能別）

| 機能 | 実装の要点 | 主なモジュール |
| --- | --- | --- |
| **F-1 デコード非同期化** | `decoder.rs` にワーカープール。表示用高優先／先読み通常の **2 つの bounded キュー**を `select_biased!` で優先受信。`epoch` をワーカーへ共有しデコード前に古い要求を破棄。`refill_window` は「回収→配置→要求」。`load_texture` のみ UI スレッド。`Drop` は送信側/受信側を切断後に join（デッドロック回避）。 | `decoder.rs` / `cache.rs` / `image_loader.rs` |
| **F-2 メニュー中の自動送り凍結** | `freeze_active = menu_open │ settings.show_panel │ blocked`。凍結開始で残り時間を保存、終了で `last_advance` を再設定して残り時間から再開。開閉遷移フレームは送らない（同一フレーム抑止）。 | `app.rs` |
| **F-3 Undo 廃止** | `Ctrl+Z`・「↶ Undo」・`DeleteRecord`・`restore` を撤去。`QuarantineState` も廃止し移動ヘルパに一本化。 | `app.rs` / `quarantine.rs` |
| **F-4 サイクル停止修復** | `settle_cycle_or_empty` を毎フレーム終端で評価。`cursor >= len` かつ窓空ならサイクル境界／空状態へ。空状態は `empty_state_active` で 1 回だけ初期化。 | `cache.rs` / `app.rs` |
| **F-5 表示済み枚数** | 上部バーに `表示済み: {cursor}`。表示済み＋未表示＝総数。 | `app.rs` |
| **F-6 右クリック再生/一時停止** | 削除メニューに状態反映の「再生／一時停止」。停止→再生は間隔を計り直し。 | `app.rs` |
| **F-7 管理画面＋永続化** | `settings.rs`（rfd フォルダ選択・間隔 5〜60・枚数 1/2、即時反映）。`persist.rs`（JSON を temp→`sync_all`→`rename` で保存、節流＋`on_exit`）。起動時に復元し直下スキャンと突き合わせ（フォルダ外/サブフォルダ/`delete`/非対応/重複/消失を除外、新規は未表示へ、`cursor` 再計算）。読み込んだ保存先を以後も使用。フォルダ変更で全リセット。CLI 入力廃止。 | `settings.rs` / `persist.rs` / `app.rs` / `main.rs` |
| **F-8 巨大画像の自動退避** | ワーカーが寸法上限超過で `Oversized` 返却→`refill` が `play_order` から除去→app が `oversized` フォルダへ移動。移動直前に寸法を再確認（外部差し替え対策）。失敗は未表示末尾へ再投入、最後の候補で最大 5 回、全失敗で**ループ処理ごと停止（`halted`）＋ファイル名付きモーダル**。 | `image_loader.rs` / `cache.rs` / `app.rs` |

---

## 4. レビュー反映履歴

### 4.1 1 次レビュー反映
- 表示優先・RAM 上限・世代識別の要件化（後に F-8 で RAM はバイト予約不要へ簡素化）。
- 復元データのパス検証、F-2/F-6 の凍結競合規則、永続化の安全な保存・保存先フォールバック。

### 4.2 実装 1 次レビュー反映
- 1 枚表示時の rescan が 2 枚目を保護していた問題 → `display_count` を rescan へ伝搬。
- 旧 epoch 結果が現 epoch の `inflight` を解除する問題 → epoch チェックを先に行い、現世代のみ解除。
- `ready_bytes` 二重加算 → 同一パス再格納時に旧分を差し引く。
- 読み込んだ保存先の引き継ぎ → `load()` がパスも返し `save_path` を初期化。

### 4.3 実装 2 次レビュー反映
- 衝突回避移動の TOCTOU → **`hard_link`＋原本削除**（既存なら AlreadyExists で上書きしない）。
- 退避失敗ダイアログを**擬似モーダル化**（入力・操作・advance を `blocked` で全抑止、前面表示）。
- デコード失敗・Missing・Oversized 由来の候補列変更を**永続化 dirty 化**（`refill_window` が変更有無を返す）。
- **F-8 最終失敗の確定**: 5 回失敗で**スライドショーのループ処理ごと停止（`halted`）**。補充・送り・退避処理・
  境界判定をすべて止めるため、対象を `play_order` に残してもストール／再要求ループは起きない。モーダルに
  **具体的なファイル名**を表示。復帰はフォルダ再選択／再起動（`halted` は永続化しない）。

### 4.4 実装 3 次レビュー反映（ShufflePic v1.0）
- **#1 `halted` からの復帰**: `change_folder` は `halted` 中なら同一フォルダ判定でスキップせず全リセットする
  （ダイアログの「フォルダを選び直す」で同じフォルダを選んでも復帰可能に）。
- **#3 非対応FS フォールバック**: 予約後の `rename` 置換をやめ、**`create_new` で予約したファイルへ内容を
  コピー → `sync_all` → 原本削除**へ変更（FAT/exFAT 等でも上書き競合を生まない）。
- **#4 擬似モーダルの徹底**: 退避モーダル中（`blocked`）は設定パネルを描画しない（間隔/枚数/フォルダ操作を遮断）。
- **#5 `halted` 中の rescan 停止**: rescan も `halted` 判定の対象に含め、停止中は `play_order`/`window` を変えない。
- **#2 F-8 文書統一**: 「最後の未表示候補で 5 回」表記を、実装どおり「**対象パス単位の累計 5 回失敗で `halted`**」へ
  各文書で統一。状態の「未実装」表記・残存 `v3` 表記を v1.0 へ更新。

---

## 5. テスト（`cargo test`：13 件）

| モジュール | テスト | 検証内容 |
| --- | --- | --- |
| playback | `new_preserves_set` | シャッフルで集合保存 |
| playback | `reshuffle_tail_keeps_head_and_set` | 窓より後ろのみシャッフル |
| playback | `reshuffle_all_avoiding_keeps_set_and_avoids_front` | 直前表示を新サイクル先頭に来させない |
| playback | `reshuffle_all_avoiding_small_set_is_tolerated` | n≤2 でパニックしない |
| quarantine | `move_basic` | 移動で原本が移り複製される |
| quarantine | `move_collision_does_not_overwrite` | 同名は `name (1).ext` で退避、上書きしない |
| cache | `settle_cycle_boundary_on_tail` | 末尾到達でサイクル境界（cursor=0・cycle++） |
| cache | `settle_empty_runs_once` | 空状態の初期化は遷移時 1 回のみ |
| cache | `refill_fills_window_async_contiguously` | 実ワーカーで窓が連続ミラーとして充填 |
| cache | `refill_detects_oversized` | 寸法上限超過を検出し `play_order` から除去・報告 |
| scanner | `rescan_one_image_mode_removes_vanished_second` | 1 枚表示で 2 枚目の外部消失を除去（#3 回帰） |
| persist | `reconcile_filters_dedups_and_recomputes_cursor` | 復元時のフォルダ外/重複除外・cursor 再計算 |
| persist | `save_then_read_round_trip` | 保存→読込で同値、一時ファイルが残らない |

---

## 6. 未実施・既知の制約

- **GUI 実機動作は開発者が実機で確認済み**（起動・フォルダ選択・スライドショー・削除/退避・全画面・設定・
  永続化復元）。非同期デコード→テクスチャ化の往復は単体テスト（実ワーカー＋`load_texture`）でも担保。
  ※ 自動化された UI テストは未整備（下記）。アプリアイコンは exe 埋め込み（`build.rs`＋`winresource`）と
  ウィンドウアイコン（`with_icon`）を設定済み。
- 自動テスト未追加: 優先キュー満杯時、`DecoderPool` 終了デッドロックの専用ストレス、F-2 タイミング、
  アプリ経路での「最後の 1 枚削除」、表示枚数 2→1 のカウント維持、F-8 の累計 5 回失敗→`halted` フロー、
  `halted` 中の rescan/refill 停止と同一フォルダ復帰、`persist::load()` の保存先選択。
- **F-1 のチューニング定数は初期値**（§7）。実測調整は未実施。

---

## 7. チューニング定数（初期値・要実測調整）

実装はハードコードの初期値。`main.rs` に集約。実機・実データでの計測後に調整する想定（詳細設計 §10）。

```rust
DECODE_WORKERS = 0          // 0=自動（コア数-1 を 1..=4 にクランプ）
MAX_INFLIGHT = 6            // 同時に出してよいデコード要求の上限
MAX_READY_BUFFER = 6        // デコード済み・未配置の保持上限（枚数）
READAHEAD_DEPTH = 8         // cursor から先読み要求を出す最大相対距離
MAX_RESULTS_PER_FRAME = 4   // 1 フレームで回収する結果数の上限
MAX_UPLOADS_PER_FRAME = 2   // 1 フレームの GPU アップロード上限（表示枠は対象外）
DISPLAY_QUEUE_CAPACITY = 2  // 表示用高優先キュー容量
PREFETCH_QUEUE_CAPACITY = 6 // 先読み通常キュー容量
RESULT_QUEUE_CAPACITY = 8   // 結果キュー容量
OVERSIZED_MAX_PIXELS = 32_000_000  // これを超える幅×高さは退避（≒ デコード後 128MB）
OVERSIZED_MAX_SIDE = 10_000        // 長辺がこれを超えても退避
MAX_OVERSIZED_MOVE_ATTEMPTS = 5    // 退避失敗の最大試行回数（超過で halted）
VRAM_LIMIT = 2 GiB                  // 窓の推定 VRAM 上限（v2 から据え置き）
```

各値の意味・トレードオフ・調整指針は本書 §8 を参照。

---

## 8. チューニング定数の詳細（保留事項 #4）

これらは「動く初期値」であり、最適値は **実機（CPU コア数・GPU・ディスク速度）と実データ（画像サイズ・枚数）**
に依存するため、計測後の調整を保留している。**RAM のおおよその上限**は次で見積もれる。

```
peak RAM(概算) ≒ (MAX_INFLIGHT + RESULT_QUEUE_CAPACITY + MAX_READY_BUFFER) × （上限サイズ）
              = (6 + 8 + 6) × 約128MB ≒ 2.5GB（全てが上限寸前に重なる最悪値・通常は遥かに小さい）
```

| 定数 | 役割 | 大きすぎると | 小さすぎると | 調整指針 |
| --- | --- | --- | --- | --- |
| `DECODE_WORKERS` | デコードスレッド数 | UI を圧迫・RAM 増 | 表示/先読みが遅い | 既定（コア-1, 1..=4）で可。CPU 余力と滑らかさで調整 |
| `MAX_INFLIGHT` | 同時要求上限 | RAM 増・シャッフル時の無駄打ち増 | 先読みが追いつかない | RAM 上限と先読みの先行度で調整 |
| `MAX_READY_BUFFER` | 未配置保持枚数 | RAM 増 | 送り直後にカクつく | RAM 上限の主要レバー（枚数×上限サイズ） |
| `READAHEAD_DEPTH` | 先読み距離 | reshuffle/削除での破棄が増 | バッファ不足 | 小さめから始め計測で増やす |
| `MAX_RESULTS_PER_FRAME` | 1F の回収数 | （安価なので影響小） | 結果キューが滞留・遅延 | 4 で概ね十分 |
| `MAX_UPLOADS_PER_FRAME` | 1F の GPU 配置 | 配置集中でヒッチ | 先読み配置が遅い | 表示枠は対象外なので表示は遅れない |
| `DISPLAY_QUEUE_CAPACITY` | 表示要求キュー | （RAM 微増） | `try_send` 失敗が増（次F再投入） | 表示枚数（最大2）に合わせ 2 |
| `RESULT_QUEUE_CAPACITY` | 結果キュー | **RAM 増**（各枠が画像を保持し得る） | 滞留でワーカー待ち | 回収レートとの兼ね合い |
| `OVERSIZED_MAX_PIXELS` / `_SIDE` | 退避閾値 | 巨大画像が残り RAM/時間が増 | 正常な大判写真まで退避 | **RAM 上限の最大レバー**。利用者の画像傾向で決める |
| `MAX_OVERSIZED_MOVE_ATTEMPTS` | 退避失敗の停止しきい値 | 失敗時に粘りすぎ | すぐ停止 | 5 程度で可 |
| `VRAM_LIMIT` | 窓の VRAM 上限 | VRAM 逼迫 | 先読み浅い | GPU VRAM に合わせる（既定 2GiB） |

**計測のしかた（実機テスト時）**: 数千枚・サイズ混在のフォルダで、(1) advance/削除/境界直後の **UI 最長停止時間**
（デコード起因のフリーズが出ないこと）、(2) **ピーク RAM**、(3) 送り直後に表示が滞らないか、(4) 正常な写真が
誤って退避されないか、を観測し、上表の指針で `MAX_INFLIGHT` / `MAX_READY_BUFFER` / 閾値などを調整する。

---

## 改訂履歴

- 2026-06-24 初版作成（v1.0 実装・自動テスト完了時点。GUI 実機確認は未実施）。
