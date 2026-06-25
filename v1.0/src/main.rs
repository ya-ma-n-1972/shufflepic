//! ShufflePic v1.0 — ランダム無重複の写真スライドショー（1〜2 枚並べて自動送り）。
//! 内部開発履歴では v3.0 に相当（公開バージョンは v1.0）。
//! F-1 デコード非同期化 / F-2 メニュー中の自動送り停止 / F-3 Undo 廃止 /
//! F-4 末尾到達のサイクル停止修復 / F-5 表示済み枚数 / F-6 右クリック再生・一時停止 /
//! F-7 管理画面＋永続化 / F-8 巨大画像の自動退避。

mod app;
mod cache;
mod decoder;
mod image_loader;
mod persist;
mod playback;
mod quarantine;
mod scanner;
mod settings;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

use app::ShufflePicApp;
use persist::PersistState;
use playback::PlaybackState;
use settings::SettingsState;

// ---- 間隔・表示枚数（F-7）----
pub const DEFAULT_INTERVAL_SECS: u64 = 15;
pub const MIN_INTERVAL_SECS: u64 = 5;
pub const MAX_INTERVAL_SECS: u64 = 60;
pub const DEFAULT_DISPLAY_COUNT: usize = 2;

// ---- 永続化（F-7）----
pub const STATE_FILE_NAME: &str = "shufflepic_state.json";
pub const SAVE_THROTTLE_SECS: u64 = 5;
pub const PERSIST_SCHEMA_VERSION: u32 = 1;

// ---- F-1 非同期デコード ----
pub const DECODE_WORKERS: usize = 0; // 0=自動（コア数-1 を 1..=4）
pub const MAX_INFLIGHT: usize = 6;
pub const MAX_READY_BUFFER: usize = 6;
pub const READAHEAD_DEPTH: usize = 8;
pub const MAX_RESULTS_PER_FRAME: usize = 4;
pub const MAX_UPLOADS_PER_FRAME: usize = 2;
pub const DISPLAY_QUEUE_CAPACITY: usize = 2;
pub const PREFETCH_QUEUE_CAPACITY: usize = 6;
pub const RESULT_QUEUE_CAPACITY: usize = 8;

// ---- F-8 巨大画像の退避 ----
pub const OVERSIZED_MAX_PIXELS: u64 = 32_000_000; // ≒ デコード後 128MB
pub const OVERSIZED_MAX_SIDE: u32 = 10_000;
pub const OVERSIZED_DIR_NAME: &str = "oversized";
pub const MAX_OVERSIZED_MOVE_ATTEMPTS: u32 = 5;

// ---- 据え置き ----
pub const VRAM_LIMIT: usize = 2 * 1024 * 1024 * 1024;
pub const RESCAN_INTERVAL_SECS: u64 = 10;
pub const MAX_DECODE_FAILS: u32 = 3;
pub const ERROR_MSG_SECS: u64 = 4;
pub const DELETE_DIR_NAME: &str = "delete";

fn setup_japanese_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let font_paths = [
        "C:\\Windows\\Fonts\\meiryo.ttc",
        "C:\\Windows\\Fonts\\YuGothM.ttc",
        "C:\\Windows\\Fonts\\msgothic.ttc",
    ];
    for font_path in &font_paths {
        if let Ok(font_data) = std::fs::read(font_path) {
            fonts.font_data.insert(
                "japanese_font".to_owned(),
                FontData::from_owned(font_data).into(),
            );
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "japanese_font".to_owned());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .insert(0, "japanese_font".to_owned());
            break;
        }
    }
    ctx.set_fonts(fonts);
}

/// 永続化状態から初期の設定・再生状態と保存先を組み立てる（同一フォルダなら続きから・F-7 §4.7.4）。
fn build_initial(
    restored: Option<(std::path::PathBuf, PersistState)>,
) -> (SettingsState, PlaybackState, Option<std::path::PathBuf>) {
    if let Some((path, state)) = restored {
        if state.folder.is_dir() {
            let scanned = image_loader::scan_dir(&state.folder);
            let (play_order, cursor, last_shown) = persist::reconcile(&state, &scanned);
            let settings = SettingsState::new(
                Some(state.folder.clone()),
                state.interval_secs,
                state.display_count,
            );
            let playback =
                PlaybackState::from_restored(play_order, cursor, state.cycle_count, last_shown);
            return (settings, playback, Some(path));
        }
    }
    (
        SettingsState::defaults(None),
        PlaybackState::new(Vec::new()),
        None,
    )
}

/// ウィンドウ／タスクバー用アイコン（共有アセットの 512px PNG を起動時に読み込む）。
fn app_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../../assets/shufflepic-icon-512.png");
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}

fn main() -> Result<(), eframe::Error> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 720.0])
        .with_title("ShufflePic");
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "ShufflePic",
        options,
        Box::new(|cc| {
            setup_japanese_fonts(&cc.egui_ctx);
            let decoder = decoder::DecoderPool::new(cc.egui_ctx.clone(), DECODE_WORKERS);
            let restored = persist::load();
            let (settings, playback, save_path) = build_initial(restored);
            Ok(Box::new(ShufflePicApp::new(
                settings, playback, decoder, save_path,
            )))
        }),
    )
}
