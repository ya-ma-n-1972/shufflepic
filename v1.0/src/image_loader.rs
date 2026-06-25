//! 拡張子判定・デコード・寸法プローブ・VRAM 見積もり・巨大画像判定（v1.0 詳細 §4.1 / §4.8）

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 対応拡張子（小文字比較）
pub const SUPPORTED_EXTS: [&str; 6] = ["jpg", "jpeg", "png", "gif", "bmp", "webp"];

/// ワーカーがデコードして返す結果（CPU 上、GPU 未アップロード）。
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub color: egui::ColorImage,
    pub rgba_bytes: usize,
}

/// デコード失敗の区別（v1.0 詳細 §4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// open 時点で存在しない（失敗カウンタ非対象）。
    Missing,
    /// デコード失敗・寸法不正（失敗カウンタ対象）。
    Failed,
    /// 寸法上限超過（F-8 で退避対象、失敗カウンタ非対象）。
    Oversized { w: u32, h: u32 },
}

/// 外部差し替え検出用のファイル指紋（少なくとも長さと更新時刻）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFingerprint {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

/// 現在の metadata から指紋を取得（取得不能なら None）。
pub fn fingerprint(path: &Path) -> Option<FileFingerprint> {
    let m = std::fs::metadata(path).ok()?;
    Some(FileFingerprint {
        len: m.len(),
        modified: m.modified().ok(),
    })
}

/// 拡張子が対応形式かどうか（大文字小文字無関係）
pub fn is_supported(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let lower = ext.to_ascii_lowercase();
            SUPPORTED_EXTS.contains(&lower.as_str())
        }
        None => false,
    }
}

/// 指定ディレクトリ「直下」の対応画像ファイル一覧（サブフォルダは無視）。
/// `delete` / `oversized` サブフォルダは is_file() 判定で自動的に除外される。
pub fn scan_dir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_file() && is_supported(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// フルデコードせずにヘッダから寸法のみ取得（詳細設計 §2.7 / §8）。
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    image::image_dimensions(path).ok()
}

/// VRAM 見積もり値 `width * height * 4`（RGBA8、byte）。
pub fn vram_size(width: u32, height: u32) -> usize {
    (width as usize) * (height as usize) * 4
}

/// F-8: 寸法が上限を超えるか（フルデコードしない）。
pub fn is_oversized(width: u32, height: u32) -> bool {
    let pixels = (width as u64) * (height as u64);
    pixels > crate::OVERSIZED_MAX_PIXELS || width.max(height) > crate::OVERSIZED_MAX_SIDE
}

/// ワーカー本体: 「寸法取得 → 上限判定（F-8）→ デコード」の順。
pub fn decode_color(path: &Path) -> Result<DecodedImage, DecodeError> {
    // 寸法プローブ（ヘッダ読み）。失敗は存在有無で Missing/Failed を分ける。
    match dimensions(path) {
        Some((w, h)) => {
            if is_oversized(w, h) {
                return Err(DecodeError::Oversized { w, h });
            }
        }
        None => {
            return if path.exists() {
                Err(DecodeError::Failed)
            } else {
                Err(DecodeError::Missing)
            };
        }
    }

    let img = match image::open(path) {
        Ok(img) => img,
        Err(_) => {
            return if path.exists() {
                Err(DecodeError::Failed)
            } else {
                Err(DecodeError::Missing)
            };
        }
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let pixels = rgba.into_raw();
    let rgba_bytes = pixels.len();
    let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &pixels);
    Ok(DecodedImage {
        width: w,
        height: h,
        color,
        rgba_bytes,
    })
}
