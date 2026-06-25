//! eframe::App 本体：update ループ・advance・delete・描画・各機能統合
//! （v1.0 詳細 §4.1〜§4.8 / §5）。Undo は廃止（F-3）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::cache::{self, CacheState, Settle};
use crate::decoder::DecoderPool;
use crate::image_loader;
use crate::persist::{self, PersistState};
use crate::playback::PlaybackState;
use crate::quarantine;
use crate::scanner::{self, ScannerState};
use crate::settings::{self, SettingsState};
use crate::{
    DELETE_DIR_NAME, ERROR_MSG_SECS, MAX_OVERSIZED_MOVE_ATTEMPTS, OVERSIZED_DIR_NAME,
    PERSIST_SCHEMA_VERSION, RESCAN_INTERVAL_SECS, SAVE_THROTTLE_SECS,
};

pub struct ShufflePicApp {
    pub playback: PlaybackState,
    pub cache: CacheState,
    pub scanner: ScannerState,
    pub settings: SettingsState,
    pub decoder: DecoderPool,

    delete_dir: Option<PathBuf>,
    oversized_dir: Option<PathBuf>,

    playing: bool,
    last_advance: Instant,

    fullscreen: bool,
    menu_open: bool,
    menu_target: Option<PathBuf>,
    error: Option<(String, Instant)>,

    // F-2 凍結
    advance_frozen_remaining: Option<Duration>,
    advance_freeze_active: bool,

    // F-7 永続化
    state_dirty: bool,
    last_saved: Instant,
    save_path: Option<PathBuf>,

    // F-8 退避失敗の試行回数
    oversized_attempts: HashMap<PathBuf, u32>,
    /// F-8 退避が最大回数失敗した時のモーダル通知メッセージ（OK で解除）。
    oversized_blocked_msg: Option<String>,
    /// F-8 退避失敗でスライドショーのループ処理ごと停止した状態（フォルダ再選択／再起動で解除）。
    halted: bool,
}

impl ShufflePicApp {
    /// フォルダ未選択（管理画面から選ばせる）状態で起動。
    pub fn new(
        settings: SettingsState,
        playback: PlaybackState,
        decoder: DecoderPool,
        save_path: Option<PathBuf>,
    ) -> Self {
        let (delete_dir, oversized_dir) = match &settings.folder {
            Some(f) => (Some(f.join(DELETE_DIR_NAME)), Some(f.join(OVERSIZED_DIR_NAME))),
            None => (None, None),
        };
        Self {
            playback,
            cache: CacheState::new(),
            scanner: ScannerState::new(Duration::from_secs(RESCAN_INTERVAL_SECS)),
            settings,
            decoder,
            delete_dir,
            oversized_dir,
            playing: true,
            last_advance: Instant::now(),
            fullscreen: false,
            menu_open: false,
            menu_target: None,
            error: None,
            advance_frozen_remaining: None,
            advance_freeze_active: false,
            state_dirty: false,
            last_saved: Instant::now(),
            save_path,
            oversized_attempts: HashMap::new(),
            oversized_blocked_msg: None,
            halted: false,
        }
    }

    fn set_error(&mut self, msg: String) {
        self.error = Some((msg, Instant::now() + Duration::from_secs(ERROR_MSG_SECS)));
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(self.settings.interval_secs)
    }

    /// 次の画像へ（境界判定は settle に委ねる・F-4）。
    fn advance(&mut self) {
        let dc = self.settings.display_count;
        let shown = self.cache.window.len().min(dc);
        if shown == 0 || self.playback.play_order.is_empty() {
            return;
        }
        let mut last_shown = Vec::with_capacity(shown);
        for _ in 0..shown {
            if let Some(p) = self.cache.evict_front() {
                last_shown.push(p);
            }
        }
        self.playback.last_shown = last_shown;
        self.cache.preload_blocked_by_vram = false;
        self.playback.cursor += shown;
    }

    /// 削除（隔離移動）。Undo は無い（F-3）。
    fn do_delete_path(&mut self, target: PathBuf) {
        let dir = match &self.delete_dir {
            Some(d) => d.clone(),
            None => return,
        };
        if let Some(folder) = self.settings.folder.clone() {
            if let Err(e) = quarantine::prepare_dir(&folder, DELETE_DIR_NAME) {
                self.set_error(e);
                return;
            }
        }
        match self.cache.window.iter().position(|c| c.path == target) {
            Some(i) => {
                let path = self.cache.window[i].path.clone();
                match quarantine::move_with_collision_avoidance(&path, &dir) {
                    Ok(_) => {
                        self.cache.evict_at(i);
                        if let Some(pos) =
                            self.playback.play_order.iter().position(|p| *p == path)
                        {
                            self.playback.play_order.remove(pos);
                        }
                        self.cache.preload_blocked_by_vram = false;
                        self.cache.bump_epoch(&self.decoder);
                        self.state_dirty = true;
                    }
                    Err(e) => self.set_error(format!("削除に失敗しました: {e}")),
                }
            }
            None => self.set_error("対象画像が切り替わったため削除をキャンセルしました".to_string()),
        }
    }

    /// F-8: 巨大画像の退避処理（refill が検出して除去済みのパス群を処理）。
    fn handle_oversized(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let dir = match &self.oversized_dir {
            Some(d) => d.clone(),
            None => return,
        };
        let mut changed = false;
        for path in paths {
            // 退避直前の再確認：外部差し替えで現在は上限以下なら退避せず通常候補へ戻す。
            match image_loader::dimensions(&path) {
                Some((w, h)) if !image_loader::is_oversized(w, h) => {
                    self.playback.play_order.push(path);
                    changed = true;
                    continue;
                }
                None => continue, // 消失 → そのまま除去済み
                _ => {}
            }
            if let Err(e) = quarantine::prepare_dir(
                self.settings
                    .folder
                    .as_deref()
                    .unwrap_or(std::path::Path::new(".")),
                OVERSIZED_DIR_NAME,
            ) {
                self.set_error(e);
            }
            match quarantine::move_with_collision_avoidance(&path, &dir) {
                Ok(_) => {
                    self.oversized_attempts.remove(&path);
                }
                Err(e) => {
                    let count = {
                        let n = self.oversized_attempts.entry(path.clone()).or_insert(0);
                        *n += 1;
                        *n
                    };
                    if count >= MAX_OVERSIZED_MOVE_ATTEMPTS {
                        // 最終段階：スライドショーのループ処理ごと停止する。
                        // 残りの未表示も表示不能な可能性が高いため、止めて利用者の手動処理に委ねる。
                        let fname = path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string());
                        self.oversized_blocked_msg = Some(format!(
                            "退避できない画像があるため、スライドショーを停止しました。\n\n\
                             ファイル名: {fname}\n\
                             場所: {}\n\
                             原因: {e}\n\n\
                             未表示の残りも表示できない画像の可能性が高いため停止します。\n\
                             この画像（および他の不適切な画像）を手動で処理してから、\n\
                             「⚙ 設定」でフォルダを選び直すか、アプリを再起動してください。",
                            path.display(),
                        ));
                        self.playing = false;
                        self.halted = true;
                        self.oversized_attempts.remove(&path);
                        // 仕様どおりファイルは残す（原本はディスク上、play_order にも戻す）。
                        // ループ処理を停止するため、残しても詰まり（ストール）は起きない。
                        self.playback.play_order.push(path);
                        changed = true;
                        break;
                    } else {
                        self.playback.play_order.push(path);
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.cache.bump_epoch(&self.decoder);
            self.state_dirty = true;
        }
    }

    /// フォルダ変更＝全リセット（F-7 §4.7.3）。
    fn change_folder(&mut self, new_dir: PathBuf) {
        let new_canon = std::fs::canonicalize(&new_dir).unwrap_or(new_dir.clone());
        // halted 中は同一フォルダの再選択でも全リセットして復帰する（同一判定でスキップしない）。
        if !self.halted {
            if let Some(cur) = &self.settings.folder {
                let cur_canon = std::fs::canonicalize(cur).unwrap_or(cur.clone());
                if cur_canon == new_canon {
                    return; // 同一フォルダ（表記差）→ リセットしない
                }
            }
        }
        if !new_dir.is_dir() {
            self.set_error("指定がフォルダではありません".to_string());
            return;
        }
        let delete_dir = match quarantine::prepare_dir(&new_dir, DELETE_DIR_NAME) {
            Ok(d) => d,
            Err(e) => {
                self.set_error(e);
                return;
            }
        };
        let oversized_dir = match quarantine::prepare_dir(&new_dir, OVERSIZED_DIR_NAME) {
            Ok(d) => d,
            Err(e) => {
                self.set_error(e);
                return;
            }
        };
        // サイクル状態を全クリア。
        while self.cache.evict_front().is_some() {}
        self.cache.current_vram = 0;
        self.cache.preload_blocked_by_vram = false;
        self.cache.bump_epoch(&self.decoder); // ready/inflight クリア＋世代更新
        self.oversized_attempts.clear();

        let paths = image_loader::scan_dir(&new_dir);
        self.playback = PlaybackState::new(paths);

        self.settings.folder = Some(new_dir);
        self.delete_dir = Some(delete_dir);
        self.oversized_dir = Some(oversized_dir);
        self.halted = false; // フォルダ再選択で停止状態を解除（復帰）。
        self.oversized_blocked_msg = None;
        self.playing = true;

        self.state_dirty = true;
        self.save_now();
    }

    fn persist_state(&self) -> Option<PersistState> {
        let folder = self.settings.folder.clone()?;
        Some(PersistState {
            schema_version: PERSIST_SCHEMA_VERSION,
            folder,
            interval_secs: self.settings.interval_secs,
            display_count: self.settings.display_count,
            play_order: self.playback.play_order.clone(),
            cursor: self.playback.cursor,
            cycle_count: self.playback.cycle_count,
            last_shown: self.playback.last_shown.clone(),
        })
    }

    fn save_now(&mut self) {
        if let Some(state) = self.persist_state() {
            match persist::save(&mut self.save_path, &state) {
                Ok(()) => {
                    self.state_dirty = false;
                    self.last_saved = Instant::now();
                }
                Err(e) => self.set_error(format!("状態の保存に失敗しました: {e}")),
            }
        }
    }
}

impl eframe::App for ShufflePicApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. 前フレームの pending_free を drop。
        self.cache.pending_free.clear();

        let menu_open_prev = self.menu_open;

        // rescan タイマー。halted 中はループ処理ごと停止のため rescan も止める。
        if !self.halted
            && self.settings.folder.is_some()
            && self.scanner.last_rescan.elapsed() >= self.scanner.interval
        {
            let folder = self.settings.folder.clone().unwrap();
            let changed = scanner::rescan(
                &mut self.playback,
                &mut self.cache,
                &folder,
                self.settings.display_count,
            );
            self.scanner.last_rescan = Instant::now();
            if changed {
                self.cache.bump_epoch(&self.decoder);
                self.state_dirty = true;
            }
        }

        // 回収→配置→要求（F-1）。
        let dc = self.settings.display_count;
        let mut oversized_out: Vec<PathBuf> = Vec::new();
        if !self.halted
            && self.settings.folder.is_some()
            && cache::refill_window(
                &mut self.playback,
                &mut self.cache,
                &self.decoder,
                ctx,
                dc,
                &mut oversized_out,
            )
        {
            self.state_dirty = true;
        }
        if !self.halted {
            self.handle_oversized(oversized_out);
        }

        // F-8 退避失敗の擬似モーダル中は全操作を抑止する。
        let blocked = self.oversized_blocked_msg.is_some();

        // 入力（メニュー/設定/モーダル中は Space/F11 抑制。Ctrl+Z は廃止）。
        let mut want_play = false;
        let mut want_fs = false;
        if !menu_open_prev && !self.settings.show_panel && !blocked {
            ctx.input(|i| {
                if i.key_pressed(egui::Key::Space) {
                    want_play = true;
                }
                if i.key_pressed(egui::Key::F11) {
                    want_fs = true;
                }
            });
        }

        let mut menu_open_now = false;
        let mut want_delete: Option<PathBuf> = None;

        // 設定パネル（F-7）。退避モーダル中（blocked）はパネルを描画せず操作を遮断する。
        let panel = if blocked {
            settings::SettingsPanelResult {
                folder_chosen: None,
                changed: false,
            }
        } else {
            settings::draw_panel(&mut self.settings, ctx)
        };
        let folder_chosen = panel.folder_chosen;
        if panel.changed {
            self.state_dirty = true;
        }

        // F-8 退避失敗のモーダル通知（OK で解除）。前面に出し、他操作は blocked で抑止する。
        if let Some(msg) = self.oversized_blocked_msg.clone() {
            let mut clear = false;
            egui::Window::new("退避エラー")
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(msg);
                    if ui.button("OK").clicked() {
                        clear = true;
                    }
                });
            if clear {
                self.oversized_blocked_msg = None;
            }
        }

        // 上部バー（F-5: 表示済み、F-7: 設定ボタン）。
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let label = if self.playing { "⏸ 停止" } else { "▶ 再生" };
                if ui.button(label).clicked() && !menu_open_prev && !blocked {
                    want_play = true;
                }
                if ui.button("⚙ 設定").clicked() && !blocked {
                    self.settings.show_panel = !self.settings.show_panel;
                }
                ui.separator();
                let total = self.playback.play_order.len();
                let shown = self.playback.cursor.min(total);
                let remaining = total.saturating_sub(self.playback.cursor);
                ui.label(format!("表示済み: {shown}"));
                ui.label(format!("未表示: {remaining}"));
                ui.label(format!("キャッシュ: {}", self.cache.window.len()));
                ui.label(format!("サイクル: {}", self.playback.cycle_count));
                if let Some((msg, _)) = &self.error {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(230, 90, 90), msg);
                }
            });
        });

        // 中央（画像描画・メニュー検出）。
        egui::CentralPanel::default().show(ctx, |ui| {
            let full = ui.max_rect();
            let bg = ui.interact(full, egui::Id::new("bg_area"), egui::Sense::click());

            let n = self.cache.window.len().min(dc);
            if n == 0 {
                let msg = if self.settings.folder.is_none() {
                    "設定からフォルダを選択してください"
                } else if self.playback.play_order.is_empty() {
                    "画像がありません（空フォルダ状態）"
                } else {
                    "読み込み中..."
                };
                ui.painter().text(
                    full.center(),
                    egui::Align2::CENTER_CENTER,
                    msg,
                    egui::FontId::proportional(20.0),
                    egui::Color32::GRAY,
                );
                return;
            }

            let gap = 8.0;
            let h = full.height();
            let mut widths: Vec<f32> = (0..n)
                .map(|i| {
                    let c = &self.cache.window[i];
                    if c.height > 0.0 {
                        h * (c.width / c.height)
                    } else {
                        0.0
                    }
                })
                .collect();
            let total: f32 = widths.iter().sum::<f32>() + gap * ((n - 1) as f32);
            let scale = if total > full.width() && total > 0.0 {
                full.width() / total
            } else {
                1.0
            };
            let draw_h = h * scale;
            for w in widths.iter_mut() {
                *w *= scale;
            }
            let total_scaled: f32 = widths.iter().sum::<f32>() + gap * ((n - 1) as f32);
            let mut x = full.left() + (full.width() - total_scaled) / 2.0;
            let top = full.top() + (full.height() - draw_h) / 2.0;

            let mut img_rects: Vec<egui::Rect> = Vec::with_capacity(n);
            for (i, &w) in widths.iter().enumerate() {
                let rect = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(w, draw_h));
                let image_path = self.cache.window[i].path.clone();
                let tex_id = self.cache.window[i].texture.id();
                ui.painter().image(
                    tex_id,
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                let resp = ui.interact(rect, egui::Id::new("img").with(i), egui::Sense::click());
                if resp.secondary_clicked() {
                    self.menu_target = Some(image_path.clone());
                }
                if !menu_open_prev && resp.double_clicked() {
                    want_fs = true;
                }
                let delete_target = self
                    .menu_target
                    .clone()
                    .unwrap_or_else(|| image_path.clone());
                let playing = self.playing;
                let menu = resp.context_menu(|ui| {
                    // メニューを一回り大きく（フォント・余白・最小幅を拡大）。
                    ui.spacing_mut().button_padding = egui::vec2(14.0, 8.0);
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
                    ui.set_min_width(190.0);
                    let sz = 18.0;
                    let plabel = if playing { "⏸ 一時停止" } else { "▶ 再生" };
                    if ui.button(egui::RichText::new(plabel).size(sz)).clicked() {
                        want_play = true;
                        ui.close_menu();
                    }
                    if ui.button(egui::RichText::new("削除").size(sz)).clicked() {
                        want_delete = Some(delete_target.clone());
                        ui.close_menu();
                    }
                    if ui.button(egui::RichText::new("キャンセル").size(sz)).clicked() {
                        ui.close_menu();
                    }
                });
                if menu.is_some() {
                    menu_open_now = true;
                }
                img_rects.push(rect);
                x += w + gap;
            }

            if !menu_open_prev && bg.double_clicked() {
                let over_img = bg
                    .interact_pointer_pos()
                    .map(|p| img_rects.iter().any(|r| r.contains(p)))
                    .unwrap_or(false);
                if !over_img {
                    want_fs = true;
                }
            }
        });

        if !menu_open_now && want_delete.is_none() {
            self.menu_target = None;
        }
        self.menu_open = menu_open_now;

        // F-2/F-7 凍結遷移（モーダル中も advance を凍結）。
        let freeze_active_now = menu_open_now || self.settings.show_panel || blocked;
        let interval = self.interval();
        let mut advance_suppressed = false;
        if !self.advance_freeze_active && freeze_active_now {
            let remaining = interval
                .checked_sub(self.last_advance.elapsed())
                .unwrap_or(Duration::ZERO);
            self.advance_frozen_remaining = Some(remaining);
            advance_suppressed = true;
        } else if self.advance_freeze_active && !freeze_active_now {
            let remaining = self
                .advance_frozen_remaining
                .take()
                .unwrap_or(interval)
                .min(interval);
            let passed = interval.checked_sub(remaining).unwrap_or(Duration::ZERO);
            self.last_advance = Instant::now().checked_sub(passed).unwrap_or_else(Instant::now);
            advance_suppressed = true;
        }
        self.advance_freeze_active = freeze_active_now;

        // 描画後の操作適用（モーダル中は OK 以外の操作を受け付けない）。
        let mut acted = false;
        if !blocked {
            if want_fs {
                self.fullscreen = !self.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
                ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(!self.fullscreen));
                acted = true;
            }
            if want_play {
                self.playing = !self.playing;
                if self.playing {
                    // F-6: 停止→再生は設定間隔を全て計り直す（F-2 の残り時間を破棄）。
                    self.last_advance = Instant::now();
                    self.advance_frozen_remaining = None;
                }
                acted = true;
            }
            if let Some(path) = want_delete {
                self.do_delete_path(path);
                self.menu_target = None;
                acted = true;
            }
            if let Some(dir) = folder_chosen {
                self.change_folder(dir);
                acted = true;
            }
        }

        // advance 判定（F-2: 凍結中・遷移フレームは送らない）。
        if self.playing
            && !self.halted
            && !freeze_active_now
            && !advance_suppressed
            && !self.playback.play_order.is_empty()
            && self.last_advance.elapsed() >= interval
        {
            self.advance();
            self.last_advance = Instant::now();
            self.state_dirty = true;
            acted = true;
        }

        // advance / 削除後の配置。
        if !self.halted && self.settings.folder.is_some() {
            let mut ov2: Vec<PathBuf> = Vec::new();
            if cache::refill_window(
                &mut self.playback,
                &mut self.cache,
                &self.decoder,
                ctx,
                dc,
                &mut ov2,
            ) {
                self.state_dirty = true;
            }
            self.handle_oversized(ov2);
        }

        // F-4 共通遷移（フレーム終端で1回）。停止中は遷移させない。
        let settle = if self.halted {
            Settle::None
        } else {
            cache::settle_cycle_or_empty(&mut self.playback, &mut self.cache, &self.decoder)
        };
        match settle {
            Settle::None => {}
            Settle::CycleBoundary => {
                self.state_dirty = true;
                acted = true;
            }
            Settle::Empty => {
                acted = true;
            }
        }

        // エラー失効。
        if let Some((_, exp)) = &self.error {
            if Instant::now() >= *exp {
                self.error = None;
            }
        }

        // 永続化（節流）。
        if self.state_dirty
            && self.settings.folder.is_some()
            && self.last_saved.elapsed() >= Duration::from_secs(SAVE_THROTTLE_SECS)
        {
            self.save_now();
        }

        // 再描画スケジュール。
        let mut wait = Duration::from_secs(1);
        if self.playing && !freeze_active_now && !self.playback.play_order.is_empty() {
            let rem = interval
                .checked_sub(self.last_advance.elapsed())
                .unwrap_or(Duration::ZERO);
            wait = wait.min(rem);
        }
        if self.settings.folder.is_some() {
            let rescan_rem = self
                .scanner
                .interval
                .checked_sub(self.scanner.last_rescan.elapsed())
                .unwrap_or(Duration::ZERO);
            wait = wait.min(rescan_rem);
        }
        ctx.request_repaint_after(wait);

        let loading = self.cache.window.len() < dc
            && !self.playback.play_order.is_empty()
            && self.settings.folder.is_some();
        if loading || acted {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.settings.folder.is_some() {
            self.save_now();
        }
    }
}
