//! play_order と cursor の管理、シャッフル、サイクル境界用ヘルパ（v1.0 詳細 §3.3.1 / §4）

use rand::seq::SliceRandom;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

/// 書き込み完了確認待ちの新規検出ファイル（詳細設計 §3.1）
pub struct PendingFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

pub struct PlaybackState {
    /// シャッフルされた再生順。
    pub play_order: Vec<PathBuf>,
    /// 現在表示中の左画像（window[0]）に対応する play_order インデックス。
    pub cursor: usize,
    /// 新規検出されたが書き込み完了確認待ちのファイル。
    pub pending: Vec<PendingFile>,
    /// パス単位の連続デコード失敗カウンタ（§2.8）。
    pub fail_counts: HashMap<PathBuf, u32>,
    /// 当該サイクル数（表示用）。
    pub cycle_count: u32,
    /// 直近 advance で実際に表示していた最大2件（境界での連続再表示回避用・F-4）。
    pub last_shown: Vec<PathBuf>,
    /// 空フォルダ状態への初期化済みフラグ（揮発・F-4）。
    pub empty_state_active: bool,
}

impl PlaybackState {
    /// 起動時：パス一覧をシャッフルして初期化。
    pub fn new(mut paths: Vec<PathBuf>) -> Self {
        paths.shuffle(&mut rand::thread_rng());
        Self {
            play_order: paths,
            cursor: 0,
            pending: Vec::new(),
            fail_counts: HashMap::new(),
            cycle_count: 0,
            last_shown: Vec::new(),
            empty_state_active: false,
        }
    }

    /// 永続化からの復元用：play_order / cursor / cycle_count / last_shown を指定して構築。
    pub fn from_restored(
        play_order: Vec<PathBuf>,
        cursor: usize,
        cycle_count: u32,
        last_shown: Vec<PathBuf>,
    ) -> Self {
        Self {
            play_order,
            cursor,
            pending: Vec::new(),
            fail_counts: HashMap::new(),
            cycle_count,
            last_shown,
            empty_state_active: false,
        }
    }

    /// `play_order[from..]` のみを再シャッフルする（窓と対応する範囲は触らない）。
    pub fn reshuffle_tail(&mut self, from: usize) {
        if from >= self.play_order.len() {
            return;
        }
        self.play_order[from..].shuffle(&mut rand::thread_rng());
    }

    /// サイクル境界：全体を再シャッフルし、直前表示パスが新サイクル先頭に来ないよう担保する。
    pub fn reshuffle_all_avoiding(&mut self, last_shown: &[PathBuf]) {
        let n = self.play_order.len();
        self.play_order.shuffle(&mut rand::thread_rng());
        if n <= 2 {
            return;
        }
        let avoid_count = last_shown.len().min(2).min(n);
        for i in 0..avoid_count {
            if last_shown.contains(&self.play_order[i]) {
                if let Some(j) =
                    (avoid_count..n).find(|&j| !last_shown.contains(&self.play_order[j]))
                {
                    self.play_order.swap(i, j);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }
    fn paths(n: usize) -> Vec<PathBuf> {
        (0..n).map(|i| p(&format!("f{i}.png"))).collect()
    }
    fn sorted(mut v: Vec<PathBuf>) -> Vec<PathBuf> {
        v.sort();
        v
    }
    fn st(order: Vec<PathBuf>, cursor: usize) -> PlaybackState {
        PlaybackState::from_restored(order, cursor, 0, Vec::new())
    }

    #[test]
    fn new_preserves_set() {
        let v = paths(30);
        let s = PlaybackState::new(v.clone());
        assert_eq!(sorted(s.play_order), sorted(v));
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn reshuffle_tail_keeps_head_and_set() {
        let mut s = st(paths(10), 3);
        let head = s.play_order[..5].to_vec();
        s.reshuffle_tail(5);
        assert_eq!(&s.play_order[..5], &head[..], "head must be untouched");
        assert_eq!(sorted(s.play_order), sorted(paths(10)), "set preserved");
    }

    #[test]
    fn reshuffle_all_avoiding_keeps_set_and_avoids_front() {
        let mut s = st(paths(10), 0);
        let last = vec![p("f0.png"), p("f1.png")];
        for _ in 0..100 {
            s.reshuffle_all_avoiding(&last);
            assert!(!last.contains(&s.play_order[0]), "last_shown must not be at index 0");
            assert!(!last.contains(&s.play_order[1]), "last_shown must not be at index 1");
            assert_eq!(sorted(s.play_order.clone()), sorted(paths(10)));
        }
    }

    #[test]
    fn reshuffle_all_avoiding_small_set_is_tolerated() {
        let mut s = st(paths(2), 0);
        let last = vec![p("f0.png"), p("f1.png")];
        s.reshuffle_all_avoiding(&last);
        assert_eq!(sorted(s.play_order), sorted(paths(2)));
    }
}
