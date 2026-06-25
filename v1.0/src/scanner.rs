//! 起動時スキャンと定期 rescan（v1.0 詳細 §11 踏襲。候補列変更を bool で返す）

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::cache::CacheState;
use crate::image_loader;
use crate::playback::{PendingFile, PlaybackState};

pub struct ScannerState {
    pub last_rescan: Instant,
    pub interval: Duration,
}

impl ScannerState {
    pub fn new(interval: Duration) -> Self {
        Self {
            last_rescan: Instant::now(),
            interval,
        }
    }
}

fn stat(path: &Path) -> Option<(u64, SystemTime)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// 定期 rescan。読み取り専用。窓と未表示部分のみ対象。
/// 候補列（play_order）が変わったら true を返す（呼び出し側が epoch を更新する）。
pub fn rescan(
    pb: &mut PlaybackState,
    cache: &mut CacheState,
    input_dir: &Path,
    display_count: usize,
) -> bool {
    let scanned_vec = image_loader::scan_dir(input_dir);
    let scanned: HashSet<PathBuf> = scanned_vec.into_iter().collect();

    let mut known: HashSet<PathBuf> = pb.play_order.iter().cloned().collect();
    for pf in &pb.pending {
        known.insert(pf.path.clone());
    }

    let mut changed = false;

    // 1) 既存 pending の昇格判定（新規検出より前に）。
    let mut added = false;
    let mut still_pending: Vec<PendingFile> = Vec::new();
    for pf in std::mem::take(&mut pb.pending) {
        if let Some((size, modified)) = stat(&pf.path) {
            if size == pf.size && modified == pf.modified {
                pb.play_order.push(pf.path.clone());
                added = true;
            } else {
                still_pending.push(PendingFile {
                    path: pf.path,
                    size,
                    modified,
                });
            }
        }
    }
    pb.pending = still_pending;

    // 2) 新規 → pending（次回 rescan で昇格）。
    for p in &scanned {
        if !known.contains(p) {
            if let Some((size, modified)) = stat(p) {
                pb.pending.push(PendingFile {
                    path: p.clone(),
                    size,
                    modified,
                });
            }
        }
    }

    // 3) 外部消失の処理。表示中とみなす枚数は display_count に従う（1枚表示で2枚目を保護しない）。
    let displayed = cache.window.len().min(display_count.max(1));

    // 3a) 窓内「未表示」キャッシュが消失していたら、window と play_order から除去。
    let mut k = cache.window.len();
    while k > displayed {
        k -= 1;
        let p = cache.window[k].path.clone();
        if !scanned.contains(&p) {
            cache.evict_at(k);
            let idx = pb.cursor + k;
            if idx < pb.play_order.len() && pb.play_order[idx] == p {
                pb.play_order.remove(idx);
            } else if let Some(pos) = pb.play_order.iter().position(|x| *x == p) {
                pb.play_order.remove(pos);
            }
            cache.preload_blocked_by_vram = false;
            changed = true;
        }
    }

    // 3b) 窓より後ろ（play_order[cursor + window.len()..]）の外部消失を除去。
    let from = pb.cursor + cache.window.len();
    let mut i = from;
    while i < pb.play_order.len() {
        if !scanned.contains(&pb.play_order[i]) {
            pb.play_order.remove(i);
            changed = true;
        } else {
            i += 1;
        }
    }
    if changed {
        cache.preload_blocked_by_vram = false;
    }

    // pending からも消失分を除去。
    pb.pending.retain(|pf| scanned.contains(&pf.path));

    // 4) 新規追加があれば窓より後ろのみ再シャッフル。
    if added {
        pb.reshuffle_tail(pb.cursor + cache.window.len());
        cache.preload_blocked_by_vram = false;
        changed = true;
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheState, CachedImage};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn unique_temp(tag: &str) -> PathBuf {
        static C: AtomicU32 = AtomicU32::new(0);
        let n = C.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sp_s_{tag}_{}_{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_img(path: &Path) {
        image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))
            .save(path)
            .unwrap();
    }

    fn dummy_tex(ctx: &egui::Context, name: &str) -> egui::TextureHandle {
        let color = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
        ctx.load_texture(name, color, egui::TextureOptions::LINEAR)
    }

    /// 表示枚数1のとき、window[1]（未表示）の外部消失が rescan で除去される（#3 回帰）。
    #[test]
    fn rescan_one_image_mode_removes_vanished_second() {
        let dir = unique_temp("one");
        let a = dir.join("a.png");
        let b = dir.join("b.png");
        write_img(&a);
        write_img(&b);

        let ctx = egui::Context::default();
        let mut cache = CacheState::new();
        cache.window.push_back(CachedImage {
            path: a.clone(),
            texture: dummy_tex(&ctx, "a"),
            width: 1.0,
            height: 1.0,
            vram_size: 4,
        });
        cache.window.push_back(CachedImage {
            path: b.clone(),
            texture: dummy_tex(&ctx, "b"),
            width: 1.0,
            height: 1.0,
            vram_size: 4,
        });
        let mut pb = PlaybackState::from_restored(vec![a.clone(), b.clone()], 0, 0, Vec::new());

        // b を外部削除。表示枚数1なら window[1]=b は「未表示」として除去対象。
        std::fs::remove_file(&b).unwrap();
        let changed = rescan(&mut pb, &mut cache, &dir, 1);

        assert!(changed);
        assert!(!pb.play_order.contains(&b), "vanished second image removed from play_order");
        assert_eq!(cache.window.len(), 1, "window[1] evicted");
        assert_eq!(cache.window[0].path, a);

        std::fs::remove_dir_all(&dir).ok();
    }
}
