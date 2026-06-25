//! VRAM 窓と非同期補充（v1.0 詳細 §4.1 / §4.4 / §4.7.5 / §4.8）。
//! 補充は「回収 → 配置 → 要求」の非ブロック処理。デコードはワーカー（decoder.rs）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use egui::TextureHandle;

use crate::decoder::{DecodeOutcome, DecodeRequest, DecoderPool};
use crate::image_loader::{self, DecodedImage};
use crate::playback::PlaybackState;
use crate::{
    MAX_DECODE_FAILS, MAX_INFLIGHT, MAX_READY_BUFFER, MAX_RESULTS_PER_FRAME, MAX_UPLOADS_PER_FRAME,
    READAHEAD_DEPTH, VRAM_LIMIT,
};

pub struct CachedImage {
    pub path: PathBuf,
    pub texture: TextureHandle,
    pub width: f32,
    pub height: f32,
    pub vram_size: usize,
}

pub struct CacheState {
    /// VRAM 窓。先頭が現在表示中（または次に表示する）画像。
    pub window: VecDeque<CachedImage>,
    pub current_vram: usize,
    pub pending_free: Vec<TextureHandle>,
    pub preload_blocked_by_vram: bool,

    // ---- F-1 非同期デコード ----
    pub ready: HashMap<PathBuf, DecodedImage>,
    pub inflight: HashSet<PathBuf>,
    pub ready_bytes: usize,
    pub epoch: u64,
}

/// settle の遷移種別。
#[derive(Debug, PartialEq, Eq)]
pub enum Settle {
    None,
    CycleBoundary,
    Empty,
}

impl CacheState {
    pub fn new() -> Self {
        Self {
            window: VecDeque::new(),
            current_vram: 0,
            pending_free: Vec::new(),
            preload_blocked_by_vram: false,
            ready: HashMap::new(),
            inflight: HashSet::new(),
            ready_bytes: 0,
            epoch: 0,
        }
    }

    pub fn evict_front(&mut self) -> Option<PathBuf> {
        let ci = self.window.pop_front()?;
        self.current_vram = self.current_vram.saturating_sub(ci.vram_size);
        let path = ci.path.clone();
        self.pending_free.push(ci.texture);
        Some(path)
    }

    pub fn evict_at(&mut self, index: usize) {
        if let Some(ci) = self.window.remove(index) {
            self.current_vram = self.current_vram.saturating_sub(ci.vram_size);
            self.pending_free.push(ci.texture);
        }
    }

    /// 候補列世代を進め、ready/inflight をクリアして現世代の要求を作り直す（v1.0 詳細 §3.4）。
    /// 旧世代の到着結果は epoch 不一致で破棄される。
    pub fn bump_epoch(&mut self, decoder: &DecoderPool) {
        self.epoch = self.epoch.wrapping_add(1);
        self.ready.clear();
        self.inflight.clear();
        self.ready_bytes = 0;
        decoder.set_epoch(self.epoch);
    }
}

impl Default for CacheState {
    fn default() -> Self {
        Self::new()
    }
}

/// play_order から path を1件除去（位置探索）。除去できたら true。
fn remove_path(pb: &mut PlaybackState, path: &std::path::Path) -> bool {
    if let Some(pos) = pb.play_order.iter().position(|p| p == path) {
        pb.play_order.remove(pos);
        true
    } else {
        false
    }
}

/// デコード失敗の統一処理（v1.0 詳細 §12.2 準拠）。cursor は進めない。
fn handle_decode_fail(pb: &mut PlaybackState, path: &std::path::Path) {
    let counter = pb.fail_counts.entry(path.to_path_buf()).or_insert(0);
    *counter += 1;
    let count = *counter;
    remove_path(pb, path);
    if count < MAX_DECODE_FAILS {
        pb.play_order.push(path.to_path_buf());
    }
}

/// 補充本体：回収 → 配置 → 要求（v1.0 詳細 §4.1 / F-1 提案 §6.1）。
/// `oversized_out` に検出した巨大画像パスを積む（実際の退避は app 側が行う・§4.8）。
/// 補充本体。候補列（play_order）が変わったら true を返す（呼び出し側が永続化 dirty 化）。
pub fn refill_window(
    pb: &mut PlaybackState,
    cache: &mut CacheState,
    decoder: &DecoderPool,
    ctx: &egui::Context,
    display_count: usize,
    oversized_out: &mut Vec<PathBuf>,
) -> bool {
    let mut candidate_changed = false;

    // 1. 回収
    let mut got = 0;
    while got < MAX_RESULTS_PER_FRAME {
        let res = match decoder.try_recv() {
            Some(r) => r,
            None => break,
        };
        got += 1;

        // 旧世代の結果は、現世代の inflight を解除せずに破棄する
        // （inflight は bump_epoch でクリア済み。現世代の同一パス要求を未要求扱いにしない）。
        if res.epoch != cache.epoch {
            continue;
        }
        // 現世代の結果のみ inflight を解除する。
        cache.inflight.remove(&res.path);

        // 外部差し替え検出：要求時と現在の指紋が違えば破棄して再要求に委ねる。
        let current_fp = image_loader::fingerprint(&res.path);
        if res.fingerprint != current_fp {
            continue;
        }

        match res.outcome {
            DecodeOutcome::Ok(img) => {
                let bytes = img.rgba_bytes;
                // 同一パスの二重格納で ready_bytes が二重加算されないようにする。
                if let Some(old) = cache.ready.insert(res.path.clone(), img) {
                    cache.ready_bytes = cache.ready_bytes.saturating_sub(old.rgba_bytes);
                }
                cache.ready_bytes += bytes;
                pb.fail_counts.remove(&res.path);
            }
            DecodeOutcome::Failed => {
                handle_decode_fail(pb, &res.path);
                candidate_changed = true;
            }
            DecodeOutcome::Missing => {
                remove_path(pb, &res.path);
                candidate_changed = true;
            }
            DecodeOutcome::Oversized { .. } => {
                cache.ready.remove(&res.path);
                remove_path(pb, &res.path);
                oversized_out.push(res.path.clone());
                candidate_changed = true;
            }
        }
    }

    // 2. 配置（ready から窓へ連続ミラーで）
    let mut uploads = 0usize;
    loop {
        if pb.play_order.is_empty() {
            break;
        }
        let idx = pb.cursor + cache.window.len();
        if idx >= pb.play_order.len() {
            break;
        }
        let path = pb.play_order[idx].clone();
        let decoded = match cache.ready.get(&path) {
            Some(d) => d,
            None => break, // 連続性のため、次の表示位置が未到着なら止める
        };
        let vram = image_loader::vram_size(decoded.width, decoded.height);
        let is_display_slot = cache.window.len() < display_count;
        if !is_display_slot {
            if cache.current_vram + vram > VRAM_LIMIT {
                cache.preload_blocked_by_vram = true;
                break;
            }
            if uploads >= MAX_UPLOADS_PER_FRAME {
                break;
            }
        }
        let decoded = cache.ready.remove(&path).unwrap();
        cache.ready_bytes = cache.ready_bytes.saturating_sub(decoded.rgba_bytes);
        let texture = ctx.load_texture(
            path.to_string_lossy().to_string(),
            decoded.color,
            egui::TextureOptions::LINEAR,
        );
        cache.window.push_back(CachedImage {
            path: path.clone(),
            texture,
            width: decoded.width as f32,
            height: decoded.height as f32,
            vram_size: vram,
        });
        cache.current_vram += vram;
        if !is_display_slot {
            uploads += 1;
        }
    }

    // 3. 候補列が変わったら世代を進める（先読みを作り直す）
    if candidate_changed {
        cache.bump_epoch(decoder);
        cache.preload_blocked_by_vram = false;
    }

    // 4. 要求発行（表示枠は高優先、その先は先読み）
    if pb.play_order.is_empty() {
        return candidate_changed;
    }
    let wlen = cache.window.len();
    let max_pos = READAHEAD_DEPTH.max(display_count);
    for pos in wlen..max_pos {
        let idx = pb.cursor + pos;
        if idx >= pb.play_order.len() {
            break;
        }
        let path = pb.play_order[idx].clone();
        if cache.ready.contains_key(&path) || cache.inflight.contains(&path) {
            continue;
        }
        let is_display = pos < display_count;
        if !is_display {
            if cache.inflight.len() >= MAX_INFLIGHT {
                break;
            }
            if cache.ready.len() >= MAX_READY_BUFFER {
                break;
            }
            if cache.preload_blocked_by_vram {
                break;
            }
        }
        let req = DecodeRequest {
            path: path.clone(),
            epoch: cache.epoch,
            fingerprint: image_loader::fingerprint(&path),
        };
        let sent = if is_display {
            decoder.request_display(req)
        } else {
            decoder.request_prefetch(req)
        };
        if sent.is_ok() {
            cache.inflight.insert(path);
        } else if is_display {
            // 高優先キュー満杯：次フレームで再投入。
            break;
        }
    }

    candidate_changed
}

/// 末尾到達・空状態への共通遷移（F-4。v1.0 詳細 §4.4）。
pub fn settle_cycle_or_empty(
    pb: &mut PlaybackState,
    cache: &mut CacheState,
    decoder: &DecoderPool,
) -> Settle {
    if pb.play_order.is_empty() {
        if pb.empty_state_active {
            return Settle::None; // 空状態継続。再初期化・epoch 更新を繰り返さない。
        }
        while cache.evict_front().is_some() {}
        cache.current_vram = 0;
        cache.preload_blocked_by_vram = false;
        pb.cursor = 0;
        pb.empty_state_active = true;
        cache.bump_epoch(decoder);
        return Settle::Empty;
    }
    pb.empty_state_active = false;

    if cache.window.is_empty() && pb.cursor >= pb.play_order.len() {
        // サイクル境界（v2 詳細 §7.3 と同手順。window は既に空）。
        cache.current_vram = 0;
        cache.preload_blocked_by_vram = false;
        pb.cursor = 0;
        let last_shown = pb.last_shown.clone();
        pb.reshuffle_all_avoiding(&last_shown);
        pb.cycle_count = pb.cycle_count.wrapping_add(1);
        cache.bump_epoch(decoder);
        return Settle::CycleBoundary;
    }
    Settle::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::DecoderPool;
    use crate::playback::PlaybackState;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn unique_temp(tag: &str) -> PathBuf {
        static C: AtomicU32 = AtomicU32::new(0);
        let n = C.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sp_c_{tag}_{}_{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_img(path: &std::path::Path, w: u32, h: u32) {
        image::RgbaImage::from_pixel(w, h, image::Rgba([7, 8, 9, 255]))
            .save(path)
            .unwrap();
    }

    #[test]
    fn settle_cycle_boundary_on_tail() {
        let ctx = egui::Context::default();
        let decoder = DecoderPool::new(ctx, 1);
        let mut pb = PlaybackState::from_restored(vec![p("a"), p("b"), p("c")], 3, 0, Vec::new());
        let mut cache = CacheState::new();
        let s = settle_cycle_or_empty(&mut pb, &mut cache, &decoder);
        assert_eq!(s, Settle::CycleBoundary);
        assert_eq!(pb.cursor, 0);
        assert_eq!(pb.cycle_count, 1);
        assert_eq!(pb.play_order.len(), 3, "set preserved on boundary");
    }

    #[test]
    fn settle_empty_runs_once() {
        let ctx = egui::Context::default();
        let decoder = DecoderPool::new(ctx, 1);
        let mut pb = PlaybackState::from_restored(Vec::new(), 0, 0, Vec::new());
        let mut cache = CacheState::new();
        assert_eq!(
            settle_cycle_or_empty(&mut pb, &mut cache, &decoder),
            Settle::Empty
        );
        assert!(pb.empty_state_active);
        assert_eq!(
            settle_cycle_or_empty(&mut pb, &mut cache, &decoder),
            Settle::None,
            "空状態継続フレームは再処理しない"
        );
    }

    #[test]
    fn refill_fills_window_async_contiguously() {
        let base = unique_temp("fill");
        let mut order = Vec::new();
        for i in 0..4 {
            let p = base.join(format!("v{i}.png"));
            write_img(&p, 16, 16);
            order.push(p);
        }
        let mut pb = PlaybackState::from_restored(order.clone(), 0, 0, Vec::new());
        let mut cache = CacheState::new();
        let ctx = egui::Context::default();
        let decoder = DecoderPool::new(ctx.clone(), 2);
        let mut ov = Vec::new();

        let mut filled = false;
        for _ in 0..400 {
            refill_window(&mut pb, &mut cache, &decoder, &ctx, 2, &mut ov);
            if cache.window.len() >= 2 {
                filled = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(filled, "window should fill via async decode");
        for i in 0..cache.window.len() {
            assert_eq!(
                cache.window[i].path,
                pb.play_order[pb.cursor + i],
                "window is a contiguous mirror"
            );
        }
        assert!(ov.is_empty(), "small images are not oversized");

        drop(decoder);
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn refill_detects_oversized() {
        let base = unique_temp("over");
        // 上限超過の画像（長辺 > OVERSIZED_MAX_SIDE）。
        let big = base.join("big.png");
        let side = crate::OVERSIZED_MAX_SIDE + 2;
        write_img(&big, side, 1);
        let mut pb = PlaybackState::from_restored(vec![big.clone()], 0, 0, Vec::new());
        let mut cache = CacheState::new();
        let ctx = egui::Context::default();
        let decoder = DecoderPool::new(ctx.clone(), 1);
        let mut ov = Vec::new();

        let mut found = false;
        for _ in 0..400 {
            refill_window(&mut pb, &mut cache, &decoder, &ctx, 2, &mut ov);
            if !ov.is_empty() {
                found = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(found, "oversized image must be reported");
        assert_eq!(ov[0], big);
        assert!(!pb.play_order.contains(&big), "oversized removed from play_order");

        drop(decoder);
        fs::remove_dir_all(&base).ok();
    }
}
