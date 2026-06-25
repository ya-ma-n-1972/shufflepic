//! 設定値と管理画面（F-7。v1.0 詳細 §3.1 / §4.7）。

use std::path::PathBuf;

use crate::{
    DEFAULT_DISPLAY_COUNT, DEFAULT_INTERVAL_SECS, MAX_INTERVAL_SECS, MIN_INTERVAL_SECS,
};

pub struct SettingsState {
    /// 現在の入力フォルダ（未選択起動を表現するため Option）。
    pub folder: Option<PathBuf>,
    /// advance 間隔（5..=60 にクランプ）。
    pub interval_secs: u64,
    /// 表示枚数（1 または 2）。
    pub display_count: usize,
    /// 管理画面（設定パネル）を開いているか。
    pub show_panel: bool,
}

fn clamp_interval(s: u64) -> u64 {
    s.clamp(MIN_INTERVAL_SECS, MAX_INTERVAL_SECS)
}

fn clamp_count(c: usize) -> usize {
    if c <= 1 {
        1
    } else {
        2
    }
}

impl SettingsState {
    pub fn new(folder: Option<PathBuf>, interval_secs: u64, display_count: usize) -> Self {
        Self {
            folder,
            interval_secs: clamp_interval(interval_secs),
            display_count: clamp_count(display_count),
            show_panel: false,
        }
    }

    pub fn defaults(folder: Option<PathBuf>) -> Self {
        Self::new(folder, DEFAULT_INTERVAL_SECS, DEFAULT_DISPLAY_COUNT)
    }
}

/// 管理画面の操作結果。
pub struct SettingsPanelResult {
    /// 「参照...」で新しいフォルダが選ばれた（フォルダ変更＝全リセット契機）。
    pub folder_chosen: Option<PathBuf>,
    /// 間隔・枚数が変更された（即時反映・サイクル状態は維持・節流保存対象）。
    pub changed: bool,
}

/// 設定パネルを描画し、操作結果を返す（F-7）。
pub fn draw_panel(settings: &mut SettingsState, ctx: &egui::Context) -> SettingsPanelResult {
    let mut folder_chosen: Option<PathBuf> = None;
    let mut changed = false;
    let mut open = settings.show_panel;

    egui::Window::new("⚙ 設定")
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("画像フォルダ:");
                let cur = settings
                    .folder
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(未選択)".to_string());
                ui.label(cur);
            });
            if ui.button("参照...").clicked() {
                let mut dlg = rfd::FileDialog::new();
                if let Some(f) = &settings.folder {
                    dlg = dlg.set_directory(f);
                }
                if let Some(dir) = dlg.pick_folder() {
                    folder_chosen = Some(dir);
                }
            }

            ui.separator();

            ui.horizontal(|ui| {
                ui.label("切替間隔（秒）:");
                let mut secs = settings.interval_secs;
                let resp =
                    ui.add(egui::Slider::new(&mut secs, MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS));
                if resp.changed() {
                    settings.interval_secs = clamp_interval(secs);
                    changed = true;
                }
            });

            ui.horizontal(|ui| {
                ui.label("表示枚数:");
                if ui
                    .radio_value(&mut settings.display_count, 1, "1 枚")
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .radio_value(&mut settings.display_count, 2, "2 枚")
                    .changed()
                {
                    changed = true;
                }
            });
        });

    settings.show_panel = open;
    SettingsPanelResult {
        folder_chosen,
        changed,
    }
}
